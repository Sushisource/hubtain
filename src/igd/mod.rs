use crate::broadcast_addr_picker::find_local_ip;
use anyhow::{Context, Error};
use igd::{search_gateway, Gateway, PortMappingProtocol, SearchOptions};
use std::net::SocketAddrV4;

/// Request an external address from the local gateway. The provided port number is where the
/// incoming connections will be sent back to this machine.
///
/// ## Compatability notes
/// * On windows wifi adapters, joining multicast seems to take a while or not work at all sometimes
pub fn get_external_addr(local_port: u16) -> Result<IGDHandle, Error> {
    // The local address needs to be set
    // router (at least, this TPLink one) to work properly.
    let local_addr = find_local_ip()?;
    let local_addr = SocketAddrV4::new(local_addr, local_port);
    info!("Getting external address for internal addr {}", &local_addr);
    let gw = search_gateway(SearchOptions::default())
        .context("Problem searching for network gateway")?;
    // Leases default to an hour given we make a solid cleanup attempt
    let addr = gw
        .get_any_address(PortMappingProtocol::TCP, local_addr, 60 * 60, "hubtain")
        .context("Problem getting external address from gateway")?;
    info!(
        "Obtained external address, share this to your downloader: {}",
        addr
    );
    Ok(IGDHandle {
        gateway: gw,
        port: addr.port(),
    })
}

pub struct IGDHandle {
    gateway: Gateway,
    port: u16,
}

impl IGDHandle {
    pub fn free(self) -> Result<(), Error> {
        self.gateway
            .remove_port(PortMappingProtocol::TCP, self.port)?;
        Ok(())
    }
}
