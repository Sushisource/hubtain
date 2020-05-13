use crate::broadcast_addr_picker::find_local_ip;
use anyhow::{Context, Error};
use igd::{search_gateway, PortMappingProtocol, SearchOptions};
use std::net::SocketAddrV4;

/// Request an external address from the local gateway. The provided port number is where the
/// incoming connections will be sent back to this machine.
///
/// ## Compatability notes
/// * On windows wifi adapters, joining multicast seems to take a while or not work at all sometimes
pub fn get_external_addr(local_port: u16) -> Result<SocketAddrV4, Error> {
    // The local address needs to be set
    // router (at least, this TPLink one) to work properly.
    let local_addr = find_local_ip()?;
    let local_addr = SocketAddrV4::new(local_addr, local_port);
    info!("Getting external address for internal addr {}", &local_addr);
    let gw = search_gateway(SearchOptions::default())
        .context("Problem searching for network gateway")?;
    // TODO: Rewnew lease periodically?
    let addr = gw
        .get_any_address(PortMappingProtocol::TCP, local_addr, 600, "hubtain")
        .context("Problem getting external address from gateway")?;
    // TODO: Remove port at end of run - maybe some kind of handle w/ Drop impl
    Ok(addr)
}
