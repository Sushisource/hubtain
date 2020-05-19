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

use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use std::fmt::Debug;
use thiserror::Error as DError;

static PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const MAX_CHUNK_SIZE: usize = 65535;

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

// TODO: Make this better

/// Hyper-basic stream transport receiver. 16-bit BE size followed by payload.
pub async fn recv<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<Vec<u8>> {
    let mut msg_len_buf = [0u8; 2];
    stream.read_exact(&mut msg_len_buf).await?;
    let msg_len = u16::from_be_bytes(msg_len_buf);
    let mut msg = vec![0u8; msg_len as usize];
    stream.read_exact(&mut msg[..]).await?;
    Ok(msg)
}

/// Hyper-basic stream transport sender. 16-bit BE size followed by payload.
pub async fn send<S: AsyncWrite + Unpin>(stream: &mut S, buf: &[u8]) -> std::io::Result<usize> {
    let msg_len_buf = (buf.len() as u16).to_be_bytes();
    stream.write_all(&msg_len_buf).await.unwrap();
    stream.write_all(buf).await.unwrap();
    Ok(msg_len_buf.len() + buf.len())
}

#[cfg(test)]
mod encrypted_stream_tests {
    use super::*;
    use crate::{
        encrypted_stream::server::ServerEncryptedStreamStarter, server::ClientApprovalStrategy,
        server::ConsoleApprover,
    };
    use async_std::task::block_on;
    use futures::{future::join, io::Cursor};
    use futures_ringbuf::Endpoint;
    use log::LevelFilter;
    use test::Bencher;

    #[async_std::test]
    #[ignore]
    async fn encrypted_copy_works() {
        // TOOD: Third read packet is wrong somehow in this setup
        env_logger::builder()
            .filter_level(LevelFilter::Debug)
            .init();
        let test_data = &b"Oh boy what fun data to send!".repeat(100_000);
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
        let server_stream = ServerEncryptedStreamStarter::new(&mut server_sock);
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
        let enc_stream = ClientEncryptedStreamStarter::new(&mut client_sock);
        let enc_stream = enc_stream.key_exchange().await.unwrap();
        futures::io::copy(enc_stream, &mut data_sink).await.unwrap();
        data_sink.into_inner()
    }
}
