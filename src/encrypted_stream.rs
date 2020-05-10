//! Defines write and read halves of async streams that can perform encryption.
//!
//! Probably doing the encryption/decryption inside the poll() functions is less than ideal
//! since it could be considered blocking. I also think I could probably avoid some of the weird
//! chunking/leftover bytes stuff. But this does make for an easy-to-use interface and is plenty
//! fast in practice.

use crate::{
    models::ClientId,
    server::{ClientApprovalStrategy, ClientApprover},
};
use anyhow::{anyhow, Context as AnyhowCtx};
use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ring::aead::{
    Aad, BoundKey, Nonce, NonceSequence, OpeningKey, SealingKey, UnboundKey, CHACHA20_POLY1305,
    NONCE_LEN,
};
use ring::error::Unspecified;
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
    pubkey: [u8; 32],
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
        approval_strat: ClientApprovalStrategy,
        approver: &dyn ClientApprover,
    ) -> Result<EncryptedWriteStream<'a, S>, EncStreamErr> {
        let pubkey = PublicKey::from(&self.secret);
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

        match approval_strat {
            ClientApprovalStrategy::ApproveAll => {
                #[cfg(not(test))]
                panic!("Approve all mode should never be enabled with encryption in production!");
                // Send approval byte
                #[cfg(test)]
                self.underlying.write_all(&[1]).await?;
            }
            ClientApprovalStrategy::Interactive => {
                let pubkey_bytes = their_pubkey.as_bytes();
                let approved = approver.submit(ClientId::new(*pubkey_bytes)).await?;
                // Server sends signal to client if it is not approved for graceful hangup
                let approval_byte = if approved { 1 } else { 0 };
                self.underlying
                    .write_all(&[approval_byte])
                    .await
                    .context("Client hung up during approval")?;

                if !approved {
                    return Err(EncStreamErr::ClientNotAccepted);
                }
            }
        };

        // Compute shared secret
        let shared_secret = self.secret.diffie_hellman(&their_pubkey);
        EncryptedWriteStream::new(self.underlying, shared_secret, their_pubkey)
    }
}

const CHUNK_SIZE: usize = 5012;

#[derive(Serialize, Deserialize)]
struct EncryptedPacket {
    data: Vec<u8>,
}

pub struct EncryptedWriteStream<'a, S: AsyncWrite> {
    underlying: Pin<&'a mut S>,
    key: SealingKey<SharedSecretNonceSeq>,
    their_pubkey: PublicKey,
}

impl<'a, S> EncryptedWriteStream<'a, S>
where
    S: AsyncWrite,
{
    fn new(
        underlying: Pin<&'a mut S>,
        shared_secret: SharedSecret,
        their_pubkey: PublicKey,
    ) -> Result<Self, EncStreamErr> {
        let unbound_k = UnboundKey::new(&CHACHA20_POLY1305, shared_secret.as_bytes())
            .map_err(|_| anyhow!("Couldn't bind key"))?;
        let derived_nonce_seq = SharedSecretNonceSeq::new(shared_secret);
        // key used to encrypt data
        let key = SealingKey::new(unbound_k, derived_nonce_seq);

        Ok(Self {
            underlying,
            key,
            their_pubkey,
        })
    }

    pub fn get_client_id(&self) -> ClientId {
        ClientId::new(*self.their_pubkey.as_bytes())
    }
}

impl<'a, S> AsyncWrite for EncryptedWriteStream<'a, S>
where
    S: AsyncWrite,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let siz = min(buf.len(), CHUNK_SIZE);
        let mut encrypt_me = vec![0; siz];
        encrypt_me.copy_from_slice(&buf[0..siz]);
        let bufsiz = encrypt_me.len();
        self.key
            .seal_in_place_append_tag(Aad::empty(), &mut encrypt_me)
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

impl<'a, S> EncryptedReadStream<'a, S>
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

        total_written
    }
}

#[derive(Constructor)]
struct SharedSecretNonceSeq {
    inner: SharedSecret,
}

impl NonceSequence for SharedSecretNonceSeq {
    fn advance(&mut self) -> Result<Nonce, Unspecified> {
        Ok(Nonce::try_assume_unique_for_key(
            &self.inner.as_bytes()[..NONCE_LEN],
        )?)
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

#[cfg(test)]
mod encrypted_stream_tests {
    use super::*;
    use crate::server::ConsoleApprover;
    use async_std::{os::unix::net::UnixStream, task::block_on};
    use futures::{future::join, io::Cursor};
    use rand::rngs::OsRng;
    use test::Bencher;

    #[async_std::test]
    async fn encrypted_copy_works() {
        let test_data = &b"Oh boy what fun data to send!".repeat(100);
        let (server_sock, mut client_sock) = UnixStream::pair().unwrap();

        let server_task = server_task(test_data, server_sock);
        let client_task = client_task(&mut client_sock);

        join(server_task, client_task).await;
    }

    #[bench]
    fn full_small_encrypted_transfer_with_exchange(b: &mut Bencher) {
        b.iter(|| {
            block_on(async {
                let test_data = &b"Oh boy what fun data to send!".repeat(100);
                let (server_sock, mut client_sock) = UnixStream::pair().unwrap();

                let server_task = server_task(test_data, server_sock);
                let client_task = client_task(&mut client_sock);

                join(server_task, client_task).await;
            })
        })
    }

    #[inline]
    async fn server_task(test_data: &[u8], mut server_sock: UnixStream) {
        let data_src = Cursor::new(test_data);
        let secret = EphemeralSecret::new(&mut OsRng);
        let server_stream = ServerEncryptedStreamStarter::new(&mut server_sock, secret);
        let ca = ConsoleApprover::default();
        let mut enc_stream = server_stream
            .key_exchange(ClientApprovalStrategy::ApproveAll, &ca)
            .await
            .unwrap();
        futures::io::copy(data_src, &mut enc_stream).await.unwrap();
    }

    #[inline]
    async fn client_task(mut client_sock: &mut UnixStream) -> Vec<u8> {
        let mut data_sink = Cursor::new(vec![]);
        let secret = EphemeralSecret::new(&mut OsRng);
        let enc_stream = ClientEncryptedStreamStarter::new(&mut client_sock, secret);
        let enc_stream = enc_stream.key_exchange().await.unwrap();
        futures::io::copy(enc_stream, &mut data_sink).await.unwrap();
        data_sink.into_inner()
    }
}
