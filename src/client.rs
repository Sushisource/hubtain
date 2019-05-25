use crate::{filewriter::AsyncFileWriter, models::HandshakeReply, BROADCAST_ADDR, LOG};
use bincode::deserialize;
use failure::Error;
use futures::{compat::Future01CompatExt, io::AsyncReadExt, FutureExt as OFutureExt, TryFutureExt};
use runtime::net::{TcpStream, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{
    net::SocketAddr, path::PathBuf, sync::atomic::AtomicUsize, time::Duration, time::Instant,
};
use tokio::{prelude::FutureExt, timer::Delay};
use indicatif::ProgressBar;

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
        client_s.send_to(b"I'm a client!", broadcast_addr).await?;
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
                ))
            })
            .collect();
        let replies = replies?;
        info!(LOG, "Client found the following servers: {:?}", &replies);
        // Connect to the tcp port
        let selected_server = replies.into_iter().find(server_selection_strategy).unwrap();
        let stream = TcpStream::connect(selected_server.addr).await?;
        info!(LOG, "Client connected to server!");
        Ok(DownloadClient {
            stream,
            server_info: selected_server,
        })
    }

    /// Tell the client to download the server's data to the provided location on disk
    pub async fn download_to_file(&mut self, path: PathBuf) -> Result<(), Error> {
        info!(LOG, "Starting download!");

        let mut as_fwriter = AsyncFileWriter::new(path.as_path())?;

        let bytes_written_ref = as_fwriter.bytes_writen.clone();
        let mut progress_fut = progress_counter(bytes_written_ref, self.server_info.data_len)
            .boxed()
            .fuse();
        let mut download_fut = self.stream.copy_into(&mut as_fwriter).fuse();

        select! {
            _ = progress_fut => (),
            _ = download_fut => ()
        };

        info!(
            LOG,
            "Downloaded {} bytes",
            as_fwriter.bytes_writen.load(Ordering::Relaxed),
        );
        info!(LOG, "...done!");
        Ok(())
    }

    /// For testing - downloads server data to a vec
    #[cfg(test)]
    pub async fn download_to_vec(&mut self) -> std::io::Result<Vec<u8>> {
        let mut download = Vec::with_capacity(2056);
        self.stream.read_to_end(&mut download).await?;
        Ok(download)
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
//        info!(
//            LOG,
//            "got {}/{} bytes",
//            bytes_read,
//            total_size
//        );
        pbar.set_position(bytes_read as u64);
    }
}

#[derive(new, Debug)]
/// Identifying information of a server discovered on the network
pub struct ServerInfo {
    name: String,
    addr: SocketAddr,
    data_len: u64,
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
