//! `cargo xtask video-check`: video playback in Firefox and Voyage on a
//! disposable disk, measured (tools/smoke_video.py), with clips made by
//! tools/video_fixture.py.
use std::{
    fs,
    path::PathBuf,
    process::{exit, Command},
};

pub fn run(args: &[String]) {
    let value = |key: &str| args.iter().position(|a| a == key).and_then(|n| args.get(n + 1)).cloned();
    let output = value("--output")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::root().join("target/video-check/run"));
    fs::create_dir_all(&output).expect("create video-check output");
    let output = output.canonicalize().unwrap();
    let clips = super::root().join("target/video-check/clips");
    let made = Command::new("python3")
        .arg(super::root().join("tools/video_fixture.py"))
        .arg("--directory")
        .arg(&clips)
        .arg("--prepare")
        .status()
        .expect("make the video clips");
    assert!(made.success(), "could not make the video clips (ffmpeg with libx264, libvpx-vp9, libsvtav1 and libopus)");
    let esp = if args.iter().any(|a| a == "--no-build") {
        super::esp_dir(false)
    } else {
        super::build(super::Profile::Release, false)
    };
    let linux = super::linux_root();
    assert!(super::voyage::build(&linux).is_some(), "video-check requires a successfully built Voyage");
    for path in ["usr/lib/voyage/voyage", "usr/lib/firefox-esr/firefox-esr"] {
        assert!(linux.join(path).is_file(), "video-check dependency missing: {path}");
    }
    let disk = output.join("session.img");
    super::prepare_disk(&esp, &disk, false, 3072 * 1024 * 1024, Some(&linux));
    let mut command = Command::new("python3");
    command.arg(super::root().join("tools/smoke_video.py")).arg("--output").arg(&output).arg("--clips").arg(&clips);
    for key in ["--browsers", "--cases", "--decode", "--secs", "--net", "--smp", "--min-fps", "--max-drift", "--max-gaps", "--label"] {
        if let Some(v) = value(key) {
            command.arg(key).arg(v);
        }
    }
    if args.iter().any(|a| a == "--muted") {
        command.arg("--muted");
    }
    let status = command.status().expect("run the video checks");
    exit(status.code().unwrap_or(1));
}
