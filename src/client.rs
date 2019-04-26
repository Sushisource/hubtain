use crate::filewriter::AsyncFileWriter;
use bincode::deserialize;
use failure::Error;
use futures::io::AsyncReadExt;
use runtime::net::{TcpStream, UdpSocket};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

pub struct DownloadClient {
    stream: TcpStream,
}

impl DownloadClient {
    pub async fn connect(udp_port: u16) -> Result<Self, Error> {
        let broadcast_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), udp_port);
        let mut client_s = UdpSocket::bind("127.0.0.1:0")?;
        client_s.set_broadcast(true)?;
        let mut buf = vec![0u8; 24];
        await!(client_s.send_to(b"I'm a client!", broadcast_addr))?;
        dbg!("ClientSent!");
        let (_, peer) = await!(client_s.recv_from(&mut buf))?;
        let tcp_port: u16 = deserialize(&buf)?;
        println!("Client found server tcp port {}", &tcp_port);
        let mut tcp_sock_addr = peer;
        tcp_sock_addr.set_port(tcp_port);
        // Connect to the tcp port
        let stream = await!(TcpStream::connect(tcp_sock_addr))?;
        Ok(DownloadClient { stream })
    }

    pub async fn download_to_file(&mut self, path: PathBuf) -> Result<(), Error> {
        println!("Starting download!");
        let mut as_fwriter = AsyncFileWriter::new(path.as_path())?;
        await!(self.stream.copy_into(&mut as_fwriter))?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn download_to_vec(&mut self) -> std::io::Result<Vec<u8>> {
        println!("Starting download!");
        let mut download = Vec::with_capacity(2056);
        await!(self.stream.read_to_end(&mut download))?;
        Ok(download)
    }
}
