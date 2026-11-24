//! The processor architecture a build is for (`--arch x86_64|aarch64`).
//!
//! Everything that differs between the two lives here: compiler targets,
//! where the build output goes (so switching architecture never rebuilds
//! the other one), the EFI boot file's name, the disk a run uses by
//! default, and whether QEMU can run the architecture at full speed on
//! this computer. The QEMU command lines themselves are in `main.rs`
//! (x86-64, the `q35` machine) and `virt.rs` (aarch64, the `virt` machine).

use std::env;
use std::path::PathBuf;
use std::process::exit;
use std::sync::OnceLock;

/// Whether `--arch` defaults to this computer's own architecture where QEMU
/// can run it with hardware virtualization (aarch64 on Apple-silicon Macs
/// and on arm64 Linux with KVM). Off until the aarch64 port boots and passes
/// its tests: until then every build without `--arch` is x86_64. Turning it
/// on is the whole switch.
pub const DEFAULT_TO_ACCELERATED_HOST: bool = false;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    X86_64,
    Aarch64,
}

pub const ALL: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

impl Arch {
    /// `x86_64` or `aarch64`, as `--arch`, GINA manifests and disks name it.
    pub fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    pub fn parse(s: &str) -> Option<Arch> {
        match s {
            "x86_64" | "x86-64" | "amd64" => Some(Arch::X86_64),
            "aarch64" | "arm64" => Some(Arch::Aarch64),
            _ => None,
        }
    }

    /// The bootloader's target.
    pub fn uefi_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-uefi",
            Arch::Aarch64 => "aarch64-unknown-uefi",
        }
    }

    /// The kernel's target.
    pub fn kernel_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-none",
            Arch::Aarch64 => "aarch64-unknown-none",
        }
    }

    /// User programs' target: a spec in `userland/` (`<name>.json`).
    pub fn user_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-aurora-user",
            Arch::Aarch64 => "aarch64-aurora-user",
        }
    }

    /// Whether user space builds for this architecture yet (corekit's
    /// system calls and start-up code for aarch64 follow the kernel's).
    pub fn userland_ported(self) -> bool {
        self == Arch::X86_64
    }

    /// RUSTFLAGS for user programs (beside the linker script). Frame
    /// pointers everywhere, for the kernel's stack traces; on aarch64 also
    /// branch protection (return addresses signed with PAC, indirect branch
    /// targets marked for BTI), which are hint instructions on processors
    /// without those features.
    pub fn user_rustflags(self) -> &'static str {
        match self {
            Arch::X86_64 => "-C force-frame-pointers=yes",
            Arch::Aarch64 => "-C force-frame-pointers=yes -Z branch-protection=bti,pac-ret",
        }
    }

    /// The removable-media boot file UEFI firmware starts (`\EFI\BOOT\…`).
    pub fn boot_file(self) -> &'static str {
        match self {
            Arch::X86_64 => "BOOTX64.EFI",
            Arch::Aarch64 => "BOOTAA64.EFI",
        }
    }

    /// What a file name ends with for this architecture: nothing for x86_64,
    /// whose paths predate the port, `-aarch64` otherwise.
    pub fn suffix(self) -> &'static str {
        match self {
            Arch::X86_64 => "",
            Arch::Aarch64 => "-aarch64",
        }
    }

    /// `target/esp` (`target/esp-test` with the self-tests), with [`Self::suffix`].
    pub fn esp_dir(self, root: &std::path::Path, test: bool) -> PathBuf {
        root.join(format!("target/esp{}{}", if test { "-test" } else { "" }, self.suffix()))
    }

    /// Where cargo puts user space (`target/user`, with [`Self::suffix`]).
    pub fn user_target_dir(self, root: &std::path::Path) -> PathBuf {
        root.join(format!("target/user{}", self.suffix()))
    }

    /// `target/<stem>.img` with [`Self::suffix`]: the x86 disk keeps its
    /// old name, so an ARM build never picks up the x86 disk by default.
    pub fn disk(self, root: &std::path::Path, stem: &str) -> PathBuf {
        root.join(format!("target/{stem}{}.img", self.suffix()))
    }

    /// QEMU's program for this architecture.
    pub fn qemu_binary(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::Aarch64 => "qemu-system-aarch64",
        }
    }

    /// The architecture of the computer xtask runs on, if it is one of ours.
    pub fn host() -> Option<Arch> {
        if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else {
            None
        }
    }

    /// Whether QEMU can run this architecture with the host processor's own
    /// virtualization (HVF on macOS, KVM on Linux, WHPX on Windows) rather
    /// than by translating every instruction.
    pub fn accelerated_here(self) -> bool {
        if Arch::host() != Some(self) {
            return false;
        }
        if cfg!(target_os = "linux") {
            std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok()
        } else {
            // HVF on macOS (every Apple-silicon and Intel Mac), WHPX on Windows.
            cfg!(target_os = "macos") || (cfg!(target_os = "windows") && self == Arch::X86_64)
        }
    }

    /// One line for the launcher and the console: what this choice costs.
    pub fn speed_note(self) -> &'static str {
        if self.accelerated_here() {
            "Runs at full speed on this computer"
        } else {
            "Emulated, slow"
        }
    }

    /// The default without `--arch` (see [`DEFAULT_TO_ACCELERATED_HOST`]).
    pub fn default_choice() -> Arch {
        match Arch::host() {
            Some(host) if DEFAULT_TO_ACCELERATED_HOST && host.accelerated_here() => host,
            _ => Arch::X86_64,
        }
    }
}

static SELECTED: OnceLock<Arch> = OnceLock::new();

/// Reads `--arch NAME` (or `AURORA_ARCH`) from the command line once, at
/// start-up; every later [`current`] answers with it.
pub fn select_from_args(args: &[String]) -> Arch {
    let named = match args.iter().position(|a| a == "--arch") {
        Some(i) => Some(args.get(i + 1).cloned().unwrap_or_default()),
        None => env::var("AURORA_ARCH").ok().filter(|v| !v.is_empty()),
    };
    let arch = match named {
        None => Arch::default_choice(),
        Some(name) => Arch::parse(&name).unwrap_or_else(|| {
            eprintln!("--arch must be x86_64 or aarch64");
            exit(2)
        }),
    };
    // Programs this one starts (the launcher's `xtask run`) follow it.
    // Before any thread exists: nothing reads the environment concurrently.
    env::set_var("AURORA_ARCH", arch.name());
    let _ = SELECTED.set(arch);
    current()
}

/// The architecture this run of xtask builds and boots.
pub fn current() -> Arch {
    *SELECTED.get_or_init(Arch::default_choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn names_and_paths_keep_the_x86_layout() {
        let root = Path::new("/r");
        assert_eq!(Arch::X86_64.esp_dir(root, false), Path::new("/r/target/esp"));
        assert_eq!(Arch::X86_64.esp_dir(root, true), Path::new("/r/target/esp-test"));
        assert_eq!(Arch::X86_64.disk(root, "waveos-aurora"), Path::new("/r/target/waveos-aurora.img"));
        assert_eq!(Arch::X86_64.user_target_dir(root), Path::new("/r/target/user"));
        assert_eq!(Arch::Aarch64.esp_dir(root, false), Path::new("/r/target/esp-aarch64"));
        assert_eq!(Arch::Aarch64.esp_dir(root, true), Path::new("/r/target/esp-test-aarch64"));
        assert_eq!(Arch::Aarch64.disk(root, "waveos-aurora"), Path::new("/r/target/waveos-aurora-aarch64.img"));
        assert_eq!(Arch::Aarch64.user_target_dir(root), Path::new("/r/target/user-aarch64"));
        for a in ALL {
            assert_eq!(Arch::parse(a.name()), Some(a));
        }
        assert_eq!(Arch::parse("arm64"), Some(Arch::Aarch64));
        assert_eq!(Arch::parse("riscv64"), None);
        assert_eq!(Arch::Aarch64.boot_file(), "BOOTAA64.EFI");
    }

    #[test]
    fn the_default_stays_x86_until_switched() {
        if !DEFAULT_TO_ACCELERATED_HOST {
            assert_eq!(Arch::default_choice(), Arch::X86_64);
        }
    }
}
