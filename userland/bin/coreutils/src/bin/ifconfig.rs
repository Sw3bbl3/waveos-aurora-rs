//! ifconfig — network interfaces: addresses, link, traffic.
#![no_std]
#![no_main]
extern crate alloc;
use aurora::abi::net::*;
use aurora::net;
aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    for i in net::interfaces() {
        let mut flags = alloc::vec::Vec::new();
        if i.flags & IF_UP != 0 {
            flags.push("UP");
        }
        if i.flags & IF_LOOPBACK != 0 {
            flags.push("LOOPBACK");
        }
        if i.flags & IF_LINK != 0 {
            flags.push("RUNNING");
        }
        if i.flags & IF_DHCP != 0 {
            flags.push("DHCP");
        }
        aurora::println!("{}: <{}>  {}", net::name_of(&i), flags.join(","), net::driver_of(&i));
        if i.flags & IF_LOOPBACK == 0 {
            let m = i.mac;
            aurora::println!("    ether {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
        }
        if i.ip != [0; 4] {
            aurora::println!("    inet {}  netmask {}", net::ip_string(i.ip), net::ip_string(i.netmask));
        } else {
            aurora::println!("    (no address yet)");
        }
        if i.gateway != [0; 4] {
            aurora::println!("    gateway {}", net::ip_string(i.gateway));
        }
        let dns: alloc::vec::Vec<_> = i.dns.iter().filter(|d| **d != [0; 4]).map(|d| net::ip_string(*d)).collect();
        if !dns.is_empty() {
            aurora::println!("    dns {}", dns.join(", "));
        }
        if i.lease_secs > 0 {
            aurora::println!("    lease {} h {} min left", i.lease_secs / 3600, i.lease_secs / 60 % 60);
        }
        aurora::println!(
            "    RX {} packets ({})  TX {} packets ({})",
            i.rx_packets,
            coreutils::human_size(i.rx_bytes),
            i.tx_packets,
            coreutils::human_size(i.tx_bytes)
        );
    }
    0
}
