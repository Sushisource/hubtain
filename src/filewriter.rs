use failure::Error;
use futures::{io::AsyncWrite, task::Context};
use std::{fs::File, io, io::Write, path::Path, pin::Pin, task::Poll};

pub struct AsyncFileWriter {
    file: File,
}

impl AsyncFileWriter {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Ok(AsyncFileWriter {
            file: File::create(&path)?,
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
