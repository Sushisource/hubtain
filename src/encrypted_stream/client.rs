use super::*;
use crate::models::ClientId;
use anyhow::{anyhow, Error};
use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use std::{
    io,
    io::{Cursor, Write},
    pin::Pin,
    task::Poll,
};
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

pub struct ClientEncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    secret: EphemeralSecret,
}

impl<'a, S> ClientEncryptedStreamStarter<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S, secret: EphemeralSecret) -> Self {
        ClientEncryptedStreamStarter {
            underlying: Pin::new(underlying_stream),
            secret,
        }
    }

    /// Performs DH key exchange with the other side of the stream. Returns a new version
    /// of the stream that can be read/write from, transparently encrypting/decrypting the
    /// data.
    pub async fn key_exchange(mut self) -> Result<EncryptedReadStream<'a, S>, EncStreamErr> {
        let pubkey = PublicKey::from(&self.secret);
        info!("Your client id is: {}", ClientId::new(*pubkey.as_bytes()));
        let outgoing_hs = Handshake {
            pubkey: *pubkey.as_bytes(),
        };
        // Exchange public keys, first send ours
        let send_hs = bincode::serialize(&outgoing_hs)?;
        self.underlying.write_all(send_hs.as_slice()).await?;

        // Read incoming pubkey
        let read_size = bincode::serialized_size(&outgoing_hs)?;
        let mut buff = vec![0; read_size as usize];
        self.underlying.read_exact(&mut buff).await?;
        let read_hs: Handshake = bincode::deserialize_from(buff.as_slice())?;

        let their_pubkey: PublicKey = read_hs.pubkey.into();

        // Client reads the accepted/rejected byte
        let mut buff = vec![0; 1];
        info!("Awaiting approval from server...");
        self.underlying.read_exact(&mut buff).await?;
        // Give up if we were rejected
        if buff[0] != 1 {
            return Err(EncStreamErr::ClientNotAccepted);
        }

        // Compute shared secret
        let shared_secret = self.secret.diffie_hellman(&their_pubkey);
        EncryptedReadStream::new(self.underlying, shared_secret)
    }
}

pub struct EncryptedReadStream<'a, S: AsyncRead> {
    underlying: Pin<&'a mut S>,
    key: OpeningKey<SharedSecretNonceSeq>,
    read_remainder: Vec<u8>,
    unwritten: Vec<Vec<u8>>,
}

impl<'a, S> EncryptedReadStream<'a, S>
where
    S: AsyncRead,
{
    fn new(underlying: Pin<&'a mut S>, shared_secret: SharedSecret) -> Result<Self, EncStreamErr> {
        let unbound_k = UnboundKey::new(&CHACHA20_POLY1305, shared_secret.as_bytes())
            .map_err(|_| anyhow!("Couldn't bind key"))?;
        let derived_nonce_seq = SharedSecretNonceSeq::new(shared_secret);
        // key used to encrypt data
        let key = OpeningKey::new(unbound_k, derived_nonce_seq);

        Ok(Self {
            underlying,
            key,
            read_remainder: vec![],
            unwritten: vec![],
        })
    }
}

impl<'a, S> AsyncRead for EncryptedReadStream<'a, S>
where
    S: AsyncRead + AsyncWrite,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        mut buf: &mut [u8],
    ) -> Poll<Result<usize, io::Error>> {
        let mut written_from_unwritten = 0;
        while let Some(unwritten) = self.unwritten.pop() {
            debug!("Writing reserved packet");
            Write::write_all(&mut buf, &unwritten).expect("Must work");
            written_from_unwritten += unwritten.len();
        }

        let mut read_buf = vec![0; buf.len()];
        let read = self.underlying.as_mut().poll_read(cx, &mut read_buf);

        if let Poll::Ready(Ok(bytes_read)) = &read {
            if *bytes_read == 0 {
                if self.read_remainder.is_empty() {
                    return Poll::Ready(Ok(written_from_unwritten));
                }
                panic!("Should be unreachable");
            }
            let mut remainder_plus_read =
                [self.read_remainder.as_slice(), read_buf.as_slice()].concat();
            // Drop portion of the buffer which is just useless zero padding, if any.
            let useless_buffer_bytes = read_buf.len() - *bytes_read;
            remainder_plus_read.truncate(remainder_plus_read.len() - useless_buffer_bytes);
            let written = self
                .read_packets_from_buffer(&mut remainder_plus_read, buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if written == 0 {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            return Poll::Ready(Ok(written));
        };
        read
    }
}

impl<'a, S> EncryptedReadStream<'a, S>
where
    S: AsyncRead + AsyncWrite,
{
    fn read_packets_from_buffer(
        mut self: Pin<&mut Self>,
        remainder_plus_read: &mut Vec<u8>,
        write_into: &mut [u8],
    ) -> Result<usize, Error> {
        let mut read_cursor = Cursor::new(&remainder_plus_read);
        let mut write_cursor = Cursor::new(write_into);
        let mut total_written = 0;
        let mut successfull_buffer_advance = 0;

        loop {
            let deser_res: Result<EncryptedPacket, _> = bincode::deserialize_from(&mut read_cursor);
            let cursor_pos = read_cursor.position();

            match deser_res {
                Ok(packet) => {
                    successfull_buffer_advance = cursor_pos;
                    let mut decrypt_buff = packet.data;
                    if decrypt_buff.is_empty() {
                        debug!("Buffer empty, exiting");
                        break;
                    }

                    let just_content = self
                        .key
                        .open_in_place(Aad::empty(), &mut decrypt_buff)
                        .map_err(|_| anyhow!("Unspecified error decrypting data"))?;
                    match Write::write_all(&mut write_cursor, &just_content) {
                        Ok(_) => {
                            total_written += just_content.len();
                        }
                        Err(e) => {
                            // TODO: How to avoid? Reducing size of buffer that the underlying
                            //   poll reads into helped, but didn't totally eliminate. May simply
                            //   not be able to, as it looks like writing three packets in a row
                            //   is too much, but it only seems to happen at the end.
                            debug!("Couldn't write all decrypted data into buffer: {}", e);
                            self.unwritten.push(just_content.to_vec());
                        }
                    }
                }
                Err(e) => match *e {
                    // When deserialization fails because of unexpected eof, we have a partial
                    // packet, so the goal is to store it in the remainder and exit
                    bincode::ErrorKind::Io(e) => {
                        debug!("Couldn't deserialize whole packet: {}", e);
                        break;
                    }
                    e => panic!(e),
                },
            };
        }

        // Drop however much of the buffer we *did* read
        self.read_remainder = remainder_plus_read.split_off(successfull_buffer_advance as usize);

        Ok(total_written)
    }
}
