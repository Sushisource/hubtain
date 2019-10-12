use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use std::{io, io::Cursor, pin::Pin, task::Poll};

pub struct EncryptedStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    handshake_complete: bool,
}

impl<'a, S> EncryptedStream<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S ) -> Self {
        EncryptedStream {
            underlying: Pin::new(underlying_stream),
            handshake_complete: false,
        }
    }

    pub async fn handshake(&mut self) -> Result<(), std::io::Error> {
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
