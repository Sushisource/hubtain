use crate::{
    encrypted_stream::ClientEncryptedStreamStarter, filewriter::AsyncFileWriter,
    models::HandshakeReply, BROADCAST_ADDR,
};
use anyhow::{anyhow, Error};
use async_std::{
    future::{self, timeout},
    net::{TcpStream, UdpSocket},
    prelude::FutureExt,
};
use bincode::deserialize;
use derive_more::Constructor;
use futures::{select, AsyncRead, AsyncWrite, FutureExt as OFutureExt};
use indicatif::ProgressBar;
use rand::rngs::OsRng;
use std::{
    fs::OpenOptions,
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::{atomic::AtomicUsize, atomic::Ordering, Arc},
    time::Duration,
};
use x25519_dalek::EphemeralSecret;

/// Client for hubtain's fetch mode
pub struct DownloadClient {
    stream: TcpStream,
    server_info: ServerInfo,
}

impl DownloadClient {
    /// Attempt to connect to a server, if successful, the returned client can be used to
    /// download the data.
    pub async fn connect<SrvPred>(
        udp_port: u16,
        server_selection_strategy: SrvPred,
    ) -> Result<Self, Error>
    where
        SrvPred: FnMut(&ServerInfo) -> bool,
    {
        let broadcast_addr = SocketAddr::new(*BROADCAST_ADDR, udp_port);
        info!("Client broadcasting to {}", &broadcast_addr);
        let mut client_s = UdpSocket::bind("0.0.0.0:0").await?;
        client_s.set_broadcast(true)?;
        let ping = vec![0];
        client_s.send_to(&ping, broadcast_addr).await?;
        let replies = read_replies_for(&mut client_s, Duration::from_secs(1)).await?;
        let replies: Result<Vec<ServerInfo>, Error> = replies
            .into_iter()
            .map(|(bytes, peer)| {
                let server_info: HandshakeReply = deserialize(&bytes)?;
                let mut tcp_sock_addr = peer;
                tcp_sock_addr.set_port(server_info.tcp_port);
                Ok(ServerInfo::new(
                    server_info.file_name,
                    tcp_sock_addr,
                    server_info.data_length,
                    server_info.encrypted,
                ))
            })
            .collect();
        let replies = replies?;
        info!("Client found the following servers: {:?}", &replies);
        if replies.is_empty() {
            return Err(anyhow!("No servers found"));
        }
        // Connect to the tcp port
        let selected_server = replies.into_iter().find(server_selection_strategy).unwrap();
        let stream = TcpStream::connect(selected_server.addr).await?;
        info!(
            "Client on {:?} connected to server at {:?}!",
            stream.local_addr(),
            stream.peer_addr()
        );
        Ok(DownloadClient {
            stream,
            server_info: selected_server,
        })
    }

    /// Tell the client to download the server's data to the provided location on disk. If the
    /// location is a directory, the file's name will be determined by the server it's downloaded
    /// from. This consumes the client.
    pub async fn download_to_file(mut self, path: PathBuf) -> Result<(), Error> {
        let path = if path.is_dir() {
            path.join(&self.server_info.download_name)
        } else {
            path
        };

        let path = path.as_path();
        info!("Will download to {:?}", path.as_os_str());
        let data_len = self.server_info.data_len;

        // Do encryption handshake first
        let stream = self.do_handshake().await?;

        let mut as_fwriter = AsyncFileWriter::new(path)?;
        let bytes_written_ref = as_fwriter.bytes_writen.clone();
        let mut progress_fut = progress_counter(bytes_written_ref, data_len).boxed().fuse();
        let mut download_fut = Self::drain_downloaded_to_stream(&mut as_fwriter, stream)
            .boxed()
            .fuse();

        select! {
            _ = progress_fut => (),
            _ = download_fut => ()
        };
        drop(download_fut);

        info!(
            "Downloaded {} bytes",
            as_fwriter.bytes_writen.load(Ordering::Relaxed),
        );
        drop(as_fwriter);
        // Truncate any extra padding that may have been read
        OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(data_len)?;
        info!("...done!");
        Ok(())
    }

    /// downloads server data to a vec
    #[cfg(test)]
    pub async fn download_to_vec(mut self) -> Result<Vec<u8>, Error> {
        let mut download = Vec::with_capacity(2056);
        let data_len = self.server_info.data_len as usize;
        let stream = self.do_handshake().await?;
        Self::drain_downloaded_to_stream(&mut download, stream).await?;
        // Truncate any extra padding that may have been read
        download.split_off(data_len);
        Ok(download)
    }

    async fn do_handshake(&mut self) -> Result<Pin<Box<dyn AsyncRead + Send + '_>>, Error> {
        if self.server_info.encrypted {
            let secret = EphemeralSecret::new(&mut OsRng);
            let enc_stream = ClientEncryptedStreamStarter::new(&mut self.stream, secret);
            info!("Client encrypytion handshaking");
            let enc_stream = enc_stream.key_exchange().await?;
            Ok(Box::pin(enc_stream))
        } else {
            Ok(Box::pin(&mut self.stream))
        }
    }

    async fn drain_downloaded_to_stream<T>(
        mut download: &mut T,
        stream: impl AsyncRead + Send,
    ) -> Result<u64, Error>
    where
        T: AsyncWrite + Unpin,
    {
        futures::io::copy(stream, &mut download)
            .await
            .map_err(Into::into)
    }
}

async fn progress_counter(progress: Arc<AtomicUsize>, total_size: u64) {
    let pbar = ProgressBar::new(total_size);
    loop {
        // The delay works best first to avoid printing a 0 for no reason
        future::ready(()).delay(Duration::from_millis(100)).await;
        let bytes_read = progress.as_ref().load(Ordering::Relaxed);
        pbar.set_position(bytes_read as u64);
    }
}

#[derive(Constructor, Debug)]
/// Identifying information of a server discovered on the network
pub struct ServerInfo {
    download_name: String,
    addr: SocketAddr,
    data_len: u64,
    encrypted: bool,
}

#[cfg(test)]
/// Server selection strategy for testing that just selects the first server
pub fn test_srvr_sel(_: &ServerInfo) -> bool {
    true
}

async fn read_replies_for(
    sock: &mut UdpSocket,
    duration: Duration,
) -> Result<Vec<(Vec<u8>, SocketAddr)>, Error> {
    // Wait for broadcast from peer
    let mut buf = vec![0u8; 100];
    let mut retme = vec![];
    loop {
        match timeout(duration, sock.recv_from(&mut buf)).await {
            Ok(Ok((_, peer))) => {
                let cur_content = buf.to_vec();
                retme.push((cur_content, peer));
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                break;
            }
        }
    }
    Ok(retme)
}
