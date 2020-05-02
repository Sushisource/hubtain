use futures::{io::AsyncWrite, task::Context};
use std::{
    io::{self},
    pin::Pin,
    sync::{atomic::AtomicUsize, atomic::Ordering, Arc},
    task::Poll,
};

/// A wrapper around an `AsyncWrite` stream which counts the number of bytes that have been written
/// so far
pub struct ProgressWriter<W> {
    inner: W,
    bytes_writen: Arc<AtomicUsize>,
}

impl<W> ProgressWriter<W> {
    // / Returns the number of bytes that have been written to the stream so far
    // pub fn get_written(&self) -> usize {
    //     self.bytes_writen.load(Ordering::SeqCst)
    // }
}

impl<W> AsyncWrite for ProgressWriter<W>
where
    W: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let res = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = &res {
            self.bytes_writen.fetch_add(*written, Ordering::SeqCst);
        };
        res
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}
