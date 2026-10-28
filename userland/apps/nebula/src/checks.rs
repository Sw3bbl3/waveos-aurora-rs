//! `nebula --self-check`: tab behaviour, checked in the guest with real
//! threads, timers and storage. Pages are installed directly, so no network
//! is needed; only `browser-apis.html` is loaded from the system volume.

use super::*;

macro_rules! check_log { ($($arg:tt)*) => {{
    let text = format!($($arg)*);
    corekit::io::log(&format!("{text}\n"));
    corekit::eprintln!("{}", text);
}}; }

const SITE: &str = "https://nebula-self-check.invalid/";

/// A page as if it had loaded from `path` on the check's site.
fn fixture(path: &str, html: &str) -> Loaded {
    let page = Page::parse(html);
    let scripts = page
        .scripts()
        .into_iter()
        .filter_map(|s| match s {
            ScriptSource::Inline(code) => Some((String::from("inline"), code)),
            ScriptSource::External(_) => None,
        })
        .collect();
    Loaded {
        timing: Timing::default(),
        page,
        url: Url::parse(&format!("{SITE}{path}")).unwrap(),
        images: Vec::new(),
        scripts,
        pending: false,
    }
}

/// Replaces whatever tab `i` was loading with `loaded`.
fn install(b: &mut Browser, i: usize, loaded: Loaded) {
    b.tabs[i].stop();
    b.tabs[i].install(loaded);
}

fn draw(b: &mut Browser, pixels: &mut [u32]) {
    let mut cv = Canvas::new(pixels, 1100, 760);
    b.render(&mut cv, Rect::new(0, 0, 1100, 680));
}

/// Ticks every 10 ms until `done` or `ms` pass. Returns whether `done` held.
fn run_until(b: &mut Browser, ms: u64, mut done: impl FnMut(&Browser) -> bool) -> bool {
    let deadline = corekit::time::uptime_ms() + ms;
    loop {
        let now = corekit::time::uptime_ms();
        b.tick_all(now);
        if done(b) {
            return true;
        }
        if now > deadline {
            return false;
        }
        corekit::time::sleep_ms(10);
    }
}

fn memory() -> (u64, u32) {
    let me = corekit::process::pid();
    corekit::process::list().into_iter().find(|p| p.user == 1 && p.pid == me).map_or((0, 0), |p| (p.mem_kib, p.threads))
}

pub fn self_check() -> i32 {
    let result = checks();
    // Remove only the check site's own storage file.
    let _ = corekit::fs::remove(&storage_path(&Url::parse(SITE).unwrap()));
    match result {
        Ok(()) => {
            check_log!("NEBULA CHECK PASS");
            0
        }
        Err(e) => {
            check_log!("NEBULA CHECK FAIL: {}", e);
            1
        }
    }
}

fn checks() -> Result<(), String> {
    let mut pixels = alloc::vec![0u32; 1100 * 760];
    let mut b = Browser::new(HOME);
    if !run_until(&mut b, 10_000, |b| !b.tabs[0].loading()) {
        return Err(String::from("the start page didn't load"));
    }
    draw(&mut b, &mut pixels);

    // Each tab keeps its own zoom, scroll and address.
    b.open(HOME, false);
    run_until(&mut b, 10_000, |b| !b.tabs[1].loading());
    draw(&mut b, &mut pixels);
    b.tabs[1].set_zoom(150);
    b.tabs[1].address.set_text("typed in tab two");
    b.select(0);
    draw(&mut b, &mut pixels);
    if b.tabs[0].zoom == 150 || b.tabs[0].address.text() == "typed in tab two" || b.tabs[1].zoom != 150 {
        return Err(String::from("tab state isolation"));
    }
    check_log!("NEBULA CHECK isolation: ok");

    // A page opened behind finishes loading, but isn't laid out until shown.
    let apis = "file:///System/Library/Nebula/browser-apis.html";
    b.open(apis, true);
    if b.active != 0 {
        return Err(String::from("a background tab took focus"));
    }
    if !run_until(&mut b, 15_000, |b| !b.tabs[2].loading() && b.tabs[2].page.is_some()) {
        return Err(String::from("a background tab didn't finish loading"));
    }
    let title = b.tabs[2].title();
    if b.tabs[2].layout.is_some() || title == "Loading…" || title == "Nebula" {
        return Err(format!("background load: layout {}, title {title:?}", b.tabs[2].layout.is_some()));
    }
    b.select(2);
    draw(&mut b, &mut pixels);
    if b.tabs[2].layout.is_none() {
        return Err(String::from("no layout once selected"));
    }
    check_log!("NEBULA CHECK background load: title={:?}", title);
    b.close_tab(2);
    b.close_tab(1);

    // Storage events reach other tabs on the same site; background timers
    // are throttled to once a second.
    install(
        &mut b,
        0,
        fixture(
            "listener.html",
            "<!doctype html><title>waiting</title><script>\
             addEventListener('storage', e => document.title = 'got ' + e.key + '=' + e.newValue);\
             </script>",
        ),
    );
    b.open(HOME, false);
    install(
        &mut b,
        1,
        fixture(
            "counter.html",
            "<!doctype html><title>0</title><script>\
             let n = 0; setInterval(() => document.title = String(++n), 10);\
             localStorage.setItem('flavour', 'mint');\
             </script>",
        ),
    );
    if !run_until(&mut b, 3000, |b| b.tabs[0].title() == "got flavour=mint") {
        return Err(format!("storage event: background tab title {:?}", b.tabs[0].title()));
    }
    if b.storage.borrow_mut().get(&Url::parse(SITE).unwrap()).get("flavour").map(String::as_str) != Some("mint") {
        return Err(String::from("storage wasn't shared"));
    }
    check_log!("NEBULA CHECK storage event: ok");
    b.select(0);
    let start: i32 = b.tabs[1].title().parse().unwrap_or(0);
    run_until(&mut b, 2500, |_| false);
    let background: i32 = b.tabs[1].title().parse().unwrap_or(0) - start;
    b.select(1);
    let start: i32 = b.tabs[1].title().parse().unwrap_or(0);
    run_until(&mut b, 1000, |_| false);
    let foreground: i32 = b.tabs[1].title().parse().unwrap_or(0) - start;
    check_log!("NEBULA CHECK timers: background={} in 2.5 s, foreground={} in 1 s", background, foreground);
    if !(1..=4).contains(&background) || foreground < 10 {
        return Err(String::from("background timers weren't throttled, or foreground ones were"));
    }

    // Switching tabs doesn't accumulate memory or threads.
    let mut before = (0, 0);
    for i in 0..200 {
        b.select(i % 2);
        draw(&mut b, &mut pixels);
        b.tick_all(corekit::time::uptime_ms());
        if i == 19 {
            before = memory();
        }
    }
    let after = memory();
    check_log!(
        "NEBULA CHECK switch200: before_kib={}, after_kib={}, before_threads={}, after_threads={}",
        before.0,
        after.0,
        before.1,
        after.1
    );
    if after.0 > before.0 + 1024 || after.1 > before.1 {
        return Err(String::from("switching tabs accumulated resources"));
    }

    // Eight tabs at most, and the tab strip says so.
    while b.tabs.len() < MAX_TABS {
        b.open(HOME, true);
    }
    if b.open(HOME, false) || b.tabs.len() != MAX_TABS || !b.notice() {
        return Err(String::from("tab limit"));
    }
    check_log!("NEBULA CHECK limit: ok");
    Ok(())
}
