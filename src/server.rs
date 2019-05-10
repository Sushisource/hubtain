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
    pub fn new(udp_sock: UdpSocket, tcp_sock: TcpListener, data: T) -> Self {
        FileSrv {
            stay_alive: false,
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

        spawn(FileSrv::data_srv(
            self.tcp_sock.take().unwrap(),
            self.data.take().unwrap(),
        ));

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

    async fn data_srv(mut tcp_sock: TcpListener, data: T) -> Result<(), Error> {
        info!(LOG, "TCP listening on {}", tcp_sock.local_addr()?);
        loop {
            let (mut stream, addr) = await!(tcp_sock.accept())?;
            info!(LOG, "Accepted connection from {:?}", &addr);
            // TODO: Unneeded clone?
            let mut data_src = data.clone();
            spawn(async move {
                info!(LOG, "Copying data to stream!");
                await!(data_src.copy_into(&mut stream))
            });
        }
    }
}
