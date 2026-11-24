//! Self-tests of the sandbox (`cargo xtask test`): the policy itself, then
//! programs inside a sandbox — `sbxtest` (native) and `sandboxtest`
//! (Linux) trying every way out they can, and both system-call fuzzers
//! aimed at an *oracle*: a folder outside their sandbox, on a filesystem
//! of its own that counts every call a sandboxed process makes to its
//! secret, to its folder's list, or to a name it was never given. Any count
//! is an escape, whatever the call returned.

use super::*;
use crate::fs::{ramfs::RamFs, DirEntry, Filesystem, FsResult, Ino, Kind as FileKind, Metadata};
use crate::proc::Handle;
use crate::sched;
use alloc::string::ToString;

pub const ORACLE: &str = "/SandboxOracle";
const SECRET: &[u8] = b"oracle: no sandboxed program may read this\n";
/// Files in the oracle: (name, contents).
const FILES: [(&str, &[u8]); 5] = [
    ("secret.txt", SECRET),
    ("picked.txt", b"picked in the open panel\n"),
    ("dropped.txt", b"dropped on the app\n"),
    ("pasted.txt", b"pasted into the app\n"),
    ("inherited.txt", b"inherited descriptor\n"),
];

/// A filesystem that counts the calls sandboxed processes make to it
/// that nothing they were given explains: to the secret, to the folder's
/// list or entries, or looking up a name other than the files tests hand
/// out (the picked, dropped, pasted and inherited ones).
struct OracleFs {
    inner: RamFs,
}

static HITS: AtomicU64 = AtomicU64::new(0);
/// The secret's inode.
static SECRET_INO: AtomicU64 = AtomicU64::new(u64::MAX);

fn touched(what: &str) {
    if let Some(s) = current() {
        HITS.fetch_add(1, Ordering::Relaxed);
        log!("sandbox", "ESCAPE: {} reached the oracle ({})", s.profile.id, what);
    }
}

/// A call on inode `ino`: an escape if it is the secret.
fn on(ino: Ino, what: &str) {
    if ino == SECRET_INO.load(Ordering::Relaxed) {
        touched(what);
    }
}

impl Filesystem for OracleFs {
    fn describe(&self) -> String {
        String::from("the sandbox oracle (RamFS)")
    }
    fn root(&self) -> Ino {
        self.inner.root()
    }
    fn lookup(&self, dir: Ino, name: &str) -> FsResult<Ino> {
        if !FILES.iter().any(|(n, _)| *n == name && *n != "secret.txt") {
            touched("lookup");
        }
        self.inner.lookup(dir, name)
    }
    fn metadata(&self, ino: Ino) -> FsResult<Metadata> {
        on(ino, "metadata");
        self.inner.metadata(ino)
    }
    fn read(&self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        on(ino, "read");
        self.inner.read(ino, offset, buf)
    }
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        on(ino, "write");
        self.inner.write(ino, offset, data)
    }
    fn truncate(&self, ino: Ino, size: u64) -> FsResult<()> {
        on(ino, "truncate");
        self.inner.truncate(ino, size)
    }
    fn readdir(&self, dir: Ino) -> FsResult<Vec<DirEntry>> {
        touched("readdir");
        self.inner.readdir(dir)
    }
    fn create(&self, dir: Ino, name: &str, kind: FileKind) -> FsResult<Ino> {
        touched("create");
        self.inner.create(dir, name, kind)
    }
    fn unlink(&self, dir: Ino, name: &str) -> FsResult<()> {
        touched("unlink");
        self.inner.unlink(dir, name)
    }
    fn rename(&self, from_dir: Ino, from: &str, to_dir: Ino, to: &str) -> FsResult<()> {
        touched("rename");
        self.inner.rename(from_dir, from, to_dir, to)
    }
    fn owner(&self, ino: Ino) -> Option<(Owner, bool)> {
        on(ino, "owner");
        self.inner.owner(ino)
    }
    fn set_owner(&self, ino: Ino, owner: Owner) -> FsResult<()> {
        touched("set_owner");
        self.inner.set_owner(ino, owner)
    }
    fn create_owned(&self, dir: Ino, name: &str, kind: FileKind, owner: Option<Owner>) -> FsResult<Ino> {
        touched("create");
        self.inner.create_owned(dir, name, kind, owner)
    }
}

/// Mounts the oracle (once) and puts its files back as they were.
fn oracle() {
    static MOUNTED: AtomicBool = AtomicBool::new(false);
    if !MOUNTED.swap(true, Ordering::AcqRel) {
        fs::mount(ORACLE, Arc::new(OracleFs { inner: RamFs::new() }));
    }
    for (name, data) in FILES {
        fs::write_all(&format!("{ORACLE}/{name}"), data).expect("write the oracle");
    }
    let (_, ino) = fs::lookup(&format!("{ORACLE}/secret.txt")).unwrap();
    SECRET_INO.store(ino, Ordering::Relaxed);
}

/// Nothing sandboxed reached the oracle, and its secret is as it was.
fn oracle_intact(what: &str) {
    assert_eq!(HITS.load(Ordering::Relaxed), 0, "{what}: a sandboxed program reached the oracle (see ESCAPE above)");
    assert_eq!(fs::read_all(&format!("{ORACLE}/secret.txt")).ok().as_deref(), Some(SECRET), "{what}: the secret changed");
    let names: Vec<String> = fs::readdir(ORACLE).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names.len(), FILES.len(), "{what}: the oracle's folder changed: {names:?}");
    volume_ok(what);
}

/// The home volume is consistent after what ran in a sandbox.
fn volume_ok(what: &str) {
    fs::sync_all();
    let (root, _) = fs::lookup("/").unwrap();
    if let Err(e) = root.check() {
        panic!("{what}: the home volume is not consistent: {e}");
    }
}

fn uid() -> u32 {
    crate::users::session().uid
}

fn sandbox(id: &str, declared: u64) -> Arc<Sandbox> {
    Sandbox::new(Profile::for_test(id, declared, Kind::Package, uid()))
}

/// The policy on its own: paths, folders and consent, grants, restricting.
pub fn policy() {
    oracle();
    consent::set_auto_answer(None);
    let s = sandbox("org.waveos.test.policy", cap::DOCUMENTS_READ | cap::NET_CLIENT);
    let p = s.profile.clone();
    let home = p.home.clone();
    let doc = join(&home, "Documents/policy.txt");
    let ok = |path: &str, a: Access| s.check_path(path, a).is_ok();
    // The system image (read-only) and the container (everything).
    assert!(ok("/System/version.txt", Access::Read) && !ok("/System/version.txt", Access::Write));
    assert!(ok(&join(&p.container, "x/y"), Access::Write), "the container");
    assert!(fs::is_dir(&join(&p.container, "Preferences")), "the container's layout");
    // Folders on the way to it can be looked at, not listed.
    assert!(ok("/", Access::Stat) && !ok("/", Access::Read));
    assert!(!ok(ORACLE, Access::Stat) && !ok(&format!("{ORACLE}/secret.txt"), Access::Read));
    // Another app's container, even with all of the home folder.
    let other = join(&home, "Library/Containers/org.waveos.other/x");
    assert!(!ok(&other, Access::Read));
    let all = sandbox("org.waveos.test.policy-home", cap::HOME);
    consent::decide(uid(), &all.profile.id, cap::HOME, Some(true));
    assert!(all.check_path(&doc, Access::Write).is_ok() && all.check_path(&other, Access::Read).is_err());
    // A guarded folder: nobody to ask means no; asked and allowed means
    // yes (remembered); declared read-only stays read-only.
    assert!(!ok(&doc, Access::Read), "Documents without an answer");
    assert!(!ok(&join(&home, "Downloads/x"), Access::Read), "an undeclared folder");
    consent::set_auto_answer(Some(true));
    assert!(ok(&doc, Access::Read) && !ok(&doc, Access::Write));
    consent::set_auto_answer(None);
    assert_eq!(consent::decisions(uid(), &p.id).0 & cap::DOCUMENTS_READ, cap::DOCUMENTS_READ, "remembered");
    assert!(ok(&doc, Access::Read), "remembered without asking");
    // Taken away in Settings: refused at once.
    consent::decide(uid(), &p.id, cap::DOCUMENTS_READ, Some(false));
    assert!(!ok(&doc, Access::Read), "switched off");
    let saved = fs::read_all(&format!("/Security/Privacy/{}.conf", uid())).expect("the decisions are kept");
    let line = format!("decision\t{}\tdocuments:read\tdeny", p.id);
    assert!(String::from_utf8_lossy(&saved).contains(&line), "the decision is on disk");
    // Unguarded and declared: allowed until switched off.
    assert!(s.require(cap::NET_CLIENT).is_ok() && s.require(cap::NOTIFY).is_err());
    consent::decide(uid(), &p.id, cap::NET_CLIENT, Some(false));
    assert!(s.require(cap::NET_CLIENT).is_err());
    consent::decide(uid(), &p.id, cap::NET_CLIENT, None);
    assert!(s.require(cap::NET_CLIENT).is_ok());
    // Given files: exactly them (a prefix is not a folder).
    s.grant(&format!("{ORACLE}/picked.txt"), false);
    assert!(ok(&format!("{ORACLE}/picked.txt"), Access::Read) && !ok(&format!("{ORACLE}/picked.txt"), Access::Write));
    assert!(!ok(&format!("{ORACLE}/picked.txt2"), Access::Read) && !ok(&format!("{ORACLE}/secret.txt"), Access::Read));
    s.grant(ORACLE, true);
    assert!(ok(&format!("{ORACLE}/a/b"), Access::Write));
    assert_eq!(s.grants().len(), 2);
    // Children copy; dropping is for good, and only for the one dropping.
    let child = s.child();
    child.restrict(!cap::NET_CLIENT);
    assert!(child.require(cap::NET_CLIENT).is_err() && s.require(cap::NET_CLIENT).is_ok());
    child.restrict(cap::ALL);
    assert!(child.require(cap::NET_CLIENT).is_err(), "rights never come back");
    assert!(child.check_path(&format!("{ORACLE}/a"), Access::Write).is_ok(), "grants are inherited");
    // The system's own apps: the folders they declare without asking,
    // devices only when allowed.
    let terminal = manifests::for_program("/System/Apps/Terminal.elf").expect("Terminal's manifest");
    assert!(terminal.kind == Kind::System && terminal.declared & cap::ALL_FILES != 0);
    let notes = Sandbox::new(Profile::new(&manifests::for_program("/System/Apps/Notes.elf").unwrap(), uid()));
    assert!(notes.check_path(&doc, Access::Write).is_ok(), "Notes may use the home folder");
    assert!(notes.require(cap::MICROPHONE).is_err() && notes.require(cap::NET_CLIENT).is_err());
    assert!(manifests::system_apps().len() == 17, "every system app's manifest is valid");
    // Programs without a manifest: their account's files, devices asked.
    let legacy = Sandbox::new(Profile::new(&manifests::legacy("/Linux/usr/bin/steam"), uid()));
    assert!(legacy.check_path(&doc, Access::Write).is_ok() && legacy.require(cap::NET_SERVER).is_ok());
    assert!(legacy.require(cap::MICROPHONE).is_err(), "the microphone needs an answer");
    // Bookmarks belong to an app.
    let token = consent::add_bookmark(uid(), &p.id, "/x/y", true);
    assert!(consent::bookmark(uid(), &p.id, &token).is_some_and(|b| b.path == "/x/y" && b.write));
    assert!(consent::bookmark(uid(), "org.waveos.other", &token).is_none());
    assert!(consent::forget_bookmark(uid(), &p.id, &token) && consent::bookmark(uid(), &p.id, &token).is_none());
    // Settings' view.
    let list = syscall::privacy_list(uid());
    assert!(list.contains("app\torg.waveos.terminal\tTerminal\tsystem\t"), "Settings lists the system apps");
    assert!(syscall::privacy_set(uid(), "org.waveos.test.policy\tcamera\tallow").is_ok());
    assert!(syscall::privacy_set(uid(), "org.waveos.test.policy\tnonsense\tallow").is_err());
    for (bit, _) in [(cap::DOCUMENTS_READ, ()), (cap::CAMERA, ())] {
        consent::decide(uid(), &p.id, bit, None);
    }
    consent::decide(uid(), &all.profile.id, cap::HOME, None);
    oracle_intact("policy");
}

/// Runs `path` in `sandbox` (stdin: `stdin`); `step` sees the output so
/// far each time round (and may answer). Returns (exit code, output).
fn run(
    path: &str,
    argv: &[&str],
    sandbox: Arc<Sandbox>,
    stdin: Option<Arc<Handle>>,
    timeout_ms: u64,
    step: &mut dyn FnMut(u32, &str),
) -> (Option<i64>, String) {
    let (r, w) = crate::proc::new_pipe(false);
    let confine = Confine::With(Some(sandbox));
    let pid = crate::proc::spawn_confined(path, argv, None, [stdin, Some(w.clone()), Some(w)], 0, confine)
        .expect("start a sandboxed program");
    let p = crate::proc::get(pid).unwrap();
    let reader = match &*r {
        Handle::PipeRead { pipe, .. } => pipe.clone(),
        _ => unreachable!(),
    };
    let deadline = crate::time::uptime_ms() + timeout_ms;
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        step(pid, &String::from_utf8_lossy(&out));
        if reader.readable() {
            match reader.read(&mut buf, true) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        } else if p.exit_code().is_some() || crate::time::uptime_ms() > deadline {
            break;
        } else {
            sched::sleep_ms(10);
        }
        // No compositor runs during the tests: answer what windows ask.
        for cmd in crate::gui::server::take_commands() {
            if let crate::gui::server::Command::Present(id, _) = cmd {
                crate::gui::server::frame_done(id);
            }
        }
    }
    if p.exit_code().is_none() {
        let _ = crate::proc::kill(pid);
    }
    let text = String::from_utf8_lossy(&out).to_string();
    for line in text.lines() {
        crate::kprintln!("    {line}");
    }
    (p.exit_code(), text)
}

/// A file of the oracle opened by the kernel, to hand down as stdin.
fn inherited() -> Arc<Handle> {
    let f = fs::open(&format!("{ORACLE}/inherited.txt"), aurora_abi::open::READ).unwrap();
    Arc::new(Handle::File(crate::sync::Mutex::new(f)))
}

/// An unconfined process to try to signal.
fn victim() -> u32 {
    crate::proc::spawn("/System/Bin/fuzz", &["fuzz", "victim", "sleep"], [None, None, None], 0).expect("victim")
}

/// `sbxtest`: refused everything it did not declare; given what the person
/// gives; a bookmark across runs; allowed, then switched off while running.
pub fn programs() {
    oracle();
    let victim = victim();
    let v = format!("{victim}");
    let sbx = "/System/Bin/sbxtest";
    let (code, _) = run(sbx, &["sbxtest", "deny", &v], sandbox("org.waveos.test.sbx", 0), Some(inherited()), 60_000, &mut |_, _| {});
    assert_eq!(code, Some(0), "sbxtest deny");
    assert!(crate::proc::get(victim).is_some_and(|p| p.exit_code().is_none()), "the victim lives");

    // What the person gives: the panel, a drop, a paste (it is in front).
    crate::sandbox::panel::set_auto_answer(Some(Some(format!("{ORACLE}/picked.txt"))));
    let pasted = format!("{ORACLE}/pasted.txt");
    crate::gui::clipboard::set(aurora_abi::clip::FILES, pasted.as_bytes()).unwrap();
    let mut started = false;
    let (code, out) = run(sbx, &["sbxtest", "grants"], sandbox("org.waveos.test.sbx", 0), None, 60_000, &mut |pid, _| {
        // In front throughout: a window of an earlier program going away
        // meanwhile would otherwise take the focus back to nothing.
        crate::gui::server::set_frontmost_pid(pid);
        if !started {
            started = true;
            crate::gui::dnd::deliver(pid, format!("{ORACLE}/dropped.txt").into_bytes(), true);
        }
    });
    crate::sandbox::panel::set_auto_answer(None);
    crate::gui::server::set_frontmost_pid(0);
    assert_eq!(code, Some(0), "sbxtest grants");
    let token = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("sbxtest: token "))
        .map(String::from)
        .expect("a bookmark token");
    // The next run of the same app brings the file back with the token.
    let (code, _) = run(sbx, &["sbxtest", "resolve", &token], sandbox("org.waveos.test.sbx", 0), None, 60_000, &mut |_, _| {});
    assert_eq!(code, Some(0), "sbxtest resolve");
    // Another app can't use that token.
    assert!(consent::bookmark(uid(), "org.waveos.test.sbx-allow", &token).is_none());

    // Declared and allowed; then Documents is switched off while it runs.
    let id = "org.waveos.test.sbx-allow";
    let declared = cap::NET_CLIENT | cap::DOCUMENTS | cap::NOTIFY | cap::SPAWN | cap::AUDIO_OUT;
    consent::set_auto_answer(Some(true));
    let (ir, iw) = crate::proc::new_pipe(false);
    let mut stdin_writer = Some(iw);
    let (code, _) = run(sbx, &["sbxtest", "allow"], sandbox(id, declared), Some(ir), 60_000, &mut |_, out| {
        if out.contains("sbxtest: ready") {
            if let Some(w) = stdin_writer.take() {
                consent::decide(uid(), id, cap::DOCUMENTS, Some(false));
                if let Handle::PipeWrite(p) = &*w {
                    let _ = p.write(b"go\n");
                }
            }
        }
    });
    consent::set_auto_answer(None);
    assert_eq!(code, Some(0), "sbxtest allow");
    consent::decide(uid(), id, cap::DOCUMENTS, None);
    let _ = fs::unlink(&join(&home_of(uid()), "Documents/sbxtest.txt"));
    let _ = crate::proc::kill(victim);
    oracle_intact("sbxtest");
}

/// Apps without `background` end once they have had no window for a
/// while — but not programs still at work: one with a program of its own
/// running, one that never had a window, one that may run on.
pub fn background() {
    let sbx = "/System/Bin/sbxtest";
    let started = crate::time::uptime_ms();
    let (code, _) = run(sbx, &["sbxtest", "window", "40"], sandbox("org.waveos.test.bg", 0), None, 90_000, &mut |_, _| {});
    let took = crate::time::uptime_ms() - started;
    assert_eq!(code, Some(crate::proc::KILLED_CODE), "a windowless app without background was not ended");
    assert!(took < 35_000, "it was ended after {took} ms");
    let cases: [(&str, &[&str], u64); 3] = [
        ("background", &["sbxtest", "window", "15"], cap::BACKGROUND),
        ("a program of its own running", &["sbxtest", "window-child", "15"], 0),
        ("never had a window", &["sbxtest", "sleep", "15"], 0),
    ];
    for (what, argv, declared) in cases {
        let (code, _) = run(sbx, argv, sandbox("org.waveos.test.bg", declared), None, 90_000, &mut |_, _| {});
        assert_eq!(code, Some(0), "ended although {what}");
    }
    volume_ok("background");
}

/// `sandboxtest`, a Linux program, tries every way out it knows.
pub fn linux() {
    use crate::compat::linux::{links, shm, unix};
    let path = "/System/Linux/bin/sandboxtest";
    if fs::stat(path).is_err() {
        crate::kprint!("(skipped: built without musl-gcc) ");
        return;
    }
    oracle();
    let victim = victim();
    let v = format!("{victim}");
    // The system's own socket and shared memory, and a link to the oracle
    // someone else planted where the program keeps its temporary files.
    let public = unix::Socket::new(unix::Type::Stream);
    public.bind(String::from("@sbx-public")).expect("bind");
    public.listen().expect("listen");
    let _shm = shm::open("sbx-public", true, false, false).expect("shared memory");
    // (The writable Linux tree, where the link table lives, may be new.)
    crate::compat::linux::fs::prepare_root(&crate::compat::linux::fs::account_user(), uid());
    let _ = links::remove("/tmp/sbx-planted");
    links::create("/tmp/sbx-planted", &format!("{ORACLE}/secret.txt")).expect("plant a link");
    let s = sandbox("org.waveos.test.sbx-linux", cap::LINUX);
    let container = s.profile.container.clone();
    let (code, out) = run(path, &["sandboxtest", &v], s, Some(inherited()), 180_000, &mut |_, _| {});
    let _ = links::remove("/tmp/sbx-planted");
    let _ = shm::unlink("sbx-public");
    let _ = crate::proc::kill(victim);
    assert_eq!(code, Some(0), "sandboxtest failed");
    assert!(out.contains("sandbox-test passed"), "sandboxtest did not finish");
    // Its /tmp and home folder were in its container.
    assert!(fs::stat(&format!("{container}/tmp/sbx-own.txt")).is_ok(), "/tmp is in the container");
    assert!(fs::stat(&format!("{container}/Home/sbx-home.txt")).is_ok(), "the home folder is in the container");
    oracle_intact("sandboxtest");
}

/// The native fuzzer in a sandbox, aimed at the oracle.
pub fn fuzz_native() {
    oracle();
    let opt = |k: &str, d: &str| crate::gui::prefs::boot_option(k).unwrap_or_else(|| String::from(d));
    let seed = opt("fuzz_seed", "1");
    let declared = cap::SPAWN | cap::NET_CLIENT | cap::NET_SERVER | cap::AUDIO_OUT | cap::NOTIFY | cap::GAMEPADS;
    let s = sandbox("org.waveos.test.fuzz", declared);
    // Its scratch folder is given to it.
    s.grant("/fuzz", true);
    let argv = ["fuzz", "run", "--seed", &seed, "--iters", "600", "--rounds", "2", "--oracle", ORACLE];
    let (code, _) = run("/System/Bin/fuzz", &argv, s, None, 20 * 60_000, &mut |_, _| {});
    assert_eq!(code, Some(0), "the sandboxed fuzzer reported failures");
    oracle_intact("the native fuzzer");
    let _ = fs::remove_tree("/fuzz");
    crate::kprint!("[seed {seed}] ");
}

/// The Linux fuzzer in a sandbox, aimed at the oracle.
pub fn fuzz_linux() {
    let path = "/System/Linux/bin/linuxfuzz";
    if fs::stat(path).is_err() {
        crate::kprint!("(skipped: built without musl-gcc) ");
        return;
    }
    oracle();
    let opt = |k: &str, d: &str| crate::gui::prefs::boot_option(k).unwrap_or_else(|| String::from(d));
    let seed = opt("fuzz_seed", "1");
    let declared = cap::LINUX | cap::SPAWN | cap::NET_CLIENT | cap::NET_SERVER | cap::AUDIO_OUT | cap::NOTIFY;
    let s = sandbox("org.waveos.test.linuxfuzz", declared);
    let argv = ["linuxfuzz", &seed, "3", "200", ORACLE];
    let tmp = format!("{}/tmp/lfz", s.profile.container);
    let (code, out) = run(path, &argv, s, None, 20 * 60_000, &mut |_, _| {});
    // What its own clean-up could not remove (a child may have taken the
    // permissions away): the next boot starts from the same place.
    let _ = fs::remove_tree(&tmp);
    assert_eq!(code, Some(0), "the sandboxed Linux fuzzer failed");
    assert!(out.contains("linuxfuzz passed"), "the sandboxed Linux fuzzer did not finish");
    oracle_intact("the Linux fuzzer");
}
