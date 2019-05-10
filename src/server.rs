use futures::io::{AsyncRead, AsyncReadExt};
use runtime::{
    net::{TcpListener, UdpSocket},
    spawn,
};
use failure::Error;
use bincode::serialize;
use crate::LOG;

pub async fn serve<T: 'static + AsyncRead + Send + Unpin + Clone>(
    tcp_sock: TcpListener,
    mut socket: UdpSocket,
    data_src: T,
) -> Result<(), Error> {
    let tcp_port = tcp_sock.local_addr()?.port();

    socket.set_broadcast(true)?;
    info!(LOG, "UDP Listening on {}", socket.local_addr()?);

    spawn(data_srv(tcp_sock, data_src));

    // Wait for broadcast from peer
    let mut buf = vec![0u8; 100];
    loop {
        let (_, peer) = await!(socket.recv_from(&mut buf)).unwrap();
        info!(LOG, "Got client handshake from {}", &peer);
        // Reply with tcp portnum
        let portnum = serialize(&tcp_port)?;
        await!(socket.send_to(&portnum, &peer))?;
    }
}

async fn data_srv<T: 'static + AsyncRead + Unpin + Send + Clone>(
    mut listener: TcpListener,
    data_src: T,
) -> Result<(), Error> {
    info!(LOG, "TCP listening on {}", listener.local_addr()?);
    loop {
        let (mut stream, addr) = await!(listener.accept())?;
        info!(LOG, "Accepted connection from {:?}", &addr);
        let mut data_src = data_src.clone();
        spawn(async move {
            info!(LOG, "Copying data to stream!");
            await!(data_src.copy_into(&mut stream))
        });
    }
}

