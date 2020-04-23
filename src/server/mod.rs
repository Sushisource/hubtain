mod client_approver;
#[cfg(not(test))]
mod tui;

pub use client_approver::ClientApprover;

use crate::{
    encrypted_stream::{EncStreamErr, ServerEncryptedStreamStarter},
    mnemonic::random_word,
    models::HandshakeReply,
};
use anyhow::{Context, Error};
use async_std::{
    io,
    net::{TcpListener, UdpSocket},
    task::spawn,
};
use bincode::serialize;
use futures::io::AsyncRead;
use rand::rngs::OsRng;
use x25519_dalek::EphemeralSecret;

#[cfg(not(test))]
use self::tui::ServerTui;
#[cfg(test)]
use crate::server::client_approver::ConsoleApprover;
#[cfg(not(test))]
use crate::tui::TuiApprover;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
}

impl<T> FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    /// Begin listening for connections and serving data.
    pub async fn serve(self) -> Result<(), Error> {
        // TODO: Handle this better than compile flags
        // TODO: Allow for a fallback cli mode for one-client too, and when tui not supported
        #[cfg(not(test))]
        let tui_handle = ServerTui::start(self.name.clone())?;

        let tcp_port = self.tcp_sock.local_addr()?.port();

        self.udp_sock.set_broadcast(true)?;
        info!("UDP Listening on {}", self.udp_sock.local_addr()?);

        #[cfg(test)]
        let approver = Box::leak(Box::new(ConsoleApprover::default()));
        #[cfg(not(test))]
        let approver = Box::leak(Box::new(TuiApprover::new(tui_handle.tx.clone())));

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
            &*approver,
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
            })?;
            self.udp_sock.send_to(&initial_info, &peer).await?;
            if !self.stay_alive {
                data_handle.await?;
                break;
            }
            info!("Done serving");
        }
        tui_handle.join();
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
            // TODO: On error, send message indicating client dropped to approver
            let h = spawn(async move {
                let res = (|| async {
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

#[derive(Clone, Copy)]
pub enum ClientApprovalStrategy {
    Interactive,
    #[cfg(test)]
    ApproveAll,
}

pub struct FileSrvBuilder<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    data: T,
    data_len: u64,
    udp_port: u16,
    stay_alive: bool,
    encryption: bool,
    listen_addr: String,
    client_approval_strategy: ClientApprovalStrategy,
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

impl<T> FileSrvBuilder<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    pub fn new(data: T, data_len: u64) -> FileSrvBuilder<T> {
        FileSrvBuilder {
            data,
            data_len,
            udp_port: 0,
            stay_alive: false,
            encryption: false,
            listen_addr: DEFAULT_TCP_LISTEN_ADDR.to_string(),
            client_approval_strategy: ClientApprovalStrategy::Interactive,
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

    pub fn set_encryption(
        mut self,
        encryption: bool,
        approval_strategy: ClientApprovalStrategy,
    ) -> Self {
        self.encryption = encryption;
        self.client_approval_strategy = approval_strategy;
        self
    }

    pub async fn build(self) -> Result<FileSrv<T>, Error> {
        let tcp_sock = TcpListener::bind(format!("{}:0", &self.listen_addr)).await?;
        let udp_sock = UdpSocket::bind(udp_srv_bind_addr(self.udp_port)).await?;
        let name = random_word();
        Ok(FileSrv {
            stay_alive: self.stay_alive,
            udp_sock,
            tcp_sock,
            data: self.data,
            data_length: self.data_len,
            name: name.to_string(),
            encrypted: self.encryption,
            client_approval_strategy: self.client_approval_strategy,
        })
    }
}
