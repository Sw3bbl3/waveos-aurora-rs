//! On-demand native connection checks. No background Internet probing.
use crate::{net, time, web};
use alloc::{format, string::String, vec::Vec};

pub struct NetworkCheck {
    pub ok: bool,
    pub lines: Vec<String>,
}

pub fn network(address: &str) -> NetworkCheck {
    let mut result = NetworkCheck { ok: false, lines: Vec::new() };
    let started = time::uptime_ms();
    let ifaces = net::interfaces();
    let v6 = net::interfaces6();
    let mut configured = false;
    for (index, i) in ifaces.iter().enumerate().filter(|(_, i)| i.flags & crate::abi::net::IF_LOOPBACK == 0) {
        let link = i.flags & crate::abi::net::IF_LINK != 0;
        result.lines.push(format!(
            "{}: link {}, IPv4 {}",
            net::name_of(i),
            if link { "up" } else { "down" },
            net::ip_string(i.ip)
        ));
        result.lines.push(format!("  Router: {}", net::ip_string(i.gateway)));
        for d in i.dns.iter().filter(|d| **d != [0; 4]) {
            result.lines.push(format!("  DNS: {}", net::ip_string(*d)));
        }
        if let Some(ipv6) = v6.get(index) {
            for a in ipv6.addrs.iter().take(ipv6.addr_count.min(4) as usize) {
                result.lines.push(format!("  IPv6: {}", net::ip6_string(a)));
            }
            for d in ipv6.dns.iter().take(ipv6.dns_count.min(2) as usize) {
                result.lines.push(format!("  DNS: {}", net::ip6_string(d)));
            }
            configured |= link && ipv6.router != [0; 16] && ipv6.addr_count > 0;
        }
        configured |= link && i.ip != [0; 4];
    }
    if !configured {
        result.lines.push(String::from("No active interface has a usable address."));
        return result;
    }
    let Some(url) = nebula_web::Url::parse(address).filter(|u| matches!(u.scheme.as_str(), "http" | "https")) else {
        result.lines.push(String::from("Use an HTTP or HTTPS address."));
        return result;
    };
    let dns_start = time::uptime_ms();
    match net::resolve_all(&url.host) {
        Ok(addresses) => {
            result.lines.push(format!("DNS: {} resolved in {} ms", url.host, time::uptime_ms() - dns_start));
            for a in addresses {
                result.lines.push(format!("  {a}"));
            }
        }
        Err(e) => {
            result.lines.push(format!("DNS failed: {}", net::describe(e)));
            return result;
        }
    }
    let before = web::stats();
    match web::fetch(&url) {
        Ok(response) => {
            result.ok = response.ok();
            result.lines.push(format!("{}: HTTP {} ({} bytes)", response.url, response.status, response.body.len()));
        }
        Err(e) => result.lines.push(format!("Connection failed: {e}")),
    }
    let after = web::stats();
    result.lines.push(format!(
        "TCP {} ms; TLS {} ms; total {} ms",
        after.connect_ms - before.connect_ms,
        after.tls_ms - before.tls_ms,
        time::uptime_ms() - started
    ));
    result.lines.push(String::from("Native client check; browser compatibility is tested separately."));
    result
}
