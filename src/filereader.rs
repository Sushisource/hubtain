use failure::Error;
use futures::io::AsyncRead;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct AsyncFileReader {
    file: File,
}

impl AsyncFileReader {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Ok(AsyncFileReader {
            file: File::open(path)?,
        })
    }
}

impl AsyncRead for AsyncFileReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let bytes_read = self.file.read(buf)?;
        Poll::Ready(Ok(bytes_read))
    }
}
