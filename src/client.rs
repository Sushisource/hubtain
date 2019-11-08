use crate::encrypted_stream::EncryptedStreamStarter;
use crate::{filewriter::AsyncFileWriter, models::HandshakeReply, BROADCAST_ADDR, LOG};
use anyhow::{anyhow, Error};
use bincode::deserialize;
use futures::{
    compat::Future01CompatExt, io::AsyncReadExt, select, AsyncWrite, FutureExt as OFutureExt,
    TryFutureExt,
};
use indicatif::ProgressBar;
use rand::rngs::OsRng;
use runtime::net::{TcpStream, UdpSocket};
use std::fs::OpenOptions;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{atomic::AtomicUsize, atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::{prelude::FutureExt, timer::Delay};
use x25519_dalek::EphemeralSecret;
use crate::server::ClientApprovalStrategy;

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
        info!(LOG, "Client broadcasting to {}", &broadcast_addr);
        let mut client_s = UdpSocket::bind("0.0.0.0:0")?;
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
                    server_info.server_name,
                    tcp_sock_addr,
                    server_info.data_length,
                    server_info.encrypted,
                ))
            })
            .collect();
        let replies = replies?;
        info!(LOG, "Client found the following servers: {:?}", &replies);
        if replies.is_empty() {
            return Err(anyhow!("No servers found"));
        }
        // Connect to the tcp port
        let selected_server = replies.into_iter().find(server_selection_strategy).unwrap();
        let stream = TcpStream::connect(selected_server.addr).await?;
        info!(
            LOG,
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
    pub async fn download_to_file(self, path: PathBuf) -> Result<(), Error> {
        let path = if path.is_dir() {
            path.join(&self.server_info.name)
        } else {
            path
        };

        //        if path.exists() {
        //            // TODO: Allow override
        //            return Err(anyhow!("Refusing to overwrite existing file!"));
        //        }

        let path = path.as_path();
        info!(LOG, "Downloading to {:?}", path.as_os_str());
        let data_len = self.server_info.data_len;

        let mut as_fwriter = AsyncFileWriter::new(path)?;
        let bytes_written_ref = as_fwriter.bytes_writen.clone();
        let mut progress_fut = progress_counter(bytes_written_ref, data_len).boxed().fuse();
        let mut download_fut = self
            .drain_downloaded_to_stream(&mut as_fwriter)
            .boxed()
            .fuse();

        select! {
            _ = progress_fut => (),
            _ = download_fut => ()
        };
        drop(download_fut);

        info!(
            LOG,
            "Downloaded {} bytes",
            as_fwriter.bytes_writen.load(Ordering::Relaxed),
        );
        drop(as_fwriter);
        // Truncate any extra padding that may have been read
        OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(data_len)?;
        info!(LOG, "...done!");
        Ok(())
    }

    /// downloads server data to a vec
    #[cfg(test)]
    pub async fn download_to_vec(self) -> Result<Vec<u8>, Error> {
        let mut download = Vec::with_capacity(2056);
        let data_len = self.server_info.data_len as usize;
        dbg!(&data_len);
        self.drain_downloaded_to_stream(&mut download).await?;
        // Truncate any extra padding that may have been read
        download.split_off(data_len);
        Ok(download)
    }

    async fn drain_downloaded_to_stream<T>(mut self, mut download: &mut T) -> Result<u64, Error>
    where
        T: AsyncWrite + Unpin,
    {
        if self.server_info.encrypted {
            let mut rng = OsRng::new().unwrap();
            let secret = EphemeralSecret::new(&mut rng);
            let enc_stream = EncryptedStreamStarter::new(&mut self.stream, secret);
            info!(LOG, "Client encrypytion handshaking");
            let enc_stream = enc_stream.key_exchange(ClientApprovalStrategy::ApproveAll).await?;

            enc_stream.copy_into(&mut download).await
        } else {
            self.stream.copy_into(download).await
        }
        .map_err(Into::into)
    }
}

async fn progress_counter(progress: Arc<AtomicUsize>, total_size: u64) {
    let pbar = ProgressBar::new(total_size);
    loop {
        // The delay works best first to avoid printing a 0 for no reason
        let _ = Delay::new(Instant::now() + Duration::from_millis(100))
            .compat()
            .await;
        let bytes_read = progress.as_ref().load(Ordering::Relaxed);
        pbar.set_position(bytes_read as u64);
    }
}

#[derive(new, Debug)]
/// Identifying information of a server discovered on the network
pub struct ServerInfo {
    name: String,
    addr: SocketAddr,
    data_len: u64,
    encrypted: bool,
}

#[cfg(test)]
/// Server selection strategy for testing that just selects the first server
pub fn test_srvr_sel(_: &ServerInfo) -> bool {
    true
}

// TODO: Don't need to bother keeping this so generic. Move inside impl and only accept one
//   reply per socket addr. Return map instead.
async fn read_replies_for(
    sock: &mut UdpSocket,
    duration: Duration,
) -> Result<Vec<(Vec<u8>, SocketAddr)>, Error> {
    // Wait for broadcast from peer
    let mut buf = vec![0u8; 100];
    let mut retme = vec![];
    loop {
        match sock
            .recv_from(&mut buf)
            .compat()
            .timeout(duration)
            .compat()
            .await
        {
            Ok((_, peer)) => {
                let cur_content = buf.to_vec();
                retme.push((cur_content, peer));
            }
            Err(e) => {
                if e.is_elapsed() {
                    break;
                }
                return Err(e.into());
            }
        }
    }
    Ok(retme)
}
