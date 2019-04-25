#![feature(async_await, await_macro, test)]

mod client;
mod filereader;

use bincode::serialize;
use failure::Error;
use futures::io::{AsyncRead, AsyncReadExt};
use futures01::Future as Future01;
use runtime::{
    net::{TcpListener, UdpSocket},
    spawn,
};
use std::{
    time::{Duration, Instant},
};
use tokio::timer::Delay;

static TEST_DATA: &[u8] = b"Hi I'm data";

#[runtime::main]
async fn main() -> Result<(), Error> {
    let tcp_sock = TcpListener::bind("127.0.0.1:0")?;
    let udp_sock = UdpSocket::bind("127.0.0.1:42444")?;
    await!(serve(tcp_sock, udp_sock, TEST_DATA))?;
    Ok(())
}

async fn serve<T: 'static + AsyncRead + Send + Unpin>(
    tcp_sock: TcpListener,
    mut socket: UdpSocket,
    data_src: T,
) -> Result<(), Error> {
    let tcp_port = tcp_sock.local_addr()?.port();

    socket.set_broadcast(true)?;
    println!("Listening on {}", socket.local_addr()?);

    spawn(data_srv(tcp_sock, data_src));

    // Wait for broadcast from peer
    let mut buf = vec![0u8; 100];
    loop {
        let (_, peer) = await!(socket.recv_from(&mut buf)).unwrap();
        println!("Got client handshake from {}", &peer);
        // Reply with tcp portnum
        let portnum = serialize(&tcp_port)?;
        await!(socket.send_to(&portnum, &peer))?;

        let sleep = Delay::new(Instant::now() + Duration::from_millis(300))
          .map_err(|e| panic!("timer failed; err={:?}", e));
        tokio::run(sleep);
    }
}

async fn data_srv<T: AsyncRead + Unpin>(
    mut listener: TcpListener,
    mut data_src: T,
) -> Result<(), Error> {
    println!("TCP listening on {}", listener.local_addr()?);
    let (mut stream, addr) = await!(listener.accept())?;
    println!("Accepted connection from {:?}", &addr);
    await!(data_src.copy_into(&mut stream))?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::DownloadClient;
    use crate::filereader::AsyncFileReader;
    use std::fs::File;
    use std::io::Read;

    #[runtime::test]
    async fn basic_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        spawn(serve(tcp_sock, udp_sock, TEST_DATA));

        let mut client = await!(DownloadClient::connect(udp_port)).unwrap();
        let content = await!(client.download_to_vec()).unwrap();
        assert_eq!(content, TEST_DATA);
    }

    #[runtime::test]
    async fn file_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        spawn(serve(tcp_sock, udp_sock, test_file));

        let mut client = await!(DownloadClient::connect(udp_port)).unwrap();
        let content = await!(client.download_to_vec()).unwrap();

        let mut test_dat = vec![];
        File::open("testdata/small.txt").unwrap().read_to_end(&mut test_dat).unwrap();
        assert_eq!(content, test_dat);
    }

    #[cfg(expensive_tests)]
    #[runtime::test]
    async fn large_file_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let test_file = AsyncFileReader::new("testdata/large.bin").unwrap();
        spawn(serve(tcp_sock, udp_sock, test_file));

        let mut client = await!(DownloadClient::connect(udp_port)).unwrap();
        let start = Instant::now();
        dbg!("Began transfer");
        let content = await!(client.download_to_vec()).unwrap();
        dbg!("Finished transfer after {:?}", start.elapsed());

        let start = Instant::now();
        dbg!("Loading file");
        let mut test_dat = vec![];
        File::open("testdata/large.bin").unwrap().read_to_end(&mut test_dat).unwrap();
        dbg!("Done loading file after {:?}", start.elapsed());
        assert_eq!(content, test_dat);
    }
}
