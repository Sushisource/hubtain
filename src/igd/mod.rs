use crate::broadcast_addr_picker::find_local_ip;
use anyhow::Error;
use igd::{search_gateway, PortMappingProtocol, SearchOptions};
use std::net::SocketAddrV4;

pub fn get_external_addr(local_port: u16) -> Result<(), Error> {
    // The local address needs to be set
    // router (at least, this TPLink one) to work properly.
    let local_addr = find_local_ip()?;
    let local_addr = SocketAddrV4::new(local_addr, local_port);
    info!("Getting external address for internal addr {}", &local_addr);
    let gw = search_gateway(SearchOptions::default())?;
    dbg!(&gw);
    // TODO: Rewnew lease periodically?
    let addr = gw.get_any_address(PortMappingProtocol::TCP, local_addr, 120, "hubtain")?;
    dbg!(addr);
    Ok(())
}
