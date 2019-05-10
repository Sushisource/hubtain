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
    tcp_sock: TcpListener,
    data: T,
}

impl<T> FileSrv<T>
where
    T: 'static + AsyncRead + Send + Unpin + Clone,
{
    pub fn new(udp_sock: UdpSocket, tcp_sock: TcpListener, data: T) -> Self {
        FileSrv {
            stay_alive: false,
            udp_sock,
            tcp_sock,
            data,
        }
    }

    pub async fn serve(&'static mut self) -> Result<(), Error> {
        let tcp_port = self.tcp_sock.local_addr()?.port();

        self.udp_sock.set_broadcast(true)?;
        info!(LOG, "UDP Listening on {}", self.udp_sock.local_addr()?);

        spawn(self.data_srv());

        // Wait for broadcast from peer
        let mut buf = vec![0u8; 100];
        loop {
            let (_, peer) = await!(self.udp_sock.recv_from(&mut buf)).unwrap();
            info!(LOG, "Got client handshake from {}", &peer);
            // Reply with tcp portnum
            let portnum = serialize(&tcp_port)?;
            await!(self.udp_sock.send_to(&portnum, &peer))?;
        }
    }

    async fn data_srv(&mut self) -> Result<(), Error> {
        info!(LOG, "TCP listening on {}", self.tcp_sock.local_addr()?);
        loop {
            let (mut stream, addr) = await!(self.tcp_sock.accept())?;
            info!(LOG, "Accepted connection from {:?}", &addr);
            // TODO: Unneeded clone?
            let mut data_src = self.data.clone();
            spawn(async move {
                info!(LOG, "Copying data to stream!");
                await!(data_src.copy_into(&mut stream))
            });
        }
    }
}
