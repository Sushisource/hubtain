use super::*;
use crate::{
    encrypted_stream::EncStreamErr,
    models::ClientId,
    server::{ClientApprovalStrategy, ClientApprover},
};
use anyhow::Context as AnyhowCtx;
use futures::{task::Context, AsyncRead, AsyncWrite, AsyncWriteExt, FutureExt};
use snow::TransportState;
use std::{io, pin::Pin, task::Poll};

pub struct ServerEncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
}

impl<'a, S> ServerEncryptedStreamStarter<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S) -> Self {
        ServerEncryptedStreamStarter {
            underlying: Pin::new(underlying_stream),
        }
    }

    /// Performs DH key exchange with the client side of the stream. Returns a new version
    /// of the stream that can be read/write from, transparently encrypting/decrypting the
    /// data.
    pub async fn key_exchange(
        mut self,
        approval_strat: ClientApprovalStrategy,
        approver: &dyn ClientApprover,
    ) -> Result<EncryptedWriteStream<'a, S>, EncStreamErr> {
        let noise = snow::Builder::new(PATTERN.parse().unwrap());
        let key = noise.generate_keypair().unwrap();

        let mut noise = noise
            .local_private_key(&key.private)
            .build_responder()
            .unwrap();
        let mut buf = vec![0u8; MAX_CHUNK_SIZE];
        // <- e
        noise
            .read_message(&recv(&mut self.underlying).await.unwrap(), &mut buf)
            .unwrap();
        // -> e, ee, s, es
        let len = noise.write_message(&[0u8; 0], &mut buf).unwrap();
        send(&mut self.underlying, &buf[..len]).await.unwrap();
        // <- s, se
        noise
            .read_message(&recv(&mut self.underlying).await.unwrap(), &mut buf)
            .unwrap();

        let their_pubkey = noise.get_remote_static().unwrap().to_vec();
        match approval_strat {
            ClientApprovalStrategy::ApproveAll => {
                #[cfg(not(test))]
                panic!("Approve all mode should never be enabled with encryption in production!");
                // Send approval byte
                #[cfg(test)]
                self.underlying.write_all(&[1]).await?;
                #[cfg(test)]
                info!("Sent approve byte");
            }
            ClientApprovalStrategy::Interactive => {
                let approved = approver.submit(ClientId::new(their_pubkey.clone())).await?;
                // Server sends signal to client if it is not approved for graceful hangup
                let approval_byte = if approved { 1 } else { 0 };
                self.underlying
                    .write_all(&[approval_byte])
                    .await
                    .context("Client hung up during approval")?;

                if !approved {
                    return Err(EncStreamErr::ClientNotAccepted);
                }
            }
        };

        let noise = noise.into_transport_mode().unwrap();
        EncryptedWriteStream::new(self.underlying, noise, their_pubkey)
    }
}

pub struct EncryptedWriteStream<'a, S: AsyncWrite> {
    underlying: Pin<&'a mut S>,
    noise: TransportState,
    their_pubkey: Vec<u8>,
}

impl<'a, S> EncryptedWriteStream<'a, S>
where
    S: AsyncWrite,
{
    fn new(
        underlying: Pin<&'a mut S>,
        noise: TransportState,
        their_pubkey: Vec<u8>,
    ) -> Result<Self, EncStreamErr> {
        Ok(Self {
            underlying,
            noise,
            their_pubkey,
        })
    }

    pub fn get_client_id(&self) -> ClientId {
        ClientId::new(self.their_pubkey.clone())
    }
}

impl<'a, S> AsyncWrite for EncryptedWriteStream<'a, S>
where
    S: AsyncWrite,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        // TODO: Don't reallocate every poll
        let mut msg_buf = vec![0; MAX_CHUNK_SIZE];
        let len = self.noise.write_message(&buf, &mut msg_buf).unwrap();
        let send_fut = send(&mut self.underlying, &msg_buf[..len]);
        pin_utils::pin_mut!(send_fut);
        let retme = send_fut.poll_unpin(cx);
        // TODO: Get rid of this check
        if let Poll::Ready(Ok(sent_len)) = &retme {
            if *sent_len != len + 2 {
                dbg!(sent_len, len);
                panic!("Not enough written");
            }
        };
        retme
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_close(cx)
    }
}
