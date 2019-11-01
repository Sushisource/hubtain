use anyhow::{anyhow, Error};
use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use serde::{Deserialize, Serialize};
use std::cmp::min;
use std::io::{Cursor, Write};
use std::{io, pin::Pin, task::Poll};
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

pub struct EncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    secret: EphemeralSecret,
}

#[derive(Serialize, Deserialize, Debug)]
struct Handshake {
    pkey: [u8; 32],
}

impl<'a, S> EncryptedStreamStarter<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S, secret: EphemeralSecret) -> Self {
        EncryptedStreamStarter {
            underlying: Pin::new(underlying_stream),
            secret,
        }
    }

    /// Performs DH key exchange with the other side of the stream. Returns a new version
    /// of the stream that can be read/write from, transparently encrypting/decrypting the
    /// data.
    pub async fn key_exchange(mut self) -> Result<EncryptedStream<'a, S>, Error> {
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

        // Compute shared secret
        let shared_secret = self.secret.diffie_hellman(&read_hs.pkey.into());

        // TODO: Agree on nonce sequence here?

        EncryptedStream::new(self.underlying, shared_secret)
    }
}

pub struct EncryptedStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    key: LessSafeKey,
    read_remainder: Vec<u8>,
    unwritten: Vec<Vec<u8>>
}

impl<'a, S> EncryptedStream<'a, S>
where
    S: AsyncWrite + AsyncRead,
{
    fn new(underlying: Pin<&'a mut S>, shared_secret: SharedSecret) -> Result<Self, Error> {
        let unbound_k = UnboundKey::new(&CHACHA20_POLY1305, shared_secret.as_bytes())
            .map_err(|_| anyhow!("Couldn't bind key"))?;
        // key used to encrypt data
        let key = LessSafeKey::new(unbound_k);

        Ok(Self {
            underlying,
            key,
            read_remainder: vec![],
            unwritten: vec![]
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
        //        dbg!(&bufsiz);
        self.key
            .seal_in_place_append_tag(nonce, Aad::empty(), &mut encrypt_me)
            .unwrap();

        //        dbg!(encrypt_me.len());
        let packet = EncryptedPacket { data: encrypt_me };
        let bincoded = bincode::serialize(&packet).unwrap();
        self.underlying
            .as_mut()
            .poll_write(cx, bincoded.as_slice())
            .map(|r| match r {
                Ok(really_wrote) => {
                    //                    dbg!(really_wrote);
                    Ok(bufsiz)
                }
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
        buf: &mut [u8],
    ) -> Poll<Result<usize, io::Error>> {

        let mut written_from_unwritten = 0;
        while let Some(unwritten) = self.unwritten.pop() {
            dbg!("Writing reserved packet");
            Write::write_all(&mut buf.as_mut(), &unwritten).expect("Must work");
            written_from_unwritten += unwritten.len();
        }

        // TODO: have both sides agree on nonce sequence somehow?
        let nonce = Nonce::assume_unique_for_key([0; 12]);

        // TODO: How to ensure sufficient padding here...?
        let mut read_buf = vec![0; buf.len()];
        let read = self.underlying.as_mut().poll_read(cx, &mut read_buf);

        match &read {
            Poll::Ready(Ok(bytes_read)) => {
                if *bytes_read == 0 {
                    dbg!("WAAAAAAAAAAAAAAAHHH");
                    if self.read_remainder.is_empty() {
                        return Poll::Ready(Ok(written_from_unwritten));
                    }
                    panic!("Shouldn't happen??!")
                }
                dbg!(&bytes_read);
                dbg!(self.read_remainder.len());
                let remainder_len = self.read_remainder.len();
                let mut remainder_plus_read =
                    [self.read_remainder.as_slice(), read_buf.as_slice()].concat();
                // Drop portion of the buffer which is just useless zero padding, if any.
                let useless_buffer_bytes = read_buf.len() - *bytes_read;
                remainder_plus_read.split_off(remainder_plus_read.len() - useless_buffer_bytes);
                let written =
                    self.read_packets_from_buffer(&mut read_buf, &mut remainder_plus_read, buf);
                if written == 0 {
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                return Poll::Ready(Ok(written));
            }
            _ => (),
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
        read_buf: &mut [u8],
        remainder_plus_read: &mut Vec<u8>,
        write_into: &mut [u8],
    ) -> usize {
        let mut read_cursor = Cursor::new(&remainder_plus_read);
        let mut write_cursor = Cursor::new(write_into);
        let mut total_written = 0;
        let mut successfull_buffer_advance = 0;
        dbg!(remainder_plus_read.len());

        loop {
            dbg!("loopstart", &read_cursor.position());
            // TODO: doesn't belong here
            let nonce = Nonce::assume_unique_for_key([0; 12]);

            let cursor_pos_pre_deserialize_attempt = read_cursor.position();
            let deser_res: Result<EncryptedPacket, _> = bincode::deserialize_from(&mut read_cursor);
            let cursor_pos = read_cursor.position();

            dbg!(&cursor_pos);
            match deser_res {
                Ok(packet) => {
                    successfull_buffer_advance = cursor_pos;
                    let mut decrypt_buff = packet.data;
                    dbg!(&decrypt_buff.len());
                    if decrypt_buff.is_empty() {
                        dbg!("Buffer empty, exiting");
                        break;
                    }

                    let just_content = self
                        .key
                        .open_in_place(nonce, Aad::empty(), &mut decrypt_buff)
                        .unwrap();
                    match Write::write_all(&mut write_cursor, &just_content) {
                        Ok(_) => {
                            dbg!(just_content.len());
                            total_written += just_content.len();
                        }
                        Err(_) => {
                            // TODO: How to avoid? Reducing size of buffer that the underlying
                            //   poll reads into helped, but didn't totally eliminate.
                            dbg!("Oh no big problems!!!");
                            self.unwritten.push(just_content.to_vec());
                        }
                    }
                }
                // TODO: Actually detect unexpected eof
                // When deserialization fails because of unexpected eof, we have a partial packet,
                // so the goal is to store it in the remainder and exit
                Err(e) => {
                    dbg!(e);
                    break;
                }
            };
        }

        // Drop however much of the buffer we *did* read
        self.read_remainder = remainder_plus_read.split_off(successfull_buffer_advance as usize);

        dbg!(total_written);
        total_written
    }
}
