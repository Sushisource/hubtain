use futures::{task::Context, AsyncRead, AsyncWrite};
use ossuary::OssuaryError;
use std::{io, pin::Pin, task::Poll};

pub struct OssuaryStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    handshake_complete: bool,
}

impl<'a, S> OssuaryStream<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S) -> Self {
        OssuaryStream {
            underlying: Pin::new(underlying_stream),
            handshake_complete: false,
        }
    }

    pub fn handshake(&self) -> Result<(), OssuaryError> {
        Ok(())
    }
}

pub enum OssuaryStreamErr {
    HandshakeFail,
}

impl<'a, S> AsyncWrite for OssuaryStream<'a, S>
where
    S: AsyncWrite + AsyncRead,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        if !self.handshake_complete {}
        self.underlying.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_close(cx)
    }
}

impl<'a, S> AsyncRead for OssuaryStream<'a, S>
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
