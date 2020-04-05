use crate::{
    client_approver::{ClientApprover, CONSOLE_APPROVER},
    server::ClientApprovalStrategy,
    LOG,
};
use anyhow::anyhow;
use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use serde::{Deserialize, Serialize};
use std::{
    cmp::min,
    io,
    io::{Cursor, Write},
    pin::Pin,
    task::Poll,
};
use thiserror::Error as DError;
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

pub struct ServerEncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    secret: EphemeralSecret,
}

pub struct ClientEncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    secret: EphemeralSecret,
}

#[derive(Serialize, Deserialize, Debug)]
struct Handshake {
    pkey: [u8; 32],
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
    pub async fn key_exchange(mut self) -> Result<EncryptedStream<'a, S>, EncStreamErr> {
        let pubkey = PublicKey::from(&self.secret);
        let outgoing_hs = Handshake {
            pkey: *pubkey.as_bytes(),
        };
        // Exchange public keys, first send ours
        let send_hs = bincode::serialize(&outgoing_hs)?;
        self.underlying.write_all(send_hs.as_slice()).await?;

        // Read incoming pubkey
        let read_size = bincode::serialized_size(&outgoing_hs)?;
        let mut buff = vec![0; read_size as usize];
        self.underlying.read_exact(&mut buff).await?;
        let read_hs: Handshake = bincode::deserialize_from(buff.as_slice())?;

        let their_pubkey: PublicKey = read_hs.pkey.into();

        // Client reads the accepted/rejected byte
        let mut buff = vec![0; 1];
        self.underlying.read_exact(&mut buff).await?;
        // Give up if we were rejected
        if buff[0] != 1 {
            return Err(EncStreamErr::ClientNotAccepted);
        }

        // Compute shared secret
        let shared_secret = self.secret.diffie_hellman(&their_pubkey);

        // TODO: Agree on nonce sequence here?

        EncryptedStream::new(self.underlying, shared_secret)
    }
}

impl<'a, S> ServerEncryptedStreamStarter<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S, secret: EphemeralSecret) -> Self {
        ServerEncryptedStreamStarter {
            underlying: Pin::new(underlying_stream),
            secret,
        }
    }

    /// Performs DH key exchange with the other side of the stream. Returns a new version
    /// of the stream that can be read/write from, transparently encrypting/decrypting the
    /// data.
    pub async fn key_exchange(
        mut self,
        // TODO: Param doesn't make sense for a client using an encrypted stream, since obviously
        //   they want to download from the server. Fix with builder or something.
        //   Also make this a ClientApprover
        approval_strat: ClientApprovalStrategy,
    ) -> Result<EncryptedStream<'a, S>, EncStreamErr> {
        let pubkey = PublicKey::from(&self.secret);
        let outgoing_hs = Handshake {
            pkey: *pubkey.as_bytes(),
        };
        // Exchange public keys, first send ours
        let send_hs = bincode::serialize(&outgoing_hs)?;
        self.underlying.write_all(send_hs.as_slice()).await?;

        // Read incoming pubkey
        let read_size = bincode::serialized_size(&outgoing_hs)?;
        let mut buff = vec![0; read_size as usize];
        self.underlying.read_exact(&mut buff).await?;
        let read_hs: Handshake = bincode::deserialize_from(buff.as_slice())?;

        let their_pubkey: PublicKey = read_hs.pkey.into();

        match approval_strat {
            #[cfg(test)]
            ClientApprovalStrategy::ApproveAll => {
                // Send approval byte
                self.underlying.write_all(&[1]).await?;
            }
            ClientApprovalStrategy::Interactive => {
                let approved = (&*CONSOLE_APPROVER).submit(their_pubkey.as_bytes()).await?;
                // Server sends signal to client if it is not approved for graceful hangup
                let approval_byte = if approved { 1 } else { 0 };
                self.underlying.write_all(&[approval_byte]).await?;

                if !approved {
                    return Err(EncStreamErr::ClientNotAccepted);
                }
            }
        };

        // Compute shared secret
        let shared_secret = self.secret.diffie_hellman(&their_pubkey);

        // TODO: Agree on nonce sequence here?

        EncryptedStream::new(self.underlying, shared_secret)
    }
}

pub struct EncryptedStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    key: LessSafeKey,
    read_remainder: Vec<u8>,
    unwritten: Vec<Vec<u8>>,
}

impl<'a, S> EncryptedStream<'a, S>
where
    S: AsyncWrite + AsyncRead,
{
    fn new(underlying: Pin<&'a mut S>, shared_secret: SharedSecret) -> Result<Self, EncStreamErr> {
        let unbound_k = UnboundKey::new(&CHACHA20_POLY1305, shared_secret.as_bytes())
            .map_err(|_| anyhow!("Couldn't bind key"))?;
        // key used to encrypt data
        let key = LessSafeKey::new(unbound_k);

        Ok(Self {
            underlying,
            key,
            read_remainder: vec![],
            unwritten: vec![],
        })
    }
}

const CHUNK_SIZE: usize = 5012;

#[derive(Serialize, Deserialize)]
struct EncryptedPacket {
    data: Vec<u8>,
}

impl<'a, S> AsyncWrite for EncryptedStream<'a, S>
where
    S: AsyncWrite + AsyncRead,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        // TODO: have both sides agree on nonce sequence somehow?
        let nonce = Nonce::assume_unique_for_key([0; 12]);

        let siz = min(buf.len(), CHUNK_SIZE);
        let mut encrypt_me = vec![0; siz];
        encrypt_me.copy_from_slice(&buf[0..siz]);
        let bufsiz = encrypt_me.len();
        self.key
            .seal_in_place_append_tag(nonce, Aad::empty(), &mut encrypt_me)
            .unwrap();

        let packet = EncryptedPacket { data: encrypt_me };
        let bincoded = bincode::serialize(&packet).unwrap();
        self.underlying
            .as_mut()
            .poll_write(cx, bincoded.as_slice())
            .map(|r| match r {
                Ok(_) => Ok(bufsiz),
                o => o,
            })
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_close(cx)
    }
}

impl<'a, S> AsyncRead for EncryptedStream<'a, S>
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
            debug!(LOG, "Writing reserved packet");
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
            remainder_plus_read.split_off(remainder_plus_read.len() - useless_buffer_bytes);
            let written = self.read_packets_from_buffer(&mut remainder_plus_read, buf);
            if written == 0 {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            return Poll::Ready(Ok(written));
        };
        read
    }
}

impl<'a, S> EncryptedStream<'a, S>
where
    S: AsyncRead + AsyncWrite,
{
    fn read_packets_from_buffer(
        mut self: Pin<&mut Self>,
        remainder_plus_read: &mut Vec<u8>,
        write_into: &mut [u8],
    ) -> usize {
        let mut read_cursor = Cursor::new(&remainder_plus_read);
        let mut write_cursor = Cursor::new(write_into);
        let mut total_written = 0;
        let mut successfull_buffer_advance = 0;

        loop {
            // TODO: doesn't belong here
            let nonce = Nonce::assume_unique_for_key([0; 12]);

            let deser_res: Result<EncryptedPacket, _> = bincode::deserialize_from(&mut read_cursor);
            let cursor_pos = read_cursor.position();

            match deser_res {
                Ok(packet) => {
                    successfull_buffer_advance = cursor_pos;
                    let mut decrypt_buff = packet.data;
                    if decrypt_buff.is_empty() {
                        debug!(LOG, "Buffer empty, exiting");
                        break;
                    }

                    let just_content = self
                        .key
                        .open_in_place(nonce, Aad::empty(), &mut decrypt_buff)
                        .unwrap();
                    match Write::write_all(&mut write_cursor, &just_content) {
                        Ok(_) => {
                            total_written += just_content.len();
                        }
                        Err(e) => {
                            // TODO: How to avoid? Reducing size of buffer that the underlying
                            //   poll reads into helped, but didn't totally eliminate. May simply
                            //   not be able to, as it looks like writing three packets in a row
                            //   is too much, but it only seems to happen at the end.
                            debug!(LOG, "Couldn't write all decrypted data into buffer: {}", e);
                            self.unwritten.push(just_content.to_vec());
                        }
                    }
                }
                Err(e) => match *e {
                    // When deserialization fails because of unexpected eof, we have a partial
                    // packet, so the goal is to store it in the remainder and exit
                    bincode::ErrorKind::Io(e) => {
                        debug!(LOG, "Couldn't deserialize whole packet: {}", e);
                        break;
                    }
                    e => panic!(e),
                },
            };
        }

        // Drop however much of the buffer we *did* read
        self.read_remainder = remainder_plus_read.split_off(successfull_buffer_advance as usize);

        total_written
    }
}

#[derive(Debug, DError)]
pub enum EncStreamErr {
    #[error("Client rejected by server")]
    ClientNotAccepted,
    #[error("Bincode serialization error")]
    BincodeErr {
        #[from]
        source: Box<bincode::ErrorKind>,
    },
    #[error("I/O error")]
    IOErr {
        #[from]
        source: std::io::Error,
    },
    #[error("Other error")]
    Other {
        #[from]
        source: anyhow::Error,
    },
}
