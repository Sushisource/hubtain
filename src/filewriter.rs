use anyhow::Error;
use futures::{io::AsyncWrite, task::Context};
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    pin::Pin,
    sync::{atomic::AtomicUsize, atomic::Ordering, Arc},
    task::Poll,
};

pub struct AsyncFileWriter {
    file: File,
    pub bytes_writen: Arc<AtomicUsize>,
}

impl AsyncFileWriter {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Ok(AsyncFileWriter {
            file: File::create(&path)?,
            bytes_writen: Arc::new(AtomicUsize::new(0)),
        })
    }
}

impl AsyncWrite for AsyncFileWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let bytes_written = self.file.write(buf)?;
        self.bytes_writen.fetch_add(bytes_written, Ordering::SeqCst);
        Poll::Ready(Ok(bytes_written))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.file.flush()?;
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}
