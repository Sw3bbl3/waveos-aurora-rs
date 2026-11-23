//! netcheck [URL] — layered, on-demand connection diagnostics.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);
fn main(args: corekit::Args) -> i32 {
    let serial = args.iter().any(|s| s == "--serial");
    if args.iter().any(|s| s == "--clock-check") {
        // An explicit validation action on a disposable guest. Always restore
        // the original presentation zone, including when a check fails.
        let original = corekit::time::utc_offset_minutes();
        let before = corekit::time::unix_ms();
        let started = corekit::time::uptime_ms();
        let mut ok = true;
        for zone in ["-300", "330", "0"] {
            ok &= corekit::prefs::set("utc_offset", zone).is_ok();
            let expected = before + (corekit::time::uptime_ms() - started) as f64;
            ok &= (corekit::time::unix_ms() - expected).abs() < 2000.0;
        }
        ok &= corekit::prefs::set("utc_offset", &alloc::format!("{original}")).is_ok();
        let result = alloc::format!("CLOCK CHECK {}", if ok { "PASS" } else { "FAIL" });
        if serial {
            corekit::io::log(&result);
        }
        corekit::println!("{result}");
        return i32::from(!ok);
    }
    let url = args.iter().skip(1).find(|s| !s.starts_with('-')).map(|s| s.as_str()).unwrap_or("https://example.com/");
    let report = corekit::diagnostics::network(url);
    for line in report.lines {
        if serial {
            corekit::io::log(&line);
        }
        corekit::println!("{line}");
    }
    let result = alloc::format!("NETCHECK {}", if report.ok { "PASS" } else { "FAIL" });
    if serial {
        corekit::io::log(&result);
    }
    corekit::println!("{result}");
    i32::from(!report.ok)
}
