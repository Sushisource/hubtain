use failure::{err_msg, Error};
use std::net::IpAddr;

#[cfg(target_family = "windows")]
use get_if_addrs::get_if_addrs;
use get_if_addrs::IfAddr;

#[cfg(target_family = "windows")]
pub fn select_broadcast_addr() -> Result<IpAddr, Error> {
    let addrs = get_if_addrs()?;
    let addrs = addrs.into_iter().map(|iface| iface.addr);
    select_from_ips(addrs)
}

#[cfg(target_family = "unix")]
pub fn select_broadcast_addr() -> IpAddr {}

fn select_from_ips<T: IntoIterator<Item = IfAddr>>(addrs: T) -> Result<IpAddr, Error> {
    addrs
        .into_iter()
        .filter_map(|addr| {
            match addr {
                IfAddr::V4(v4addr) => {
                    if v4addr.is_loopback() {
                        return None;
                    }
                    // Netmasks need to end in 0 for local IPs
                    if v4addr.netmask.octets()[3] != 0 {
                        return None;
                    }
                    // TODO: Sort and pick best
                    if v4addr.ip.octets()[0] != 192 {
                        return None;
                    }
                    v4addr.broadcast.map(|bc| IpAddr::V4(bc))
                }
                _ => None,
            }
        })
        .next()
        .ok_or(err_msg("Couldn't select a broadcast address"))
}
