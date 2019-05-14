use crate::LOG;
use bincode::serialize;
use failure::Error;
use futures::io::{AsyncRead, AsyncReadExt};
use runtime::{
    net::{TcpListener, UdpSocket},
    spawn,
};

pub struct FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    stay_alive: bool,
    udp_sock: UdpSocket,
    tcp_sock: Option<TcpListener>,
    data: Option<T>,
}

impl<T> FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    pub fn new(udp_sock: UdpSocket, tcp_sock: TcpListener, data: T, stay_alive: bool) -> Self {
        FileSrv {
            stay_alive,
            udp_sock,
            tcp_sock: Some(tcp_sock),
            data: Some(data),
        }
    }

    pub async fn serve(mut self) -> Result<(), Error> {
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
            let (_, peer) = self.udp_sock.recv_from(&mut buf).await.unwrap();
            info!(LOG, "Got client handshake from {}", &peer);
            // Reply with tcp portnum
            let portnum = serialize(&tcp_port)?;
            self.udp_sock.send_to(&portnum, &peer).await?;
            if !self.stay_alive {
                data_handle.await?;
                info!(LOG, "Done serving!");
                return Ok(());
            }
        }
    }

    async fn data_srv(mut tcp_sock: TcpListener, data: T, stay_alive: bool) -> Result<(), Error> {
        info!(LOG, "TCP listening on {}", tcp_sock.local_addr()?);
        loop {
            let (mut stream, addr) = tcp_sock.accept().await?;
            info!(LOG, "Accepted connection from {:?}", &addr);
            // TODO: Unneeded clone?
            let mut data_src = data.clone();
            spawn(async move {
                info!(LOG, "Copying data to stream!");
                data_src.copy_into(&mut stream).await
            });
            if !stay_alive {
                return Ok(());
            }
        }
    }
}
