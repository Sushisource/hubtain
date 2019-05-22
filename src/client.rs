use crate::filewriter::AsyncFileWriter;
use bincode::deserialize;
use failure::Error;
use futures::{compat::Future01CompatExt, io::AsyncReadExt, TryFutureExt};
use runtime::net::{TcpStream, UdpSocket};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::prelude::FutureExt;

use crate::{BROADCAST_ADDR, LOG};

/// Client for hubtain's fetch mode
pub struct DownloadClient {
    stream: TcpStream,
}

impl DownloadClient {
    /// Attempt to connect to a server, if successful, the returned client can be used to
    /// download the data.
    pub async fn connect<SrvPred>(
        udp_port: u16,
        server_selection_strategy: SrvPred,
    ) -> Result<Self, Error>
    where
        SrvPred: FnMut(&ServerId) -> bool,
    {
        let broadcast_addr = SocketAddr::new(*BROADCAST_ADDR, udp_port);
        info!(LOG, "Client broadcasting to {}", &broadcast_addr);
        let mut client_s = UdpSocket::bind("0.0.0.0:0")?;
        client_s.set_broadcast(true)?;
        client_s.send_to(b"I'm a client!", broadcast_addr).await?;
        let replies = read_replies_for(&mut client_s, Duration::from_secs(1)).await?;
        let replies: Result<Vec<ServerId>, Error> = replies
            .into_iter()
            .map(|(bytes, peer)| {
                let (name, tcp_port): (String, u16) = deserialize(&bytes)?;
                let mut tcp_sock_addr = peer;
                tcp_sock_addr.set_port(tcp_port);
                Ok(ServerId::new(name, tcp_sock_addr))
            })
            .collect();
        let replies = replies?;
        info!(LOG, "Client found the following servers: {:?}", &replies);
        // Connect to the tcp port
        let selected_server = replies.into_iter().find(server_selection_strategy).unwrap();
        let stream = TcpStream::connect(selected_server.addr).await?;
        info!(LOG, "Client connected to server!");
        Ok(DownloadClient { stream })
    }

    /// Tell the client to download the server's data to the provided location on disk
    pub async fn download_to_file(&mut self, path: PathBuf) -> Result<(), Error> {
        info!(LOG, "Starting download!");
        let mut as_fwriter = AsyncFileWriter::new(path.as_path())?;
        self.stream.copy_into(&mut as_fwriter).await?;
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

#[derive(new, Debug)]
/// Identifying information of a server discovered on the network
pub struct ServerId {
    name: String,
    addr: SocketAddr,
}

#[cfg(test)]
/// Server selection strategy for testing that just selects the first server
pub fn test_srvr_sel(_: &ServerId) -> bool {
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
