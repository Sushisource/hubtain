#![feature(async_await, await_macro, futures_api)]

use failure::Error;
use futures01::Future as Future01;
use runtime::net::{TcpListener, UdpSocket};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::timer::Delay;
use bincode::serialize;
use runtime::spawn;

#[runtime::main]
async fn main() -> Result<(), Error> {
    await!(serve())?;
    Ok(())
}

async fn serve() -> Result<(), Error> {
    let tcp_sock = TcpListener::bind("127.0.0.1:0")?;
    let tcp_port = tcp_sock.local_addr()?.port();

    let mut socket = UdpSocket::bind("127.0.0.1:42444")?;
    socket.set_broadcast(true)?;
    println!("Listening on {}", socket.local_addr()?);

    spawn(file_srv(tcp_sock));

    // Wait for broadcast from peer
    let mut buf = vec![0u8; 100];
    loop {
        let (recv, peer) = await!(socket.recv_from(&mut buf)).unwrap();
        println!("Got {} bytes {:?} from {}", &recv, &buf, &peer);
        // Reply with tcp portnum
        let portnum = serialize(&tcp_port)?;
        await!(socket.send_to(&portnum, &peer))?;

        let sleep = Delay::new(Instant::now() + Duration::from_millis(300))
            .map_err(|e| panic!("timer failed; err={:?}", e));
        tokio::run(sleep);
    }
}

async fn file_srv(mut tcp_sock: TcpListener) -> Result<(), Error> {
    println!("TCP listening on {}", tcp_sock.local_addr()?);
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use bincode::deserialize;

    #[runtime::test]
    async fn peers_discover_each_other() {
        spawn(serve());

        let broadcast_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 42444);
        let mut client_s = UdpSocket::bind("127.0.0.1:0").unwrap();
        client_s.set_broadcast(true).unwrap();
        let mut buf = vec![0u8; 24];
        await!(client_s.send_to(b"I'm a client!", broadcast_addr)).unwrap();
        dbg!("ClientSent!");
        await!(client_s.recv_from(&mut buf)).unwrap();
        let tcp_port: u16 = deserialize(&buf).unwrap();
        println!("Client found server tcp port {}", &tcp_port);
    }
}
