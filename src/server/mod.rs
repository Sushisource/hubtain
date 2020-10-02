mod client_approver;
mod ngrok;
mod tui;

pub use client_approver::{ClientApprover, ConsoleApprover};

use self::tui::ServerTui;
use crate::{
    encrypted_stream::{EncStreamErr, ServerEncryptedStreamStarter},
    filereader::AsyncFileReader,
    igd::get_external_addr,
    mnemonic::random_word,
    models::{DataSrvInfo, DiscoveryReply},
    tui::{init_console_logger, TuiApprover},
};
use anyhow::{anyhow, Context, Error};
use async_std::{
    io,
    net::{TcpListener, UdpSocket},
    task::spawn,
};
use bincode::serialize;
use futures::{io::AsyncRead, AsyncWrite};
use std::{net::SocketAddr, path::PathBuf, time::Duration};

use crate::server::ngrok::get_tunnel;
#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, Ordering};

/// Can be set true to order the server to quit. Only used for TUI purposes.
#[cfg(not(test))]
pub static SHUTDOWN_FLAG: AtomicBool = AtomicBool::new(false);

/// File server for hubtain's srv mode
pub struct FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    stay_alive: bool,
    udp_sock: UdpSocket,
    tcp_sock: TcpListener,
    data: T,
    data_length: u64,
    name: String,
    encrypted: bool,
    client_approval_strategy: ClientApprovalStrategy,
    file_name: String,
    holepuncher: HolePuncher,
}

impl<T> FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    /// Begin listening for connections and serving data
    ///
    /// ## Also:
    /// * Initialize the TUI if the terminal is a tty & using encryption & staying alive
    pub async fn serve(self) -> Result<(), Error> {
        let approver: Box<dyn ClientApprover>;

        let tui_handle = if self.client_approval_strategy == ClientApprovalStrategy::Interactive
            && self.stay_alive
        {
            if !ServerTui::is_interactive() {
                return Err(anyhow!(
                    "Refusing to use interactive mode in non-tty terminal"
                ));
            }
            let tui_handle = ServerTui::start(self.name.clone(), self.file_name.clone())?;
            approver = Box::new(TuiApprover::new(tui_handle.tx.clone()));
            Some(tui_handle)
        } else {
            init_console_logger();
            approver = Box::new(ConsoleApprover::default());
            None
        };

        info!("Serving file {}", &self.file_name);

        let tcp_addr = self.tcp_sock.local_addr()?;

        let mut ngrok_handle = None;
        let mut igd_handle = None;
        match self.holepuncher {
            HolePuncher::None => (),
            HolePuncher::IGD => {
                if let SocketAddr::V4(s) = &tcp_addr {
                    let external_addr = get_external_addr(s.port())?;
                    igd_handle = Some(external_addr);
                } else {
                    return Err(anyhow!("Couldn't determine local address during IGD"));
                }
            }
            HolePuncher::Ngrok => {
                // Ask for a tunnel to our tcp port
                if let SocketAddr::V4(s) = &tcp_addr {
                    let ngrok = get_tunnel(s.port()).await?;
                    info!(
                        "Obtained external address, share this to your downloader: {}",
                        ngrok.get_address()
                    );
                    ngrok_handle = Some(ngrok);
                } else {
                    return Err(anyhow!("Couldn't determine local address for ngrok to use"));
                }
            }
        };

        let tcp_port = tcp_addr.port();

        self.udp_sock.set_broadcast(true)?;
        info!("UDP Listening on {}", self.udp_sock.local_addr()?);

        let data_handle = spawn(FileSrv::data_srv(
            self.tcp_sock,
            ServerData {
                data: self.data,
                data_length: self.data_length,
                enctype: if self.encrypted {
                    EncryptionType::Ephemeral
                } else {
                    EncryptionType::None
                },
                data_name: self.file_name,
            },
            self.stay_alive,
            self.client_approval_strategy,
            Box::leak(approver),
        ));

        // Wait for broadcast from peer
        let mut buf = vec![0u8; 2];
        loop {
            #[cfg(not(test))]
            if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
                data_handle.await?;
                break;
            }
            let (_, peer) = match io::timeout(
                Duration::from_millis(100),
                self.udp_sock.recv_from(&mut buf),
            )
            .await
            {
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    // Need to keep checking the shutdown flag. Probably a more "future"y
                    // way to do this
                    continue;
                }
                r => r,
            }?;
            info!("Client ping from {}", &peer);
            // Reply with name and tcp portnum
            let initial_info = serialize(&DiscoveryReply {
                server_name: self.name.clone(),
                tcp_port,
            })?;
            self.udp_sock.send_to(&initial_info, &peer).await?;

            // Since we have offloaded the first client to the data server we don't care about
            // any other clients in no-stay-alive mode.
            if !self.stay_alive {
                data_handle.await?;
                break;
            }
        }

        if let Some(t) = tui_handle {
            t.join();
        }
        if let Some(h) = ngrok_handle {
            h.shutdown()?;
        }
        if let Some(h) = igd_handle {
            h.free()?;
        }

        #[cfg(not(test))]
        SHUTDOWN_FLAG.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Return the UDP port the server is bound to
    #[cfg(test)]
    pub fn udp_port(&self) -> Result<u16, Error> {
        Ok(self.udp_sock.local_addr()?.port())
    }

    /// The data srv runs independently of the main srv loop, and does the job of actually
    /// transferring data to clients.
    async fn data_srv(
        tcp_sock: TcpListener,
        server_data: ServerData<T>,
        stay_alive: bool,
        client_strat: ClientApprovalStrategy,
        approver: &'static dyn ClientApprover,
    ) -> Result<(), Error> {
        let local_addr = tcp_sock.local_addr()?;
        info!("TCP listening on {}", local_addr);
        loop {
            #[cfg(not(test))]
            if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
                break Ok(());
            }
            let (mut stream, addr) =
                match io::timeout(Duration::from_millis(100), tcp_sock.accept()).await {
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        continue;
                    }
                    r => r?,
                };
            info!("Accepted download connection from {:?}", &addr);
            // Send the client some info first
            let info = DataSrvInfo {
                tcp_port: local_addr.port(),
                data_length: server_data.data_length,
                encrypted: server_data.enctype != EncryptionType::None,
                data_name: server_data.data_name.clone(),
            };
            info.write_to_stream(&mut stream).await?;

            let data_src = server_data.data.clone();
            let enctype = server_data.enctype;
            let h = spawn(async move {
                let res = (|| async {
                    let mut extra = String::new();
                    let mut ec;
                    let mut wstream: &mut (dyn AsyncWrite + Unpin + Send) = match enctype {
                        EncryptionType::Ephemeral => {
                            info!("Server handshaking");
                            let enc_stream = ServerEncryptedStreamStarter::new(&mut stream);
                            let encrypted_stream =
                                match enc_stream.key_exchange(client_strat, approver).await {
                                    Ok(es) => es,
                                    Err(EncStreamErr::ClientNotAccepted) => {
                                        return Result::<_, anyhow::Error>::Ok(())
                                    }
                                    e => e?,
                                };
                            extra = format!("to client {}", encrypted_stream.get_client_id());
                            ec = encrypted_stream;
                            &mut ec
                        }
                        EncryptionType::None => &mut stream,
                    };
                    futures::io::copy(data_src, &mut wstream)
                        .await
                        .context(format!(
                            "I/O error while trying to transfer to client {}",
                            addr
                        ))?;
                    info!("Done serving {}", extra);
                    Ok(())
                })()
                .await;
                if let Err(e) = &res {
                    error!("{}", e);
                };
                res
            });
            if !stay_alive {
                // Assumes only one client matters in no-stay-alive mode.
                #[cfg(not(test))]
                SHUTDOWN_FLAG.store(true, Ordering::SeqCst);
                h.await?;
                return Ok(());
            }
        }
    }
}

struct ServerData<T> {
    data: T,
    data_length: u64,
    data_name: String,
    enctype: EncryptionType,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Copy)]
enum EncryptionType {
    None,
    Ephemeral,
}

#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub enum ClientApprovalStrategy {
    Interactive,
    ApproveAll,
}

pub struct FileSrvBuilder {
    data: AsyncFileReader,
    udp_port: u16,
    stay_alive: bool,
    encryption: bool,
    listen_addr: String,
    client_approval_strategy: ClientApprovalStrategy,
    file_path: PathBuf,
    holepunch: HolePuncher,
}

/// Method being used to punch a hole for an external address
pub enum HolePuncher {
    None,
    IGD,
    Ngrok,
}

#[cfg(not(test))]
const DEFAULT_TCP_LISTEN_ADDR: &str = "0.0.0.0";
#[cfg(test)]
const DEFAULT_TCP_LISTEN_ADDR: &str = "127.0.0.1";

#[cfg(target_family = "windows")]
#[cfg(not(test))]
fn udp_srv_bind_addr(port_num: u16) -> String {
    format!("0.0.0.0:{}", port_num)
}

// TODO: Make work with more than my gateway
#[cfg(target_family = "unix")]
#[cfg(not(test))]
fn udp_srv_bind_addr(port_num: u16) -> String {
    format!("192.168.1.255:{}", port_num)
}

#[cfg(test)]
fn udp_srv_bind_addr(port_num: u16) -> String {
    format!("127.0.0.1:{}", port_num)
}

impl FileSrvBuilder {
    pub fn new(data: AsyncFileReader) -> Self {
        FileSrvBuilder {
            file_path: data.orig_path.clone(),
            data,
            udp_port: 0,
            stay_alive: false,
            encryption: false,
            holepunch: HolePuncher::None,
            listen_addr: DEFAULT_TCP_LISTEN_ADDR.to_string(),
            client_approval_strategy: ClientApprovalStrategy::ApproveAll,
        }
    }

    pub fn set_udp_port(mut self, port: u16) -> Self {
        self.udp_port = port;
        self
    }

    pub fn set_stayalive(mut self, stayalive: bool) -> Self {
        self.stay_alive = stayalive;
        self
    }

    pub fn set_holepuncher(mut self, holepuncher: HolePuncher) -> Self {
        self.holepunch = holepuncher;
        self
    }

    pub fn set_encryption(mut self, encryption: bool) -> Self {
        self.encryption = encryption;
        #[cfg(not(test))]
        if encryption {
            self.client_approval_strategy = ClientApprovalStrategy::Interactive;
        }
        self
    }

    /// Build the file server, binding sockets in the process
    pub async fn build(self) -> Result<FileSrv<AsyncFileReader>, Error> {
        let tcp_sock = TcpListener::bind(format!("{}:0", &self.listen_addr)).await?;
        let udp_sock = UdpSocket::bind(udp_srv_bind_addr(self.udp_port)).await?;
        let name = random_word();
        if !self.file_path.is_file() {
            return Err(anyhow!("Provied path is not a file!"));
        }
        let file_name = self
            .file_path
            .file_name()
            .expect("just asserted it's a file")
            .to_str()
            .ok_or_else(|| anyhow!("Provied path has weird characters!"))?
            .to_string();
        Ok(FileSrv {
            stay_alive: self.stay_alive,
            udp_sock,
            tcp_sock,
            data_length: self.data.file_size,
            data: self.data,
            name: name.to_string(),
            encrypted: self.encryption,
            client_approval_strategy: self.client_approval_strategy,
            file_name,
            holepuncher: self.holepunch,
        })
    }
}

#[cfg(test)]
pub async fn test_filesrv(
    data: &'static [u8],
    encrypted: bool,
) -> FileSrv<impl AsyncRead + Send + Unpin + Clone> {
    let tcp_sock = TcpListener::bind(format!("{}:0", "127.0.0.1"))
        .await
        .unwrap();
    let udp_sock = UdpSocket::bind(udp_srv_bind_addr(0)).await.unwrap();
    let name = random_word();
    FileSrv {
        stay_alive: false,
        udp_sock,
        tcp_sock,
        data_length: data.len() as u64,
        data,
        name: name.to_string(),
        encrypted,
        client_approval_strategy: ClientApprovalStrategy::ApproveAll,
        holepuncher: HolePuncher::None,
        file_name: "not a real file".to_string(),
    }
}
