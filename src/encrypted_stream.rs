use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use std::{io, io::Cursor, pin::Pin, task::Poll};
use x25519_dalek::PublicKey;
use serde::{Deserialize, Serialize};

pub struct EncryptedStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    handshake_complete: bool,
    pubkey: PublicKey
}

#[derive(Serialize, Deserialize)]
struct Handshake {
    pkey: [u8; 32]
}

impl<'a, S> EncryptedStream<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S , pubkey: PublicKey) -> Self {
        EncryptedStream {
            underlying: Pin::new(underlying_stream),
            handshake_complete: false,
            pubkey
        }
    }

    pub async fn handshake(&mut self) -> Result<(), anyhow::Error> {
        // Exchange public keys
        let send_hs = bincode::serialize(&Handshake {
            pkey: *self.pubkey.as_bytes()
        })?;
        self.underlying.write_all(send_hs.as_slice());

        self.handshake_complete = true;
        Ok(())
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
        if !self.handshake_complete {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Other,
                "Handshake incomplete!",
            )));
        }
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


