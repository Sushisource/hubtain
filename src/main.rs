#![feature(async_await, await_macro, test)]

extern crate futures;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate log;

mod client;
mod filereader;
mod filewriter;

use crate::client::DownloadClient;
use crate::filereader::AsyncFileReader;
use bincode::serialize;
use clap::AppSettings;
use failure::Error;
use futures::io::{AsyncRead, AsyncReadExt};
use runtime::{
    net::{TcpListener, UdpSocket},
    spawn,
};
use log::LevelFilter;
use colored::Colorize;

#[runtime::main]
async fn main() -> Result<(), Error> {
    env_logger::Builder::from_default_env().filter_level(LevelFilter::Info).init();

    // Gotta have that sweet banner
    eprintln!(r#"
     _           _     _        _
    | |__  _   _| |__ | |_ __ _(_)_ __
    | '_ \| | | | '_ \| __/ _` | | '_ \
    | | | | |_| | |_) | || (_| | | | | |
    |_| |_|\__,_|_.__/ \__\__,_|_|_| |_|

      local file transfers made {}

    "#, "easy".green());

    let matches = clap_app!(hubtain =>
        (version: "0.1")
        (author: "Spencer Judge")
        (about: "Simple local file transfer server and client")
        (@subcommand srv =>
            (about: "Server mode")
            (@arg FILE: +required "The file to serve")
        )
        (@subcommand fetch =>
            (about: "Client download mode")
        )
    )
    .setting(AppSettings::SubcommandRequiredElseHelp)
    .get_matches();
    match matches.subcommand() {
        ("srv", Some(sc)) => {
            let tcp_sock = TcpListener::bind("0.0.0.0:0")?;
            let udp_sock = UdpSocket::bind("192.168.0.255:42444")?;
            let file_path = sc.value_of("FILE").unwrap();
            info!("Serving file {}", &file_path);
            let serv_file = AsyncFileReader::new(file_path)?;
            await!(serve(tcp_sock, udp_sock, serv_file))?;
        }
        ("fetch", Some(_)) => {
            let mut client = await!(DownloadClient::connect(42444))?;
            await!(client.download_to_file("download".into()))?;
        }
        _ => bail!("Unmatched subcommand"),
    }
    Ok(())
}

async fn serve<T: 'static + AsyncRead + Send + Unpin + Clone>(
    tcp_sock: TcpListener,
    mut socket: UdpSocket,
    data_src: T,
) -> Result<(), Error> {
    let tcp_port = tcp_sock.local_addr()?.port();

    socket.set_broadcast(true)?;
    info!("UDP Listening on {}", socket.local_addr()?);

    spawn(data_srv(tcp_sock, data_src));

    // Wait for broadcast from peer
    let mut buf = vec![0u8; 100];
    loop {
        let (_, peer) = await!(socket.recv_from(&mut buf)).unwrap();
        info!("Got client handshake from {}", &peer);
        // Reply with tcp portnum
        let portnum = serialize(&tcp_port)?;
        await!(socket.send_to(&portnum, &peer))?;
    }
}

async fn data_srv<T: 'static + AsyncRead + Unpin + Send + Clone>(
    mut listener: TcpListener,
    data_src: T,
) -> Result<(), Error> {
    info!("TCP listening on {}", listener.local_addr()?);
    loop {
        let (mut stream, addr) = await!(listener.accept())?;
        info!("Accepted connection from {:?}", &addr);
        let mut data_src = data_src.clone();
        spawn(async move {
            info!("Copying data to stream!");
            await!(data_src.copy_into(&mut stream))
        });
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::filereader::AsyncFileReader;
    use std::fs::File;
    use std::io::Read;
    use std::time::Instant;
    static TEST_DATA: &[u8] = b"Hi I'm data";

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
    async fn single_small_file_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        spawn(serve(tcp_sock, udp_sock, test_file));

        let mut client = await!(DownloadClient::connect(udp_port)).unwrap();
        await!(client.download_to_file("testdata/tmp.small.txt".into())).unwrap();

        let mut expected_dat = vec![];
        File::open("testdata/small.txt")
            .unwrap()
            .read_to_end(&mut expected_dat)
            .unwrap();
        let mut test_dat = vec![];
        File::open("testdata/tmp.small.txt")
            .unwrap()
            .read_to_end(&mut test_dat)
            .unwrap();
        assert_eq!(expected_dat, test_dat);
        std::fs::remove_file("testdata/tmp.small.txt").unwrap();
    }

    #[runtime::test]
    async fn multiple_small_file_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        spawn(serve(tcp_sock, udp_sock, test_file));

        let dl_futures = (1..100).map(async move |_| {
            let mut client = await!(DownloadClient::connect(udp_port)).unwrap();
            await!(client.download_to_vec())
        });
        let contents = await!(futures::future::try_join_all(dl_futures)).unwrap();

        let mut test_dat = vec![];
        File::open("testdata/small.txt")
            .unwrap()
            .read_to_end(&mut test_dat)
            .unwrap();

        for content in contents {
            assert_eq!(content, test_dat);
        }
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
        File::open("testdata/large.bin")
            .unwrap()
            .read_to_end(&mut test_dat)
            .unwrap();
        dbg!("Done loading file after {:?}", start.elapsed());
        assert_eq!(content, test_dat);
    }
}
