use anyhow::Error;
use futures::io::AsyncRead;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};

pub struct AsyncFileReader {
    file: File,
    pub file_size: u64,
    orig_path: PathBuf,
}

impl Clone for AsyncFileReader {
    fn clone(&self) -> Self {
        AsyncFileReader {
            file: File::open(self.orig_path.as_path()).expect("Bwaaargh file open explosion"),
            file_size: self.file_size,
            orig_path: self.orig_path.clone(),
        }
    }
}

impl AsyncFileReader {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let file = File::open(&path)?;
        let file_size = file.metadata()?.len();
        Ok(AsyncFileReader {
            file,
            file_size,
            orig_path: path.as_ref().to_path_buf(),
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
