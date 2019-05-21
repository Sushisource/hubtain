use crate::filewriter::AsyncFileWriter;
use bincode::deserialize;
use failure::Error;
use futures::io::AsyncReadExt;
use futures::{compat::Future01CompatExt, TryFutureExt};
use runtime::net::{TcpStream, UdpSocket};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::prelude::FutureExt;

use crate::{BROADCAST_ADDR, LOG};

pub struct DownloadClient {
    stream: TcpStream,
}

impl DownloadClient {
    pub async fn connect(udp_port: u16) -> Result<Self, Error> {
        let broadcast_addr = SocketAddr::new(*BROADCAST_ADDR, udp_port);
        info!(LOG, "Client broadcasting to {}", &broadcast_addr);
        let mut client_s = UdpSocket::bind("0.0.0.0:0")?;
        client_s.set_broadcast(true)?;
        client_s.send_to(b"I'm a client!", broadcast_addr).await?;
        let replies = read_replies_for(&mut client_s, Duration::from_secs(1)).await?;
        let replies: Result<Vec<(String, SocketAddr)>, Error> = replies
            .into_iter()
            .map(|(bytes, peer)| {
                let (name, tcp_port): (String, u16) = deserialize(&bytes)?;
                let mut tcp_sock_addr = peer;
                tcp_sock_addr.set_port(tcp_port);
                Ok((name, tcp_sock_addr))
            })
            .collect();
        let replies = replies?;
        info!(LOG, "Client found the following servers: {:?}", &replies);
        // Connect to the tcp port TODO: Select from list
        let stream = TcpStream::connect(replies[0].1).await?;
        info!(LOG, "Client connected to server!");
        Ok(DownloadClient { stream })
    }

    pub async fn download_to_file(&mut self, path: PathBuf) -> Result<(), Error> {
        info!(LOG, "Starting download!");
        let mut as_fwriter = AsyncFileWriter::new(path.as_path())?;
        self.stream.copy_into(&mut as_fwriter).await?;
        info!(LOG, "...done!");
        Ok(())
    }

    #[cfg(test)]
    pub async fn download_to_vec(&mut self) -> std::io::Result<Vec<u8>> {
        let mut download = Vec::with_capacity(2056);
        self.stream.read_to_end(&mut download).await?;
        Ok(download)
    }
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
