//! ping HOST [-c COUNT] — ICMP echo round-trip times.
#![no_std]
#![no_main]
extern crate alloc;
use aurora::net::{self, Ping};
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let count_at = args.iter().position(|a| a == "-c");
    let count: u16 = count_at.and_then(|i| args.get(i + 1)).and_then(|c| c.parse().ok()).unwrap_or(4);
    let host =
        args.iter().enumerate().skip(1).find(|(i, a)| !a.starts_with('-') && Some(i - 1) != count_at).map(|(_, a)| a);
    let Some(host) = host else {
        aurora::eprintln!("usage: ping HOST [-c COUNT]");
        return 2;
    };
    let ip = match net::resolve(host) {
        Ok(ip) => ip,
        Err(e) => {
            aurora::eprintln!("ping: {host}: {}", net::describe(e));
            return 1;
        }
    };
    let p = match Ping::new() {
        Ok(p) => p,
        Err(e) => {
            aurora::eprintln!("ping: {e}");
            return 1;
        }
    };
    aurora::println!("PING {host} ({}): 56 data bytes", net::ip_string(ip));
    let payload = [0x61u8; 56];
    let (mut received, mut total_ms, mut min, mut max) = (0u32, 0u64, u64::MAX, 0u64);
    for seq in 0..count {
        let t0 = aurora::time::uptime_ms();
        if let Err(e) = p.send(ip, seq, &payload) {
            aurora::eprintln!("ping: {e}");
            return 1;
        }
        loop {
            match p.recv(1000u64.saturating_sub(aurora::time::uptime_ms() - t0).max(1)) {
                Ok((from, s, len)) if s == seq => {
                    let ms = aurora::time::uptime_ms() - t0;
                    aurora::println!("{} bytes from {}: icmp_seq={} time={} ms", len + 8, net::ip_string(from), s, ms);
                    received += 1;
                    total_ms += ms;
                    min = min.min(ms);
                    max = max.max(ms);
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    aurora::println!("Request timeout for icmp_seq {seq}");
                    break;
                }
            }
        }
        let spent = aurora::time::uptime_ms() - t0;
        if seq + 1 < count && spent < 1000 {
            aurora::time::sleep_ms(1000 - spent);
        }
    }
    aurora::println!("--- {host} ping statistics ---");
    let loss = (count as u32 - received) * 100 / (count as u32).max(1);
    aurora::println!("{count} packets transmitted, {received} received, {loss}% packet loss");
    if received > 0 {
        aurora::println!("round-trip min/avg/max = {}/{}/{} ms", min, total_ms / received as u64, max);
    }
    if received > 0 {
        0
    } else {
        1
    }
}
