mod client_approver;
mod tui;

pub use client_approver::ClientApprover;

use self::tui::ServerTui;
use crate::{
    encrypted_stream::{EncStreamErr, ServerEncryptedStreamStarter},
    filereader::AsyncFileReader,
    mnemonic::random_word,
    models::HandshakeReply,
    server::client_approver::ConsoleApprover,
    tui::TuiApprover,
};
use anyhow::{anyhow, Context, Error};
use async_std::{
    io,
    net::{TcpListener, UdpSocket},
    task::spawn,
};
use bincode::serialize;
use futures::io::AsyncRead;
use rand::rngs::OsRng;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use x25519_dalek::EphemeralSecret;

/// Can be set true to order the server to quit.
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
}

impl<T> FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    /// Begin listening for connections and serving data
    ///
    /// Also:
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
            approver = Box::new(ConsoleApprover::default());
            None
        };

        let tcp_port = self.tcp_sock.local_addr()?.port();

        self.udp_sock.set_broadcast(true)?;
        info!("UDP Listening on {}", self.udp_sock.local_addr()?);

        let data_handle = spawn(FileSrv::data_srv(
            self.tcp_sock,
            self.data,
            self.stay_alive,
            if self.encrypted {
                EncryptionType::Ephemeral
            } else {
                EncryptionType::None
            },
            self.client_approval_strategy,
            Box::leak(approver),
        ));

        // Wait for broadcast from peer
        let mut buf = vec![0u8; 100];
        loop {
            if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
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
            let initial_info = serialize(&HandshakeReply {
                server_name: self.name.clone(),
                tcp_port,
                data_length: self.data_length,
                encrypted: self.encrypted,
                file_name: self.file_name.clone(),
            })?;
            self.udp_sock.send_to(&initial_info, &peer).await?;
            if !self.stay_alive {
                data_handle.await?;
                break;
            }
        }

        if let Some(t) = tui_handle {
            t.join()
        }

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
        data: T,
        stay_alive: bool,
        enctype: EncryptionType,
        client_strat: ClientApprovalStrategy,
        approver: &'static dyn ClientApprover,
    ) -> Result<(), Error> {
        info!("TCP listening on {}", tcp_sock.local_addr()?);
        loop {
            let (mut stream, addr) = tcp_sock.accept().await?;
            info!("Accepted download connection from {:?}", &addr);
            let data_src = data.clone();
            let enctype = enctype.clone();
            let h = spawn(async move {
                let res = (|| async {
                    let mut extra = String::new();
                    match enctype {
                        EncryptionType::Ephemeral => {
                            info!("Server handshaking");
                            let secret = EphemeralSecret::new(&mut OsRng);
                            let enc_stream = ServerEncryptedStreamStarter::new(&mut stream, secret);
                            let mut encrypted_stream =
                                match enc_stream.key_exchange(client_strat, approver).await {
                                    Ok(es) => es,
                                    Err(EncStreamErr::ClientNotAccepted) => {
                                        return Result::<_, anyhow::Error>::Ok(())
                                    }
                                    e => e?,
                                };
                            extra = format!("to client {}", encrypted_stream.get_client_id());
                            futures::io::copy(data_src, &mut encrypted_stream)
                                .await
                                .context(format!(
                                    "Couldn't complete transfer to client {}",
                                    encrypted_stream.get_client_id(),
                                ))?;
                        }
                        EncryptionType::None => {
                            futures::io::copy(data_src, &mut stream)
                                .await
                                .context("Couldn't complete transfer to client")?;
                        }
                    }
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
                h.await?;
                return Ok(());
            }
        }
    }
}

#[derive(Clone)]
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

#[cfg(target_family = "unix")]
#[cfg(not(test))]
fn udp_srv_bind_addr(port_num: u16) -> String {
    format!("192.168.0.255:{}", port_num)
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
        file_name: "not a real file".to_string(),
    }
}
