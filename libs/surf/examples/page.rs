//! Runs a page's scripts on the host, as Nebula would, and reports what
//! happened: `cargo run -p nebula-engine --example page -- page.html URL`.
//! External scripts are fetched with `curl`.

use nebula_engine::nebula_script::Host;
use nebula_engine::{Page, ScriptSource, Scripts};
use std::collections::BTreeMap;
use std::time::Instant;

struct StdHost;

impl Host for StdHost {
    fn now_ms(&mut self) -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0
    }

    fn console(&mut self, level: &str, line: &str) {
        let line: String = line.chars().take(300).collect();
        println!("  console.{level}: {line}");
    }
}

fn resolve(base: &str, href: &str) -> String {
    if href.starts_with("http") {
        return href.to_string();
    }
    if let Some(rest) = href.strip_prefix("//") {
        return format!("https://{rest}");
    }
    let origin: String = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    if href.starts_with('/') {
        format!("{origin}{href}")
    } else {
        format!("{}/{href}", base.rsplit_once('/').map(|(a, _)| a).unwrap_or(base))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let html = std::fs::read_to_string(&args[1]).expect("read page");
    let url = args.get(2).cloned().unwrap_or_else(|| String::from("https://example.com/"));
    let mut page = Page::parse(&html);
    let mut js = Scripts::new(Box::new(StdHost), &url, (1100, 800), BTreeMap::new());
    let t0 = Instant::now();
    for (i, s) in page.scripts().into_iter().enumerate() {
        let (name, code) = match s {
            ScriptSource::Inline(c) => (format!("inline #{}", i + 1), c),
            ScriptSource::External(href) => {
                let u = resolve(&url, &href);
                let out = std::process::Command::new("curl").args(["-sL", "--max-time", "20", &u]).output().unwrap();
                (u, String::from_utf8_lossy(&out.stdout).into_owned())
            }
        };
        let t = Instant::now();
        println!("script {} ({} bytes)", name, code.len());
        page.run_script(&mut js, &code, &name);
        println!("  took {:?}", t.elapsed());
    }
    page.finish_loading(&mut js);
    // Scripts that scripts insert (loaders): fetch and run until none are left.
    for _ in 0..50 {
        let fetches = js.take_fetches();
        if fetches.is_empty() {
            break;
        }
        for (node, src) in fetches {
            let u = resolve(&url, &src);
            let out = std::process::Command::new("curl").args(["-sL", "--max-time", "20", &u]).output().unwrap();
            let code = String::from_utf8_lossy(&out.stdout).into_owned();
            let t = Instant::now();
            println!("inserted script {} ({} bytes)", u, code.len());
            page.run_fetched(&mut js, node, &u, Some(&code));
            println!("  took {:?}", t.elapsed());
        }
    }
    let mut now = js.next_wakeup(0.0);
    for _ in 0..20 {
        let Some(n) = now else { break };
        page.run_timers(&mut js, n);
        now = js.next_wakeup(n);
    }
    let html_el = page.doc.find("html").unwrap();
    println!(
        "total {:?}; <html class=\"{}\">; {} nodes; title {:?}",
        t0.elapsed(),
        page.doc.element(html_el).unwrap().attr("class").unwrap_or(""),
        page.doc.nodes.len(),
        page.title
    );
}
