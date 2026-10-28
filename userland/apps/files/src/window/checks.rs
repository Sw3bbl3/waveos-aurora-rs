//! Real guest filesystem/thread acceptance checks, on an isolated fixture.
use super::*;
macro_rules! check_log { ($($arg:tt)*) => {{
    let text=format!($($arg)*);corekit::io::log(&text);corekit::eprintln!("{}",text);
}}; }
impl Files {
    /// Disposable-VM publication protocol check. The host abruptly quits QEMU
    /// at READY, reboots the same cloned disk, and runs the verify command.
    pub fn powercut_check(phase: u8, prepare: bool) -> i32 {
        let root = format!("/Documents/.files-powercut-check-{phase}");
        let stage = fs::join(&root, "stage");
        let payload = fs::join(&stage, "payload");
        let destination = fs::join(&root, "destination");
        let result = (|| -> Result<(), String> {
            let io = |e: corekit::Error| format!("{e}");
            if prepare {
                // Fail rather than reusing or deleting an existing fixture.
                fs::mkdir(&root).map_err(io)?;
                fs::mkdir(&stage).map_err(io)?;
                fs::write(&destination, &alloc::vec![0x31; 65536]).map_err(io)?;
                fs::write(&payload, &alloc::vec![0x72; 65536]).map_err(io)?;
                fs::try_sync().map_err(io)?;
                if phase >= 1 {
                    fs::rename(&payload, &destination).map_err(io)?;
                }
                if phase == 2 {
                    fs::try_sync().map_err(io)?;
                }
                check_log!("FILES POWER CUT READY phase={}", phase);
                loop {
                    corekit::time::sleep_ms(1000);
                }
            }
            let bytes = fs::read(&destination).map_err(io)?;
            let old = bytes.len() == 65536 && bytes.iter().all(|b| *b == 0x31);
            let new = bytes.len() == 65536 && bytes.iter().all(|b| *b == 0x72);
            if !match phase {
                0 => old,
                1 => old || new,
                _ => new,
            } {
                return Err(String::from("replacement was missing, partial, or inconsistent"));
            }
            check_log!("FILES POWER CUT PASS phase={}, recovered={}", phase, if old { "old" } else { "new" });
            // Remove known fixture files only, never unrelated children.
            if fs::exists(&payload) {
                fs::remove(&payload).map_err(io)?;
            }
            fs::remove(&destination).map_err(io)?;
            fs::remove(&stage).map_err(io)?;
            fs::remove(&root).map_err(io)?;
            fs::try_sync().map_err(io)?;
            Ok(())
        })();
        if let Err(e) = result {
            check_log!("FILES POWER CUT FAIL phase={}: {}", phase, e);
            1
        } else {
            0
        }
    }

    pub fn self_check() -> i32 {
        let root = format!("/Documents/.files-check-{}", corekit::time::uptime_ms());
        let other = format!("/Boot/.files-check-{}", corekit::time::uptime_ms());
        // Never clean up a directory unless this invocation created it.
        if let Err(e) = fs::mkdir(&root) {
            check_log!("FILES CHECK fixture creation: {}", e);
            return 1;
        }
        if let Err(e) = fs::mkdir(&other) {
            check_log!("FILES CHECK fixture creation: {}", e);
            let _ = fs::remove(&root);
            return 1;
        }
        let result = Self::check_fixture(&root, &other);
        for dir in [&root, &other] {
            if fs::exists(dir) {
                if let Err(e) = fs::remove_all(dir) {
                    check_log!("FILES CHECK cleanup {}: {}", dir, e);
                    return 1;
                }
            }
        }
        match result {
            Ok(()) => {
                check_log!("FILES CHECK PASS");
                0
            }
            Err(e) => {
                check_log!("FILES CHECK FAIL: {}", e);
                1
            }
        }
    }
    fn check_fixture(root: &str, other: &str) -> Result<(), String> {
        let io = |e: corekit::Error| format!("{e}");
        let source = fs::join(root, "source");
        let destination = fs::join(root, "destination");
        fs::mkdir(&source).map_err(io)?;
        fs::mkdir(&destination).map_err(io)?;
        for i in 0..128 {
            fs::write(&fs::join(&source, &format!("sample-{i}.txt")), b"search fixture").map_err(io)?;
        }
        let mut app = Self::new(&source);
        app.tabs[0].selection.insert(0);
        app.add();
        app.tabs[1].navigate(&destination, true);
        app.switch(0);
        if app.tabs[0].cwd != source || !app.tabs[0].selection.contains(&0) || app.tabs[1].cwd != destination {
            return Err(String::from("tab state isolation"));
        }
        let process =
            || corekit::process::list().into_iter().find(|p| p.user == 1 && p.pid == corekit::process::pid()).unwrap();
        let mut before = (0, 0);
        for i in 0..60 {
            app.tabs[0].query = String::from(if i % 2 == 0 { "sample" } else { "missing" });
            app.tabs[0].start_search();
            // Explicitly cross the production debounce deadline, then race
            // cancellation/switching against the real guest worker.
            corekit::time::sleep_ms(205);
            app.background(corekit::time::uptime_ms());
            app.switch(1);
            app.switch(0);
            app.tabs[0].stop_search();
            let deadline = corekit::time::uptime_ms() + 5000;
            loop {
                let now = corekit::time::uptime_ms();
                app.background(now);
                if app.worker.lock().is_none() {
                    break;
                }
                if now > deadline {
                    return Err(String::from("search worker did not finish"));
                }
                corekit::time::sleep_ms(10);
            }
            if i == 9 {
                let p = process();
                before = (p.mem_kib, p.threads);
            }
        }
        let after = process();
        check_log!(
            "FILES CHECK search50: before_kib={}, after_kib={}, before_threads={}, after_threads={}",
            before.0,
            after.mem_kib,
            before.1,
            after.threads
        );
        if after.mem_kib > before.0 + 1024 || after.threads != before.1 {
            return Err(String::from("search resources accumulated"));
        }
        let bytes = alloc::vec![0x5a;65536];
        let big = fs::join(&source, "large.bin");
        {
            let mut file = fs::File::open(
                &big,
                corekit::abi::open::WRITE | corekit::abi::open::CREATE | corekit::abi::open::EXCLUSIVE,
            )
            .map_err(io)?;
            for _ in 0..1024 {
                let mut offset = 0;
                while offset < bytes.len() {
                    let n = file.write(&bytes[offset..]).map_err(io)?;
                    if n == 0 {
                        return Err(String::from("fixture short write"));
                    }
                    offset += n;
                }
            }
        }
        let baseline = process().mem_kib;
        let mut peak = baseline;
        if !app.transfers.lock().start(alloc::vec![(big.clone(), destination.clone())], false) {
            return Err(String::from("transfer startup"));
        }
        loop {
            peak = peak.max(process().mem_kib);
            if app.transfers.lock().reap() {
                break;
            }
            corekit::time::sleep_ms(20);
        }
        let state = app.transfers.lock().state.clone();
        {
            let s = state.lock();
            if s.completed != 1 || !s.errors.is_empty() {
                return Err(format!("copy: {:?}", s.errors));
            }
        }
        let copy = fs::join(&destination, "large.bin");
        let mut file = fs::File::open(&copy, corekit::abi::open::READ).map_err(io)?;
        let mut block = alloc::vec![0;65536];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut block).map_err(io)?;
            if n == 0 {
                break;
            }
            if block[..n].iter().any(|b| *b != 0x5a) {
                return Err(String::from("copy content mismatch"));
            }
            total += n as u64;
        }
        check_log!("FILES CHECK copy64MiB: bytes={}, baseline_kib={}, peak_kib={}", total, baseline, peak);
        if total != 64 * 1024 * 1024 || peak > baseline + 4096 {
            return Err(String::from("unbounded copy memory or short copy"));
        }
        drop(file);
        fs::remove(&big).map_err(io)?;
        fs::remove(&copy).map_err(io)?;
        // Real cross-volume copy+sync+delete, AuroraFS -> FAT32.
        let small = fs::join(&source, "move.txt");
        fs::write(&small, b"cross-volume verified").map_err(io)?;
        app.transfers.lock().start(alloc::vec![(small.clone(), String::from(other))], true);
        while !app.transfers.lock().reap() {
            corekit::time::sleep_ms(10);
        }
        if fs::exists(&small) || fs::read(&fs::join(other, "move.txt")).map_err(io)? != b"cross-volume verified" {
            return Err(String::from("cross-volume move"));
        }
        check_log!("FILES CHECK cross-volume PASS");
        // Closing while a conflict waits must cancel, reap and only then close.
        let a = fs::join(&source, "sample-0.txt");
        fs::write(&fs::join(&destination, "sample-0.txt"), b"keep").map_err(io)?;
        app.transfers.lock().start(alloc::vec![(a, destination.clone())], false);
        while app.transfers.lock().state.lock().conflict.is_none() {
            corekit::time::sleep_ms(10);
        }
        if app.ask_close() {
            return Err(String::from("window closed during transfer"));
        }
        app.cancel_close();
        loop {
            app.background(corekit::time::uptime_ms());
            if !app.transfers.lock().busy() {
                break;
            }
            corekit::time::sleep_ms(10);
        }
        if fs::read(&fs::join(&destination, "sample-0.txt")).map_err(io)? != b"keep" {
            return Err(String::from("cancel changed destination"));
        }
        check_log!("FILES CHECK tab isolation and cancel-on-close PASS");
        Ok(())
    }
}
