//! ping HOST [-c COUNT] [-6] — ICMP echo round-trip times (ICMPv6 for an
//! IPv6 address, or with -6 to prefer a name's IPv6 address).
#![no_std]
#![no_main]
extern crate alloc;
use corekit::net::{self, IpAddr, Ping, Ping6};
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let count_at = args.iter().position(|a| a == "-c");
    let count: u16 = count_at.and_then(|i| args.get(i + 1)).and_then(|c| c.parse().ok()).unwrap_or(4);
    let host =
        args.iter().enumerate().skip(1).find(|(i, a)| !a.starts_with('-') && Some(i - 1) != count_at).map(|(_, a)| a);
    let Some(host) = host else {
        corekit::eprintln!("usage: ping HOST [-c COUNT]");
        return 2;
    };
    let prefer6 = args.iter().any(|a| a == "-6");
    let addr = match net::resolve_all(host) {
        Ok(addrs) => {
            let pick = |six: bool| addrs.iter().copied().find(|a| matches!(a, IpAddr::V6(_)) == six);
            match pick(prefer6).or_else(|| pick(!prefer6)) {
                Some(a) => a,
                None => {
                    corekit::eprintln!("ping: {host}: no address");
                    return 1;
                }
            }
        }
        Err(e) => {
            corekit::eprintln!("ping: {host}: {}", net::describe(e));
            return 1;
        }
    };
    enum Pinger {
        Four(Ping, net::Ip),
        Six(Ping6, [u8; 16]),
    }
    let p = match addr {
        IpAddr::V4(ip) => Ping::new().map(|p| Pinger::Four(p, ip)),
        IpAddr::V6(ip) => Ping6::new().map(|p| Pinger::Six(p, ip)),
    };
    let p = match p {
        Ok(p) => p,
        Err(e) => {
            corekit::eprintln!("ping: {e}");
            return 1;
        }
    };
    corekit::println!("PING {host} ({addr}): 56 data bytes");
    let payload = [0x61u8; 56];
    let (mut received, mut total_ms, mut min, mut max) = (0u32, 0u64, u64::MAX, 0u64);
    for seq in 0..count {
        let t0 = corekit::time::uptime_ms();
        let sent = match &p {
            Pinger::Four(p, ip) => p.send(*ip, seq, &payload),
            Pinger::Six(p, ip) => p.send(*ip, seq, &payload),
        };
        if let Err(e) = sent {
            corekit::eprintln!("ping: {e}");
            return 1;
        }
        loop {
            let wait = 1000u64.saturating_sub(corekit::time::uptime_ms() - t0).max(1);
            let got = match &p {
                Pinger::Four(p, _) => p.recv(wait).map(|(f, s, l)| (IpAddr::V4(f), s, l)),
                Pinger::Six(p, _) => p.recv(wait).map(|(f, s, l)| (IpAddr::V6(f), s, l)),
            };
            match got {
                Ok((from, s, len)) if s == seq => {
                    let ms = corekit::time::uptime_ms() - t0;
                    corekit::println!("{} bytes from {}: icmp_seq={} time={} ms", len + 8, from, s, ms);
                    received += 1;
                    total_ms += ms;
                    min = min.min(ms);
                    max = max.max(ms);
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    corekit::println!("Request timeout for icmp_seq {seq}");
                    break;
                }
            }
        }
        let spent = corekit::time::uptime_ms() - t0;
        if seq + 1 < count && spent < 1000 {
            corekit::time::sleep_ms(1000 - spent);
        }
    }
    corekit::println!("--- {host} ping statistics ---");
    let loss = (count as u32 - received) * 100 / (count as u32).max(1);
    corekit::println!("{count} packets transmitted, {received} received, {loss}% packet loss");
    if received > 0 {
        corekit::println!("round-trip min/avg/max = {}/{}/{} ms", min, total_ms / received as u64, max);
    }
    if received > 0 {
        0
    } else {
        1
    }
}
