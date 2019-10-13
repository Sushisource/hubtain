use crate::encrypted_stream::EncryptedStreamStarter;
use crate::{
    encrypted_stream::EncryptedStream, mnemonic::random_word, models::HandshakeReply, LOG,
};
use anyhow::Error;
use bincode::serialize;
use futures::io::{AsyncRead, AsyncReadExt};
use rand::rngs::OsRng;
use runtime::{
    net::{TcpListener, UdpSocket},
    spawn,
    task::JoinHandle,
};
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

/// File server for hubtain's srv mode
pub struct FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    stay_alive: bool,
    udp_sock: UdpSocket,
    tcp_sock: Option<TcpListener>,
    data: Option<T>,
    data_length: u64,
    name: String,
    encrypted: bool,
}

impl<T> FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    /// Create a new `FileSrv` given UDP and TCP sockets to listen on and some data source to serve
    /// If `stay_alive` is false, the server will shut down after serving the data to the first
    /// client who downloads it.
    pub fn new(
        udp_sock: UdpSocket,
        tcp_sock: TcpListener,
        data: T,
        data_length: u64,
        stay_alive: bool,
        encrypted: bool,
    ) -> Self {
        let name = random_word();
        FileSrv {
            stay_alive,
            udp_sock,
            tcp_sock: Some(tcp_sock),
            data: Some(data),
            data_length,
            name: name.to_string(),
            encrypted,
        }
    }

    /// Begin listening for connections and serving data.
    pub async fn serve(mut self) -> Result<(), Error> {
        info!(LOG, "Server name: {}", self.name);
        // TODO: no unwrap
        let tcp_port = self.tcp_sock.as_ref().unwrap().local_addr()?.port();

        self.udp_sock.set_broadcast(true)?;
        info!(LOG, "UDP Listening on {}", self.udp_sock.local_addr()?);

        let data_handle = spawn(FileSrv::data_srv(
            self.tcp_sock.take().unwrap(),
            self.data.take().unwrap(),
            self.stay_alive,
            match self.encrypted {
                true => EncryptionType::Ephemeral,
                false => EncryptionType::None,
            },
        ));

        // Wait for broadcast from peer
        let mut buf = vec![0u8; 100];
        loop {
            let (_, peer) = self.udp_sock.recv_from(&mut buf).await?;
            info!(LOG, "Client ping from {}", &peer);
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
                info!(LOG, "Done serving!");
                return Ok(());
            }
        }
    }

    /// Return the UDP port the server is bound to
    pub fn udp_port(&self) -> Result<u16, Error> {
        Ok(self.udp_sock.local_addr()?.port())
    }

    /// The data srv runs independently of the main srv loop, and does the job of actually
    /// transferring data to clients.
    async fn data_srv(
        mut tcp_sock: TcpListener,
        data: T,
        stay_alive: bool,
        enctype: EncryptionType,
    ) -> Result<(), Error> {
        info!(LOG, "TCP listening on {}", tcp_sock.local_addr()?);
        loop {
            let (mut stream, addr) = tcp_sock.accept().await?;
            info!(LOG, "Accepted download connection from {:?}", &addr);
            // TODO: Unneeded clone?
            let data_src = data.clone();
            let enctype = enctype.clone();
            // TODO: Don't use encrypytion when not encrypted mode
            let h: JoinHandle<Result<(), Error>> = spawn(async move {
                let mut write_stream = match enctype {
                    EncryptionType::Ephemeral => {
                        let mut rng = OsRng::new().unwrap();
                        let secret = EphemeralSecret::new(&mut rng);
                        let enc_stream = EncryptedStreamStarter::new(&mut stream, secret);
                        info!(LOG, "Server handshaking");
                        enc_stream.key_exchange().await?
                    }
                    _ => unimplemented!(),
                };
                info!(LOG, "Client downloading!");
                data_src.copy_into(&mut write_stream).await?;
                Ok(())
            });
            if !stay_alive {
                // TODO: Does this screw up multi client mode?
                h.await?;
                info!(LOG, "I'M MELLLLLLTIIIIING");
                return Ok(());
            }
        }
    }
}

#[derive(Clone)]
enum EncryptionType {
    None,
    Static(StaticSecret),
    Ephemeral,
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
        self
    }

    pub fn build(self) -> Result<FileSrv<T>, Error> {
        let tcp_sock = TcpListener::bind(format!("{}:0", &self.listen_addr))?;
        let udp_sock = UdpSocket::bind(udp_srv_bind_addr(self.udp_port))?;
        Ok(FileSrv::new(
            udp_sock,
            tcp_sock,
            self.data,
            self.data_len,
            self.stay_alive,
            self.encryption,
        ))
    }
}
