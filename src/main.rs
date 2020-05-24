#![feature(test, async_closure, fixed_size_array)]

#[macro_use]
extern crate clap;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate derive_more;
#[macro_use]
extern crate log;
#[cfg(test)]
extern crate test;

mod broadcast_addr_picker;
mod client;
mod encrypted_stream;
mod filereader;
mod filewriter;
mod igd;
mod mnemonic;
mod models;
mod progresswriter;
mod server;
mod tui;

use crate::{
    client::DownloadClient, filereader::AsyncFileReader, server::FileSrvBuilder,
    tui::init_console_logger,
};
use anyhow::{anyhow, Error};
#[cfg(not(test))]
use broadcast_addr_picker::select_broadcast_addr;
use clap::AppSettings;
use colored::Colorize;
#[cfg(test)]
use std::net::Ipv4Addr;
use std::{net::IpAddr, path::PathBuf};

#[cfg(not(test))]
lazy_static! {
    static ref BROADCAST_ADDR: IpAddr = select_broadcast_addr().unwrap();
}

#[cfg(test)]
lazy_static! {
    static ref BROADCAST_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
}

#[async_std::main]
async fn main() -> Result<(), Error> {
    let matches = clap_app!(hubtain =>
        (version: "0.1")
        (author: "Spencer Judge")
        (about: "Simple local file transfer server and client")
        (@subcommand srv =>
            (about: "Server mode")
            (@arg FILE: +required "The file to serve")
            (@arg no_encryption: -n --("no-encryption") "Disable encryption")
            (@arg stayalive: -s --stayalive "Server stays alive indefinitely rather than stopping \
                                             after serving one file")
            (@arg igd: -g --igd "Uses the UPnP IGD protocol to ask your router to map an external \
                                 address and port back to this computer. Likely to fail if you \
                                 are more than one hop away from the router.")
        )
        (@subcommand fetch =>
            (about: "Client download mode")
            (@arg SERVER_URL: "Rather than searching for a local server, directly connect to the \
                               provided IP/DNS and port combination")
            (@arg file: -o "Where to save the downloaded file. Defaults to a file in the current \
                            directory, named by the server it's downloaded from.")
        )
    )
    .settings(&[AppSettings::SubcommandRequiredElseHelp])
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
            let file_path = sc.value_of("FILE").expect("file arg is required");
            let serv_file = AsyncFileReader::new(file_path)?;
            let encryption = !sc.is_present("no_encryption");
            let fsrv = FileSrvBuilder::new(serv_file)
                .set_udp_port(42444)
                .set_stayalive(sc.is_present("stayalive"))
                .set_encryption(encryption)
                .set_igd(sc.is_present("igd"))
                .build()
                .await?;
            fsrv.serve().await?;
        }
        ("fetch", Some(sc)) => {
            // TODO: Interactive server selector? tui?
            init_console_logger();
            let client = if let Some(server_url) = sc.value_of("SERVER_URL") {
                DownloadClient::connect(server_url).await?
            } else {
                DownloadClient::find_server(42444, |_| true).await?
            };
            let file_path = sc
                .value_of("FILE")
                .map(Into::into)
                .unwrap_or_else(|| PathBuf::from("."));
            client.download_to_file(file_path).await?;
            info!("Download complete!");
        }
        _ => return Err(anyhow!("Unmatched subcommand")),
    }
    Ok(())
}

#[cfg(test)]
mod main_test {
    use super::*;
    use crate::{
        client::test_srvr_sel,
        filereader::AsyncFileReader,
        server::{test_filesrv, FileSrvBuilder},
    };
    use async_std::task::spawn;
    use std::{fs::File, io::Read};
    use tempfile::NamedTempFile;

    static TEST_DATA: &[u8] = b"Hi I'm data";

    #[async_std::test]
    async fn basic_transfer() {
        let fsrv = test_filesrv(TEST_DATA, false).await;
        let udp_port = fsrv.udp_port().unwrap();
        let server_f = spawn(fsrv.serve());

        let client = DownloadClient::find_server(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let content = client.download_to_vec().await.unwrap();
        assert_eq!(content, TEST_DATA);
        server_f.await.unwrap();
    }

    #[async_std::test]
    async fn encrypted_transfer() {
        let fsrv = test_filesrv(TEST_DATA, true).await;
        let udp_port = fsrv.udp_port().unwrap();
        let server_f = spawn(fsrv.serve());

        let client = DownloadClient::find_server(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let content = client.download_to_vec().await.unwrap();
        server_f.await.unwrap();
        assert_eq!(content, TEST_DATA);
    }

    #[async_std::test]
    async fn single_small_file_transfer() {
        file_transfer_test(false).await;
    }

    #[async_std::test]
    async fn single_small_encrypted_file_transfer() {
        file_transfer_test(true).await;
    }

    async fn file_transfer_test(encryption: bool) {
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        let fsrv = FileSrvBuilder::new(test_file)
            .set_encryption(encryption)
            .build()
            .await
            .unwrap();
        let udp_port = fsrv.udp_port().unwrap();
        let server_f = spawn(fsrv.serve());
        let client = DownloadClient::find_server(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let download_to = NamedTempFile::new().unwrap();
        client
            .download_to_file(download_to.path().to_path_buf())
            .await
            .unwrap();
        let mut expected_dat = vec![];
        File::open("testdata/small.txt")
            .unwrap()
            .read_to_end(&mut expected_dat)
            .unwrap();
        let mut test_dat = vec![];
        File::open(download_to.path())
            .unwrap()
            .read_to_end(&mut test_dat)
            .unwrap();
        assert_eq!(expected_dat, test_dat);
        server_f.await.unwrap();
    }

    #[async_std::test]
    async fn multiple_small_transfer() {
        let test_file = AsyncFileReader::new("testdata/small.txt").unwrap();
        let fsrv = FileSrvBuilder::new(test_file)
            .set_stayalive(true)
            .build()
            .await
            .unwrap();
        let udp_port = fsrv.udp_port().unwrap();
        let _ = spawn(fsrv.serve());

        let dl_futures = (1..25).map(async move |_| {
            let client = DownloadClient::find_server(udp_port, test_srvr_sel)
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
    #[async_std::test]
    async fn large_file_transfer() {
        use std::time::Instant;

        let test_file = AsyncFileReader::new("testdata/large.bin").unwrap();
        let fsrv = FileSrvBuilder::new(test_file)
            .set_encryption(true)
            .build()
            .await
            .unwrap();
        let udp_port = fsrv.udp_port().unwrap();
        let _ = spawn(fsrv.serve());

        let client = DownloadClient::find_server(udp_port, test_srvr_sel)
            .await
            .unwrap();
        let start = Instant::now();
        client
            .download_to_file("testdata/tmpdownload".into())
            .await
            .unwrap();
        dbg!("Finished transfer", start.elapsed());

        dbg!("Loading file");
        let mut test_dat = vec![];
        File::open("testdata/tmpdownload")
            .unwrap()
            .read_to_end(&mut test_dat)
            .unwrap();
        let content = include_bytes!("../testdata/large.bin");
        assert!(content.as_ref() == test_dat.as_slice());
    }
}
