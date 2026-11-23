//! Full browser checks always use an isolated disk with verified dependencies.
use std::{
    fs,
    path::PathBuf,
    process::{exit, Command},
};

pub fn run(args: &[String]) {
    let value = |key: &str| args.iter().position(|a| a == key).and_then(|n| args.get(n + 1)).cloned();
    let net = value("--net").unwrap_or_else(|| "virtio".into());
    assert!(["virtio", "e1000", "e1000e", "none"].contains(&net.as_str()), "unsupported --net");
    let output = value("--output")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::root().join(format!("target/network-web/check-{net}")));
    fs::create_dir_all(&output).expect("create web-check output");
    let output = output.canonicalize().unwrap();
    let fixture = output.join("fixture");
    let ready = Command::new("python3")
        .arg(super::root().join("tools/web_fixture.py"))
        .arg("--directory")
        .arg(&fixture)
        .arg("--prepare")
        .status()
        .expect("prepare browser fixtures");
    assert!(ready.success(), "could not create local TLS fixtures (openssl is required)");
    // This root exists only in the disposable check image. Normal run/build
    // regenerates its certificate bundle without this environment variable.
    std::env::set_var("WAVEOS_EXTRA_CA", fixture.join("ca.pem"));
    let esp = if args.iter().any(|a| a == "--no-build") {
        super::esp_dir(false)
    } else {
        super::build(super::Profile::Release, false)
    };
    let linux = super::linux_root();
    assert!(super::voyage::build(&linux).is_some(), "web-check requires a successfully built Voyage");
    for path in [
        "usr/lib/voyage/voyage",
        "usr/lib/firefox-esr/firefox-esr",
        "etc/ssl/certs/ca-certificates.crt",
        "usr/share/glib-2.0/schemas/gschemas.compiled",
    ] {
        assert!(linux.join(path).is_file(), "web-check dependency missing: {path}");
    }
    let disk = output.join("session.img");
    let baseline = value("--baseline-image");
    if let Some(source) = &baseline {
        assert!(!disk.exists(), "use a new --output directory for a baseline clone");
        let copied = Command::new("cp")
            .args(if cfg!(target_os = "macos") { vec!["-c"] } else { vec!["--reflink=auto"] })
            .arg(source)
            .arg(&disk)
            .status()
            .expect("clone baseline disk");
        assert!(copied.success(), "could not clone the baseline disk");
        let settings = super::debian::waveos_settings(&linux);
        let state = super::image::add_linux(&disk, &linux, &settings).expect("update baseline browser instrumentation");
        assert!(
            !matches!(state, super::image::LinuxOnDisk::NoRoom { .. }),
            "baseline image has no room for browser dependencies"
        );
    } else {
        super::prepare_disk(&esp, &disk, false, 3072 * 1024 * 1024, Some(&linux));
    }
    let mut command = Command::new("python3");
    command.arg(super::root().join("tools/smoke_web.py")).arg("--output").arg(&output).arg("--net").arg(net);
    if baseline.is_some() {
        command.arg("--baseline");
    }
    if args.iter().any(|a| a == "--local-only") {
        command.arg("--local-only");
    }
    if args.iter().any(|a| a == "--firefox") {
        command.arg("--firefox");
    }
    if args.iter().any(|a| a == "--benchmark-only") {
        command.arg("--benchmark-only");
    }
    if args.iter().any(|a| a == "--public-only") {
        command.arg("--public-only");
    }
    if args.iter().any(|a| a == "--connection-only") {
        command.arg("--connection-only");
    }
    if let Some(repetitions) = value("--repetitions") {
        command.arg("--repetitions").arg(repetitions);
    }
    if let Some(checks) = value("--only") {
        command.arg("--only").arg(checks);
    }
    let status = command.status().expect("run native browser checks");
    exit(status.code().unwrap_or(1));
}
