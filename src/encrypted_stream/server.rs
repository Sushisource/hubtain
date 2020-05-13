use super::*;
use crate::{
    encrypted_stream::{EncStreamErr, Handshake},
    models::ClientId,
    server::{ClientApprovalStrategy, ClientApprover},
};
use anyhow::{anyhow, Context as AnyhowCtx};
use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ring::aead::{Aad, BoundKey, SealingKey, UnboundKey, CHACHA20_POLY1305};

use std::{cmp::min, io, pin::Pin, task::Poll};

use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

pub struct ServerEncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    secret: EphemeralSecret,
}

impl<'a, S> ServerEncryptedStreamStarter<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S, secret: EphemeralSecret) -> Self {
        ServerEncryptedStreamStarter {
            underlying: Pin::new(underlying_stream),
            secret,
        }
    }

    /// Performs DH key exchange with the other side of the stream. Returns a new version
    /// of the stream that can be read/write from, transparently encrypting/decrypting the
    /// data.
    pub async fn key_exchange(
        mut self,
        approval_strat: ClientApprovalStrategy,
        approver: &dyn ClientApprover,
    ) -> Result<EncryptedWriteStream<'a, S>, EncStreamErr> {
        let pubkey = PublicKey::from(&self.secret);
        let outgoing_hs = Handshake {
            pubkey: *pubkey.as_bytes(),
        };
        // Exchange public keys, first send ours
        let send_hs = bincode::serialize(&outgoing_hs)?;
        self.underlying.write_all(send_hs.as_slice()).await?;

        // Read incoming pubkey
        let read_size = bincode::serialized_size(&outgoing_hs)?;
        let mut buff = vec![0; read_size as usize];
        self.underlying.read_exact(&mut buff).await?;
        let read_hs: Handshake = bincode::deserialize_from(buff.as_slice())?;

        let their_pubkey: PublicKey = read_hs.pubkey.into();

        match approval_strat {
            ClientApprovalStrategy::ApproveAll => {
                #[cfg(not(test))]
                panic!("Approve all mode should never be enabled with encryption in production!");
                // Send approval byte
                #[cfg(test)]
                self.underlying.write_all(&[1]).await?;
            }
            ClientApprovalStrategy::Interactive => {
                let pubkey_bytes = their_pubkey.as_bytes();
                let approved = approver.submit(ClientId::new(*pubkey_bytes)).await?;
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

        // Compute shared secret
        let shared_secret = self.secret.diffie_hellman(&their_pubkey);
        EncryptedWriteStream::new(self.underlying, shared_secret, their_pubkey)
    }
}

pub struct EncryptedWriteStream<'a, S: AsyncWrite> {
    underlying: Pin<&'a mut S>,
    key: SealingKey<SharedSecretNonceSeq>,
    their_pubkey: PublicKey,
}

impl<'a, S> EncryptedWriteStream<'a, S>
where
    S: AsyncWrite,
{
    fn new(
        underlying: Pin<&'a mut S>,
        shared_secret: SharedSecret,
        their_pubkey: PublicKey,
    ) -> Result<Self, EncStreamErr> {
        let unbound_k = UnboundKey::new(&CHACHA20_POLY1305, shared_secret.as_bytes())
            .map_err(|_| anyhow!("Couldn't bind key"))?;
        let derived_nonce_seq = SharedSecretNonceSeq::new(shared_secret);
        // key used to encrypt data
        let key = SealingKey::new(unbound_k, derived_nonce_seq);

        Ok(Self {
            underlying,
            key,
            their_pubkey,
        })
    }

    pub fn get_client_id(&self) -> ClientId {
        ClientId::new(*self.their_pubkey.as_bytes())
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
        let siz = min(buf.len(), CHUNK_SIZE);
        let mut encrypt_me = vec![0; siz];
        encrypt_me.copy_from_slice(&buf[0..siz]);
        let bufsiz = encrypt_me.len();
        self.key
            .seal_in_place_append_tag(Aad::empty(), &mut encrypt_me)
            .unwrap();

        let packet = EncryptedPacket { data: encrypt_me };
        let bincoded = bincode::serialize(&packet).unwrap();
        self.underlying
            .as_mut()
            .poll_write(cx, bincoded.as_slice())
            .map(|r| match r {
                Ok(_) => Ok(bufsiz),
                o => o,
            })
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_close(cx)
    }
}
