#![feature(async_await, test, async_closure)]

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
mod models;
mod server;

use crate::server::FileSrvBuilder;
use crate::{client::DownloadClient, filereader::AsyncFileReader};
#[cfg(not(test))]
use broadcast_addr_picker::select_broadcast_addr;
use clap::AppSettings;
use colored::Colorize;
use failure::Error;
use slog::Drain;
#[cfg(test)]
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::{net::IpAddr, time::Duration};

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
            (@arg stayalive: -s --stayalive "Server stays alive indefinitely rather than stopping \
                                             after serving one file")
        )
        (@subcommand fetch =>
            (about: "Client download mode")
            (@arg FILE: "Where to save the downloaded file. Defaults to a file in the current \
                         directory, named by the server it's downloaded from.")
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
            let file_path = sc.value_of("FILE").unwrap();
            info!(LOG, "Serving file {}", &file_path);
            let serv_file = AsyncFileReader::new(file_path)?;
            let file_siz = serv_file.file_size;
            let fsrv = FileSrvBuilder::new(serv_file, file_siz)
                .set_udp_port(42444)
                .set_stayalive(sc.is_present("stayalive"))
                .build()?;
            fsrv.serve().await?;
        }
        ("fetch", Some(sc)) => {
            // TODO: Interactive server selector
            let mut client = DownloadClient::connect(42444, |_| true).await?;
            let file_path = sc
                .value_of("FILE")
                .map(Into::into)
                .unwrap_or_else(|| PathBuf::from("."));
            client.download_to_file(file_path).await?;
            info!(LOG, "Download complete!");
        }
        _ => bail!("Unmatched subcommand"),
    }
    // Wait a beat to finish printing any async logging
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::test_srvr_sel;
    use crate::filereader::AsyncFileReader;
    use crate::server::FileSrvBuilder;
    use runtime::spawn;
    use std::{fs::File, io::Read};

    static TEST_DATA: &[u8] = b"Hi I'm data";

    #[runtime::test(runtime_tokio::Tokio)]
    async fn basic_transfer() {
        let fsrv = FileSrvBuilder::new(TEST_DATA, TEST_DATA.len() as u64)
            .build()
            .unwrap();
        let udp_port = fsrv.udp_port().unwrap();
        let server_f = spawn(fsrv.serve());

        let mut client = DownloadClient::connect(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let content = client.download_to_vec().await.unwrap();
        assert_eq!(content, TEST_DATA);
        server_f.await.unwrap();
    }

    #[runtime::test(runtime_tokio::Tokio)]
    async fn single_small_file_transfer() {
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        let file_siz = test_file.file_size;
        let fsrv = FileSrvBuilder::new(test_file, file_siz).build().unwrap();
        let udp_port = fsrv.udp_port().unwrap();
        let server_f = spawn(fsrv.serve());

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
        server_f.await.unwrap();
    }

    #[runtime::test(runtime_tokio::Tokio)]
    async fn multiple_small_file_transfer() {
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        let file_siz = test_file.file_size;
        let fsrv = FileSrvBuilder::new(test_file, file_siz)
            .set_stayalive(true)
            .build()
            .unwrap();
        let udp_port = fsrv.udp_port().unwrap();
        let _ = spawn(fsrv.serve());

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

    #[cfg(feature = "expensive_tests")]
    #[runtime::test(runtime_tokio::Tokio)]
    async fn large_file_transfer() {
        use std::time::Instant;

        let test_file = AsyncFileReader::new("testdata/large.bin").unwrap();
        let file_siz = test_file.file_size;
        let fsrv = FileSrvBuilder::new(test_file, file_siz).build().unwrap();
        let udp_port = fsrv.udp_port().unwrap();
        let _ = spawn(fsrv.serve());

        let mut client = DownloadClient::connect(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let start = Instant::now();
        dbg!("Began transfer");
        client
            .download_to_file("testdata/tmpdownload".into())
            .await
            .unwrap();
        dbg!("Finished transfer after {:?}", start.elapsed());

        let start = Instant::now();
        dbg!("Loading file");
        let mut test_dat = vec![];
        File::open("testdata/large.bin")
            .unwrap()
            .read_to_end(&mut test_dat)
            .unwrap();
        dbg!("Done loading file after {:?}", start.elapsed());
        // TODO: FIX
        //        assert_eq!(content, test_dat);
    }
}
