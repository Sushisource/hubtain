use crate::{mnemonic::random_word, models::HandshakeReply, LOG};
use bincode::serialize;
use failure::Error;
use futures::io::{AsyncRead, AsyncReadExt};
use runtime::{
    net::{TcpListener, UdpSocket},
    spawn,
};

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
    ) -> Self {
        let name = random_word();
        FileSrv {
            stay_alive,
            udp_sock,
            tcp_sock: Some(tcp_sock),
            data: Some(data),
            data_length,
            name: name.to_string(),
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
            })?;
            self.udp_sock.send_to(&initial_info, &peer).await?;
            if !self.stay_alive {
                data_handle.await?;
                info!(LOG, "Done serving!");
                return Ok(());
            }
        }
    }

    /// The data srv runs independently of the main srv loop, and does the job of actually
    /// transferring data to clients.
    async fn data_srv(mut tcp_sock: TcpListener, data: T, stay_alive: bool) -> Result<(), Error> {
        info!(LOG, "TCP listening on {}", tcp_sock.local_addr()?);
        loop {
            let (mut stream, addr) = tcp_sock.accept().await?;
            info!(LOG, "Accepted download connection from {:?}", &addr);
            // TODO: Unneeded clone?
            let mut data_src = data.clone();
            let _ = spawn(async move {
                info!(LOG, "Client downloading!");
                data_src.copy_into(&mut stream).await
            });
            if !stay_alive {
                return Ok(());
            }
        }
    }
}
