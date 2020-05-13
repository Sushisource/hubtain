//! Defines write and read halves of async streams that can perform encryption.
//!
//! Probably doing the encryption/decryption inside the poll() functions is less than ideal
//! since it could be considered blocking. I also think I could probably avoid some of the weird
//! chunking/leftover bytes stuff. But this does make for an easy-to-use interface and is plenty
//! fast in practice.

mod client;
mod server;

pub use client::ClientEncryptedStreamStarter;
pub use server::ServerEncryptedStreamStarter;

use ring::aead::{
    Aad, BoundKey, Nonce, NonceSequence, OpeningKey, UnboundKey, CHACHA20_POLY1305,
    NONCE_LEN,
};
use ring::error::Unspecified;
use serde::{Deserialize, Serialize};
use thiserror::Error as DError;
use x25519_dalek::SharedSecret;

#[derive(Serialize, Deserialize, Debug)]
struct Handshake {
    pubkey: [u8; 32],
}

const CHUNK_SIZE: usize = 5012;

#[derive(Serialize, Deserialize)]
struct EncryptedPacket {
    data: Vec<u8>,
}

#[derive(Constructor)]
struct SharedSecretNonceSeq {
    inner: SharedSecret,
}

impl NonceSequence for SharedSecretNonceSeq {
    fn advance(&mut self) -> Result<Nonce, Unspecified> {
        Ok(Nonce::try_assume_unique_for_key(
            &self.inner.as_bytes()[..NONCE_LEN],
        )?)
    }
}

#[derive(Debug, DError)]
pub enum EncStreamErr {
    #[error("Client rejected by server")]
    ClientNotAccepted,
    #[error("Bincode serialization error")]
    BincodeErr {
        #[from]
        source: Box<bincode::ErrorKind>,
    },
    #[error("I/O error")]
    IOErr {
        #[from]
        source: std::io::Error,
    },
    #[error("Other error")]
    Other {
        #[from]
        source: anyhow::Error,
    },
}

#[cfg(test)]
mod encrypted_stream_tests {
    use super::*;
    use crate::server::ClientApprovalStrategy;
    use crate::{encrypted_stream::server::ServerEncryptedStreamStarter, server::ConsoleApprover};
    use async_std::task::block_on;
    use futures::{future::join, io::Cursor};
    use futures_ringbuf::Endpoint;
    use rand::rngs::OsRng;
    use test::Bencher;
    use x25519_dalek::EphemeralSecret;

    #[async_std::test]
    async fn encrypted_copy_works() {
        let test_data = &b"Oh boy what fun data to send!".repeat(500_000);
        let (server, mut client) = Endpoint::pair(10000, 10000);

        let server_task = server_task(test_data, server);
        let client_task = client_task(&mut client);

        join(server_task, client_task).await;
    }

    #[bench]
    fn full_small_encrypted_transfer_with_exchange(b: &mut Bencher) {
        let test_data = &b"Oh boy what fun data to send!".repeat(100);
        b.iter(|| {
            block_on(async {
                let (server, mut client) = Endpoint::pair(10000, 10000);

                let server_task = server_task(test_data, server);
                let client_task = client_task(&mut client);

                join(server_task, client_task).await;
            })
        })
    }

    #[inline]
    async fn server_task(test_data: &[u8], mut server_sock: Endpoint) {
        let data_src = Cursor::new(test_data);
        let secret = EphemeralSecret::new(&mut OsRng);
        let server_stream = ServerEncryptedStreamStarter::new(&mut server_sock, secret);
        let ca = ConsoleApprover::default();
        let mut enc_stream = server_stream
            .key_exchange(ClientApprovalStrategy::ApproveAll, &ca)
            .await
            .unwrap();
        futures::io::copy(data_src, &mut enc_stream).await.unwrap();
    }

    #[inline]
    async fn client_task(mut client_sock: &mut Endpoint) -> Vec<u8> {
        let mut data_sink = Cursor::new(vec![]);
        let secret = EphemeralSecret::new(&mut OsRng);
        let enc_stream = ClientEncryptedStreamStarter::new(&mut client_sock, secret);
        let enc_stream = enc_stream.key_exchange().await.unwrap();
        futures::io::copy(enc_stream, &mut data_sink).await.unwrap();
        data_sink.into_inner()
    }
}
