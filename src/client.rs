use crate::{
    encrypted_stream::ClientEncryptedStreamStarter, filewriter::AsyncFileWriter,
    models::DataSrvInfo, models::DiscoveryReply, BROADCAST_ADDR,
};
use anyhow::{anyhow, Context, Error};
use async_std::{
    future::{self, timeout},
    net::ToSocketAddrs,
    net::{TcpStream, UdpSocket},
    prelude::FutureExt,
};
use bincode::deserialize;
use derive_more::Constructor;
use futures::{select, AsyncRead, AsyncWrite, FutureExt as OFutureExt};
use indicatif::ProgressBar;
use std::{
    fs::OpenOptions,
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::{atomic::AtomicUsize, atomic::Ordering, Arc},
    time::Duration,
};

/// Client for hubtain's fetch mode
pub struct DownloadClient {
    stream: TcpStream,
}

impl DownloadClient {
    /// Attempt to connect to a server, if successful, the returned client can be used to
    /// download the data.
    pub async fn find_server<SrvPred>(
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
        #[cfg(not(test))]
        let read_dur = Duration::from_secs(2);
        #[cfg(test)]
        let read_dur = Duration::from_millis(200);
        let replies = read_replies_for(&mut client_s, read_dur).await?;
        let replies: Result<Vec<_>, Error> = replies
            .into_iter()
            .map(|(bytes, peer)| {
                let reply: DiscoveryReply = deserialize(&bytes)?;
                let mut tcp_sock_addr = peer;
                tcp_sock_addr.set_port(reply.tcp_port);
                Ok(ServerInfo::new(tcp_sock_addr))
            })
            .collect();
        let replies = replies?;
        if replies.is_empty() {
            return Err(anyhow!(
                "No servers found when broadcasting to {}",
                &broadcast_addr
            ));
        }
        info!("Client found the following servers: {:?}", &replies);
        // Connect to the tcp port
        let selected_server = replies
            .into_iter()
            .find(server_selection_strategy)
            .ok_or_else(|| anyhow!("Failed to select a server out of found servers!"))?;
        Self::connect(selected_server.addr).await
    }

    pub async fn connect<A: ToSocketAddrs>(server_addr: A) -> Result<Self, Error> {
        let stream = timeout(Duration::from_secs(10), TcpStream::connect(server_addr))
            .await
            .context("Timeout connecting to server")??;
        info!(
            "Client on {:?} connected to server at {:?}!",
            stream.local_addr(),
            stream.peer_addr()
        );
        Ok(DownloadClient { stream })
    }

    /// Tell the client to download the server's data to the provided location on disk. If the
    /// location is a directory, the file's name will be determined by the server it's downloaded
    /// from. This consumes the client.
    pub async fn download_to_file(mut self, path: PathBuf) -> Result<(), Error> {
        // First read the server's info
        let info = DataSrvInfo::read_from_stream(&mut self.stream).await?;

        let path = if path.is_dir() {
            path.join(&info.data_name)
        } else {
            path
        };
        let path = path.as_path();
        info!("Will download to {:?}", path.as_os_str());

        // Do encryption handshake first
        let stream = if info.encrypted {
            self.encryption_handshake().await?
        } else {
            Box::pin(self.stream)
        };

        let mut as_fwriter = AsyncFileWriter::new(path)?;
        let bytes_written_ref = as_fwriter.bytes_writen.clone();
        let data_len = info.data_length;
        let mut progress_fut = progress_counter(bytes_written_ref, data_len).boxed().fuse();
        let mut download_fut = Self::drain_downloaded_to_stream(&mut as_fwriter, stream)
            .boxed()
            .fuse();

        select! {
            _ = progress_fut => {},
            _ = download_fut => {}
        };
        drop(download_fut);

        info!(
            "Downloaded {} bytes",
            as_fwriter.bytes_writen.load(Ordering::Relaxed),
        );
        drop(as_fwriter);
        // Truncate any extra padding that may exist
        OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(data_len)?;
        Ok(())
    }

    /// downloads server data to a vec
    #[cfg(test)]
    pub async fn download_to_vec(mut self) -> Result<Vec<u8>, Error> {
        let info = DataSrvInfo::read_from_stream(&mut self.stream).await?;
        let mut download = Vec::with_capacity(2056);
        let stream = if info.encrypted {
            self.encryption_handshake().await?
        } else {
            Box::pin(self.stream)
        };
        Self::drain_downloaded_to_stream(&mut download, stream).await?;
        // Truncate any extra padding that might be left over from vec capacity
        download.truncate(info.data_length as usize);
        Ok(download)
    }

    async fn encryption_handshake(&mut self) -> Result<Pin<Box<dyn AsyncRead + Send + '_>>, Error> {
        let enc_stream = ClientEncryptedStreamStarter::new(&mut self.stream);
        info!("Client encrypytion handshaking");
        let enc_stream = enc_stream.key_exchange().await?;
        Ok(Box::pin(enc_stream))
    }

    async fn drain_downloaded_to_stream<T>(
        mut download: &mut T,
        stream: impl AsyncRead,
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
    addr: SocketAddr,
}

#[cfg(test)]
/// Server selection strategy for testing that just selects the first server
pub fn test_srvr_sel(_: &ServerInfo) -> bool {
    true
}

/// Reads replies to our broadcast ping for the entire given duration
async fn read_replies_for(
    sock: &mut UdpSocket,
    duration: Duration,
) -> Result<Vec<(Vec<u8>, SocketAddr)>, Error> {
    // Wait for broadcast from peer
    let mut buf = vec![0u8; 100];
    let mut retme = vec![];
    timeout(duration, async {
        loop {
            match sock.recv_from(&mut buf).await {
                Ok((_, peer)) => {
                    let cur_content = buf.to_vec();
                    retme.push((cur_content, peer));
                }
                Err(e) => return Result::<(), _>::Err(e),
            }
        }
    })
    .await
    .unwrap_or(Ok(()))?;
    Ok(retme)
}
