#![feature(async_await, await_macro, test)]
#![cfg_attr(test, allow(unused_imports))]

extern crate futures;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate slog;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate derive_new;

mod broadcast_addr_picker;
mod client;
mod filereader;
mod filewriter;
mod mnemonic;
mod server;

use crate::client::DownloadClient;
use crate::filereader::AsyncFileReader;
use crate::server::FileSrv;
use broadcast_addr_picker::select_broadcast_addr;
use clap::AppSettings;
use colored::Colorize;
use failure::Error;
use runtime::net::{TcpListener, UdpSocket};
use slog::Drain;
use std::net::IpAddr;
#[cfg(test)]
use std::net::Ipv4Addr;
use std::time::Duration;

#[cfg(not(test))]
lazy_static! {
    pub static ref LOG: slog::Logger = {
        let decorator = slog_term::TermDecorator::new().build();
        let drain = slog_term::CompactFormat::new(decorator).build().fuse();
        let drain = slog_async::Async::new(drain).build().fuse();
        slog::Logger::root(drain, o!())
    };
    static ref BROADCAST_ADDR: IpAddr = select_broadcast_addr().unwrap();
}

#[cfg(test)]
lazy_static! {
    static ref LOG: slog::Logger = {
        let decorator = slog_term::PlainDecorator::new(slog_term::TestStdoutWriter);
        let drain = slog_term::CompactFormat::new(decorator).build().fuse();
        let drain = slog_async::Async::new(drain).build().fuse();
        slog::Logger::root(drain, o!())
    };
    static ref BROADCAST_ADDR: IpAddr = { IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)) };
}

#[runtime::main(runtime_tokio::Tokio)]
async fn main() -> Result<(), Error> {
    let matches = clap_app!(hubtain =>
        (version: "0.1")
        (author: "Spencer Judge")
        (about: "Simple local file transfer server and client")
        (@subcommand srv =>
            (about: "Server mode")
            (@arg FILE: +required "The file to serve")
            (@arg stayalive: -s --stayalive "Server stays alive indefinitely rather than stopping after serving one file")
        )
        (@subcommand fetch =>
            (about: "Client download mode")
        )
    )
    .setting(AppSettings::SubcommandRequiredElseHelp)
    .get_matches();

    // Gotta have that sweet banner
    eprintln!(
        r#"
     _           _     _        _
    | |__  _   _| |__ | |_ __ _(_)_ __
    | '_ \| | | | '_ \| __/ _` | | '_ \
    | | | | |_| | |_) | || (_| | | | | |
    |_| |_|\__,_|_.__/ \__\__,_|_|_| |_|

       local file transfers made {}

    "#,
        "easy".green()
    );

    match matches.subcommand() {
        ("srv", Some(sc)) => {
            let tcp_sock = TcpListener::bind("0.0.0.0:0")?;
            let udp_sock = UdpSocket::bind(udp_srv_bind_addr(42444))?;
            let file_path = sc.value_of("FILE").unwrap();
            info!(LOG, "Serving file {}", &file_path);
            let serv_file = AsyncFileReader::new(file_path)?;
            let server = FileSrv::new(udp_sock, tcp_sock, serv_file, sc.is_present("stayalive"));
            server.serve().await?;
        }
        ("fetch", Some(_)) => {
            // TODO: Interactive server selector
            let mut client = DownloadClient::connect(42444, |_| true).await?;
            client.download_to_file("download".into()).await?;
            info!(LOG, "Download complete!");
        }
        _ => bail!("Unmatched subcommand"),
    }
    // Wait a beat to finish printing any async logging
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

#[cfg(target_family = "windows")]
#[cfg(not(test))]
fn udp_srv_bind_addr(port_num: usize) -> String {
    format!("0.0.0.0:{}", port_num)
}
#[cfg(target_family = "unix")]
#[cfg(not(test))]
fn udp_srv_bind_addr(port_num: usize) -> String {
    format!("192.168.0.255:{}", port_num)
}
#[cfg(test)]
fn udp_srv_bind_addr(port_num: usize) -> String {
    format!("127.0.0.1:{}", port_num)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::test_srvr_sel;
    use crate::filereader::AsyncFileReader;
    use runtime::spawn;
    use std::{fs::File, io::Read, time::Instant};

    static TEST_DATA: &[u8] = b"Hi I'm data";

    #[runtime::test(runtime_tokio::Tokio)]
    async fn basic_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let server = FileSrv::new(udp_sock, tcp_sock, TEST_DATA, false);
        spawn(server.serve());

        let mut client = DownloadClient::connect(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let content = client.download_to_vec().await.unwrap();
        assert_eq!(content, TEST_DATA);
    }

    #[runtime::test(runtime_tokio::Tokio)]
    async fn single_small_file_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        let server = FileSrv::new(udp_sock, tcp_sock, test_file, false);
        spawn(server.serve());

        let mut client = DownloadClient::connect(udp_port, test_srvr_sel)
            .await
            .unwrap();
        client
            .download_to_file("testdata/tmp.small.txt".into())
            .await
            .unwrap();

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

    #[runtime::test(runtime_tokio::Tokio)]
    async fn multiple_small_file_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        let server = FileSrv::new(udp_sock, tcp_sock, test_file, true);
        spawn(server.serve());

        let dl_futures = (1..100).map(async move |_| {
            let mut client = DownloadClient::connect(udp_port, test_srvr_sel)
                .await
                .unwrap();
            client.download_to_vec().await
        });
        let contents = futures::future::try_join_all(dl_futures).await.unwrap();

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
    #[runtime::test(runtime_tokio::Tokio)]
    async fn large_file_transfer() {
        let tcp_sock = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp_sock.local_addr().unwrap().port();
        let test_file = AsyncFileReader::new("testdata/large.bin").unwrap();
        let server = FileSrv::new(udp_sock, tcp_sock, test_file, false);
        spawn(server.serve());

        let mut client = DownloadClient::connect(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let start = Instant::now();
        dbg!("Began transfer");
        let content = client.download_to_vec().await.unwrap();
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
