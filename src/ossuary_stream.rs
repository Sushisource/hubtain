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
            "Client"
        };
        loop {
            println!("{} start loop", cheap_hack);
            match self.ossuary_conn.handshake_done() {
                Ok(false) => {
                    println!("{} send handshake", cheap_hack);
                    let mut wb = Vec::with_capacity(1024);
                    let siz = self.ossuary_conn.send_handshake(&mut wb)?;
                    if siz > 0 {
                        self.underlying.write_all(&mut wb).await?;
                    }

                    println!("{} done send, start rcv handshake", cheap_hack);
                    let mut rb = vec![0; 512];
                    let bytes_read_from_stream = self.underlying.read(&mut rb).await?;
                    dbg!(cheap_hack, bytes_read_from_stream);
                    println!("{} done underlying read", cheap_hack);
                    if bytes_read_from_stream > 0 {
                        dbg!(cheap_hack, hex::encode(&rb));
                        let mut rb = Cursor::new(rb);
                        let bytes_read_for_shake = self.ossuary_conn.recv_handshake(&mut rb)?;
                        println!("{} done rcv handshake", cheap_hack);
                        dbg!(cheap_hack, bytes_read_for_shake);
                        // TODO: Is this really invariant?
                        assert_eq!(bytes_read_from_stream, bytes_read_for_shake);
                    }

                    println!("{} loop done", cheap_hack);
                    continue;
                }
                Ok(true) => break,
                Err(OssuaryError::UntrustedServer(pubkey)) => {
                    let keys: Vec<&[u8]> = vec![&pubkey];
                    self.ossuary_conn.add_authorized_keys(keys)?;
                    continue;
                }
                e @ Err(_) => return e.map(|_| ()),
            }
        }
        println!("{} done handshaking!!!", cheap_hack);
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
