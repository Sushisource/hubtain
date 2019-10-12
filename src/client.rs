use crate::encrypted_stream::EncryptedStream;
use crate::{filewriter::AsyncFileWriter, models::HandshakeReply, BROADCAST_ADDR, LOG};
use bincode::deserialize;
use failure::{err_msg, Error};
use futures::{
    compat::Future01CompatExt, io::AsyncReadExt, select, FutureExt as OFutureExt, TryFutureExt,
};
use indicatif::ProgressBar;
use runtime::net::{TcpStream, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{
    net::SocketAddr, path::PathBuf, sync::atomic::AtomicUsize, time::Duration, time::Instant,
};
use tokio::{prelude::FutureExt, timer::Delay};

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
            return Err(err_msg("No servers found"));
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

        if path.exists() {
            // TODO: Allow override
            return Err(err_msg("Refusing to overwrite existing file!"));
        }

        let path = path.as_path();
        info!(LOG, "Downloading to {:?}", path.as_os_str());

        let mut as_fwriter = AsyncFileWriter::new(path)?;

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
    pub async fn download_to_vec(&mut self) -> Result<Vec<u8>, ClientErr> {
        let mut download = Vec::with_capacity(2056);
        // TODO: This will need to be deduped with how it'll work in download_to_file
        let mut oss_stream = EncryptedStream::new(&mut self.stream);
        info!(LOG, "Client handshaking");
        oss_stream.handshake().await?;
        //        loop {
        let bytes_read = oss_stream.read_to_end(&mut download).await?;
        dbg!(bytes_read);
        //            if bytes_read > 0 {
        //                break
        //            }
        //            // TODO: Don't do this crap. Do check bytes read==expected.
        //            let _ = Delay::new(Instant::now() + Duration::from_millis(100))
        //                .compat()
        //                .await;
        //        }
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
        pbar.set_position(bytes_read as u64);
    }
}

#[derive(Debug, Fail)]
pub enum ClientErr {
    // TODO: Clean / snafu / whatever
    #[fail(display = "IOerr")]
    IOErr(std::io::Error),
}

impl From<std::io::Error> for ClientErr {
    fn from(e: std::io::Error) -> Self {
        ClientErr::IOErr(e)
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
