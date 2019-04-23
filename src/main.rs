#![feature(async_await, await_macro, futures_api)]

use failure::Error;
use futures::compat::Future01CompatExt;
use futures01::Future as Future01;
use runtime::net::UdpSocket;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::timer::Delay;

#[runtime::main]
async fn main() -> Result<(), Error> {
    let socket = UdpSocket::bind("127.0.0.1:42444")?;
    socket.set_broadcast(true)?;
    println!("Listening on {}", socket.local_addr()?);
    await!(serve(socket))?;
    Ok(())
}

async fn serve(mut socket: UdpSocket) -> Result<(), Error> {
    let broadcast_addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)),
        42444,
    );
    // Discover peer
    let mut buf = vec![0u8; 100];
    loop {
        await!(socket.send_to(b"I'm here!", broadcast_addr))?;
        dbg!("Sent!");
        let (recv, peer) = await!(socket.recv_from(&mut buf)).unwrap();
        println!("Got {:?} from {}", &buf, &peer);

        let sleep = Delay::new(Instant::now() + Duration::from_millis(300))
            .map_err(|e| panic!("timer failed; err={:?}", e));
        tokio::run(sleep);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use runtime::spawn;

    #[runtime::test]
    async fn peers_discover_each_other() {
        let sock1 = UdpSocket::bind("127.0.0.1:42444").unwrap();
        sock1.set_broadcast(true).unwrap();
        spawn(serve(sock1));

        let broadcast_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            42444,
            //        socket.local_addr()?.port(),
        );
        let mut client_s = UdpSocket::bind("127.0.0.1:0").unwrap();
        client_s.set_broadcast(true).unwrap();
        let mut buf = vec![0u8; 1024];
        await!(client_s.send_to(b"I'm a client!", broadcast_addr)).unwrap();
        dbg!("ClientSent!");
        let (recv, peer) = await!(client_s.recv_from(&mut buf)).unwrap();
    }
}
