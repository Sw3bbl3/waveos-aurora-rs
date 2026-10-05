//! nslookup NAME — look up an address through DNS.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::net;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let Some(name) = args.get(1) else {
        corekit::eprintln!("usage: nslookup NAME");
        return 2;
    };
    if let Some(i) = net::primary() {
        let dns: alloc::vec::Vec<_> = i.dns.iter().filter(|d| **d != [0; 4]).map(|d| net::ip_string(*d)).collect();
        corekit::println!("Server:  {}", dns.join(", "));
    }
    match net::resolve(name) {
        Ok(ip) => {
            corekit::println!("Name:    {name}\nAddress: {}", net::ip_string(ip));
            0
        }
        Err(e) => {
            corekit::eprintln!("nslookup: {name}: {}", net::describe(e));
            1
        }
    }
}
