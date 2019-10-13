use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ring::aead::{chacha20_poly1305_openssh::SealingKey, CHACHA20_POLY1305, UnboundKey};
use serde::{Deserialize, Serialize};
use std::{io, pin::Pin, task::Poll};
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};
use anyhow::Error;

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

        Ok(EncryptedStream {
            underlying: self.underlying,
            shared_secret,
        })
    }
}

pub struct EncryptedStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    shared_secret: SharedSecret,
}

impl<'a, S> EncryptedStream<'a, S>
    where
        S: AsyncWrite + AsyncRead,
{
    fn new(underlying_stream: &'a mut S) -> Self {

    }
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
        // TODO: Move this stuff to constructor
        let unbound_k = UnboundKey::new(&CHACHA20_POLY1305, &self.shared_secret.as_bytes())?;
        // Sealing key used to encrypt data
        let sealing_key =
            SealingKey::new(&CHACHA20_POLY1305, &self.shared_secret.as_bytes()).unwrap();

        // TODO: have both sides agree on nonce sequence somehow
        let mut nonce = vec![0; 12];

        sealing_key.seal_in_place_append_tag();

        self.underlying.as_mut().poll_write(cx, buf)
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
        self.underlying.as_mut().poll_read(cx, buf)
    }
}
