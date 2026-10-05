//! fetch [-i] [-o FILE] URL — download over HTTP (and HTTPS), following redirects.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::net;
use nebula_web::http::Error;
use nebula_web::Url;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let headers = args.iter().any(|a| a == "-i");
    let out = args.iter().position(|a| a == "-o").and_then(|i| args.get(i + 1)).cloned();
    let Some(raw) = args.iter().skip(1).find(|a| !a.starts_with('-') && Some(*a) != out.as_ref()) else {
        corekit::eprintln!("usage: fetch [-i] [-o FILE] URL");
        return 2;
    };
    let raw = if raw.contains("://") { raw.clone() } else { alloc::format!("https://{raw}") };
    let Some(url) = Url::parse(&raw) else {
        corekit::eprintln!("fetch: {raw}: not a valid address");
        return 2;
    };
    let t0 = corekit::time::uptime_ms();
    let resp = match corekit::web::fetch(&url) {
        Ok(r) => r,
        Err(Error::Io(e)) => {
            corekit::eprintln!("fetch: {}: {}", url.host, net::describe(corekit::Error(e)));
            return 1;
        }
        Err(e) => {
            corekit::eprintln!("fetch: {e}");
            return 1;
        }
    };
    if headers {
        corekit::println!("HTTP {} {}", resp.status, resp.reason);
        for (k, v) in &resp.headers {
            corekit::println!("{k}: {v}");
        }
        corekit::println!("");
    }
    match out {
        Some(path) => {
            if let Err(e) = corekit::fs::write(&coreutils::path(&path), &resp.body) {
                corekit::eprintln!("fetch: {path}: {e}");
                return 1;
            }
            let ms = (corekit::time::uptime_ms() - t0).max(1);
            corekit::println!(
                "{} from {} in {} ms ({}/s)",
                coreutils::human_size(resp.body.len() as u64),
                resp.url,
                ms,
                coreutils::human_size(resp.body.len() as u64 * 1000 / ms)
            );
        }
        None => {
            let _ = corekit::io::write_fd(1, &resp.body);
        }
    }
    if resp.ok() {
        0
    } else {
        1
    }
}
