//! fetch [-i] [-o FILE] URL — download over HTTP (and HTTPS), following redirects.
#![no_std]
#![no_main]
extern crate alloc;
use aurora::net;
use aurora_web::http::Error;
use aurora_web::Url;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let headers = args.iter().any(|a| a == "-i");
    let out = args.iter().position(|a| a == "-o").and_then(|i| args.get(i + 1)).cloned();
    let Some(raw) = args.iter().skip(1).find(|a| !a.starts_with('-') && Some(*a) != out.as_ref()) else {
        aurora::eprintln!("usage: fetch [-i] [-o FILE] URL");
        return 2;
    };
    let raw = if raw.contains("://") { raw.clone() } else { alloc::format!("https://{raw}") };
    let Some(url) = Url::parse(&raw) else {
        aurora::eprintln!("fetch: {raw}: not a valid address");
        return 2;
    };
    let t0 = aurora::time::uptime_ms();
    let resp = match aurora::web::fetch(&url) {
        Ok(r) => r,
        Err(Error::Io(e)) => {
            aurora::eprintln!("fetch: {}: {}", url.host, net::describe(aurora::Error(e)));
            return 1;
        }
        Err(e) => {
            aurora::eprintln!("fetch: {e}");
            return 1;
        }
    };
    if headers {
        aurora::println!("HTTP {} {}", resp.status, resp.reason);
        for (k, v) in &resp.headers {
            aurora::println!("{k}: {v}");
        }
        aurora::println!("");
    }
    match out {
        Some(path) => {
            if let Err(e) = aurora::fs::write(&coreutils::path(&path), &resp.body) {
                aurora::eprintln!("fetch: {path}: {e}");
                return 1;
            }
            let ms = (aurora::time::uptime_ms() - t0).max(1);
            aurora::println!(
                "{} from {} in {} ms ({}/s)",
                coreutils::human_size(resp.body.len() as u64),
                resp.url,
                ms,
                coreutils::human_size(resp.body.len() as u64 * 1000 / ms)
            );
        }
        None => {
            let _ = aurora::io::write_fd(1, &resp.body);
        }
    }
    if resp.ok() {
        0
    } else {
        1
    }
}
