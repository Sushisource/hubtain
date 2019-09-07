use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ossuary::{OssuaryConnection, OssuaryError};
use std::{io, io::Cursor, pin::Pin, task::Poll};

pub type OssuaryKey = ([u8; 32], [u8; 32]);

pub struct OssuaryStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    ossuary_conn: OssuaryConnection,
    internal_buf: Cursor<Vec<u8>>,
    handshake_complete: bool,
}

impl<'a, S> OssuaryStream<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S, ossuary_conn: OssuaryConnection) -> Self {
        OssuaryStream {
            underlying: Pin::new(underlying_stream),
            ossuary_conn,
            internal_buf: Cursor::new(vec![]),
            handshake_complete: false,
        }
    }

    pub async fn handshake(&mut self) -> Result<(), OssuaryError> {
        let cheap_hack = if self.ossuary_conn.is_server() {
            "Server"
        } else {
            // TODO: Why the fuck is server not even getting these
            self.underlying.write_all(&[1,2,3]).await?;
            "Client"
        };
        let mut rb = Cursor::new(Vec::with_capacity(1024));
        loop {
            if !self.ossuary_conn.handshake_done().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Ossuary handshake err {:?}", e),
                )
            })? {
                println!("{} send handshake", cheap_hack);
                let mut wb = Vec::with_capacity(1024);
                let siz = self.ossuary_conn.send_handshake(&mut wb)?;
                if siz > 0 {
                    dbg!(&cheap_hack, &wb);
                    self.underlying.write_all(&mut wb).await?;
                }

                println!("{} done send, start rcv handshake", cheap_hack);
                // TODO: Why is this ALWAYS reading 0 bytes?
                dbg!(self.underlying.read(rb.get_mut()).await?);
                dbg!(&rb);
                println!("{} did underlying read", cheap_hack);
                match self.ossuary_conn.recv_handshake(&mut rb) {
                    Err(OssuaryError::WouldBlock(_)) => continue,
                    o => {
                        o?;
                    }
                }

                println!("{} loop done", cheap_hack);
            } else {
                break;
            }
        }
        println!("Done handshaking!!!");
        self.handshake_complete = true;
        Ok(())
    }
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
