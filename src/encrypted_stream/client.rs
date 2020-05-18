use super::*;
use crate::models::ClientId;
use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, FutureExt};
use snow::TransportState;
use std::{io, pin::Pin, task::Poll};

pub struct ClientEncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
}

impl<'a, S> ClientEncryptedStreamStarter<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S) -> Self {
        ClientEncryptedStreamStarter {
            underlying: Pin::new(underlying_stream),
        }
    }

    /// Performs DH key exchange with the server side of the stream. Returns a new version
    /// of the stream that can be read/write from, transparently encrypting/decrypting the
    /// data.
    pub async fn key_exchange(mut self) -> Result<EncryptedReadStream<'a, S>, EncStreamErr> {
        let noise = snow::Builder::new(PATTERN.parse().unwrap());
        let key = noise.generate_keypair().unwrap();

        info!("Your client id is: {}", ClientId::new(key.public.clone()));

        let mut noise = noise
            .local_private_key(&key.private)
            .build_initiator()
            .unwrap();
        let mut buf = vec![0u8; 65535];
        // -> e
        let len = noise.write_message(&[], &mut buf).unwrap();
        send(&mut self.underlying, &buf[..len]).await.unwrap();
        // <- e, ee, s, es
        noise
            .read_message(&recv(&mut self.underlying).await.unwrap(), &mut buf)
            .unwrap();
        // -> s, se
        let len = noise.write_message(&[], &mut buf).unwrap();
        send(&mut self.underlying, &buf[..len]).await.unwrap();

        // Client reads the accepted/rejected byte
        let mut buff = vec![0; 1];
        info!("Awaiting approval from server...");
        self.underlying.read_exact(&mut buff).await?;
        // Give up if we were rejected
        if buff[0] != 1 {
            return Err(EncStreamErr::ClientNotAccepted);
        }
        info!("Approved.");

        let noise = noise.into_transport_mode().unwrap();
        EncryptedReadStream::new(self.underlying, noise)
    }
}

pub struct EncryptedReadStream<'a, S: AsyncRead> {
    underlying: Pin<&'a mut S>,
    noise: TransportState,
}

impl<'a, S> EncryptedReadStream<'a, S>
where
    S: AsyncRead,
{
    fn new(underlying: Pin<&'a mut S>, noise: TransportState) -> Result<Self, EncStreamErr> {
        Ok(Self { underlying, noise })
    }
}

impl<'a, S> AsyncRead for EncryptedReadStream<'a, S>
where
    S: AsyncRead + AsyncWrite,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        mut buf: &mut [u8],
    ) -> Poll<Result<usize, io::Error>> {
        let r = {
            let rcv_fut = recv(&mut self.underlying);
            pin_utils::pin_mut!(rcv_fut);
            rcv_fut.poll_unpin(cx)
        };

        match r {
            Poll::Ready(Ok(msg)) => {
                dbg!(dbghash(&msg));
                let written = self.noise.read_message(&msg, &mut buf).unwrap();
                Poll::Ready(Ok(written))
            }
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                dbg!("Eof");
                Poll::Ready(Ok(0))
            }
            Poll::Ready(Err(e)) => panic!(e),
            Poll::Pending => {
                info!("Poll read pending");
                Poll::Pending
            }
        }
    }
}
