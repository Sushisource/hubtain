use crate::broadcast_addr_picker::find_local_ip;
use anyhow::{Context, Error};
use igd::{search_gateway, PortMappingProtocol, SearchOptions};
use std::net::SocketAddrV4;

pub fn get_external_addr(local_port: u16) -> Result<(), Error> {
    // The local address needs to be set
    // router (at least, this TPLink one) to work properly.
    let local_addr = find_local_ip()?;
    let local_addr = SocketAddrV4::new(local_addr, local_port);
    info!("Getting external address for internal addr {}", &local_addr);
    let gw = search_gateway(SearchOptions::default())
        .context("Problem searching for network gateway")?;
    dbg!(&gw);
    // TODO: Rewnew lease periodically?
    let addr = gw
        .get_any_address(PortMappingProtocol::TCP, local_addr, 120, "hubtain")
        .context("Problem getting external address from gateway")?;
    dbg!(addr);
    Ok(())
}

#[cfg(test)]
mod test {
    use std::net::Ipv4Addr;
    use std::{net::UdpSocket, str::from_utf8, time::Duration};

    pub const SEARCH_REQUEST: &'static str = "M-SEARCH * HTTP/1.1\r
Host:239.255.255.250:1900\r
ST:urn:schemas-upnp-org:device:InternetGatewayDevice:1\r
Man:\"ssdp:discover\"\r
MX:3\r\n\r\n";

    #[test]
    fn udptest() {
        let socket = UdpSocket::bind("0.0.0.0:5555").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let interface = Ipv4Addr::new(0, 0, 0, 0);
        let mcast_addr = Ipv4Addr::new(239, 255, 255, 250);
        socket.join_multicast_v4(&mcast_addr, &interface).unwrap();
        socket
            .send_to(SEARCH_REQUEST.as_bytes(), "239.255.255.250:1900")
            .unwrap();
        let mut buf = [0u8; 1500];
        let (read, _) = socket.recv_from(&mut buf).unwrap();
        let text = from_utf8(&buf[..read]).unwrap();
        dbg!(text);
    }
}
