#![cfg_attr(test, allow(dead_code))]

use anyhow::{anyhow, Error};
use async_std::net::Ipv4Addr;
use get_if_addrs::{get_if_addrs, IfAddr};
use std::net::IpAddr;

pub fn select_broadcast_addr() -> Result<IpAddr, Error> {
    let addrs = get_if_addrs()?;
    let addrs = addrs.into_iter().map(|iface| iface.addr);
    select_from_ips(addrs)
}

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
                    v4addr.broadcast.map(IpAddr::V4)
                }
                _ => None,
            }
        })
        .next()
        .ok_or_else(|| anyhow!("Couldn't select a broadcast address"))
}

pub fn find_local_ip() -> Result<Ipv4Addr, Error> {
    let addrs = get_if_addrs()?;
    addrs
        .into_iter()
        .filter_map(|iface| {
            match iface.addr {
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
                    Some(v4addr.ip)
                }
                _ => None,
            }
        })
        .next()
        .ok_or_else(|| anyhow!("Couldn't determine local IP"))
}
