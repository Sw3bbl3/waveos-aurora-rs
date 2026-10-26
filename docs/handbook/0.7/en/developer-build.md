# Building and testing
Build on macOS or Linux with Rust/rustup, QEMU, and OVMF. ISO packaging additionally requires xorriso (`brew install xorriso` or `sudo apt install xorriso`).

## Build the system
1. Use the dated toolchain in rust-toolchain.toml. `tools/toolchain-baseline.toml` records the expected Rust commit and LLVM version; xtask rejects a mismatch.
2. Run `cargo xtask docs` to validate and compile both handbook languages.
3. Run `cargo xtask build` to compile the bootloader, kernel, and userland.
4. Run `cargo xtask run` to boot the persistent desktop.
5. Run `cargo xtask image` for a clean USB/disk image or `cargo xtask iso` for an optical image.

## Run checks
```sh
cargo xtask lint
cargo test -p aurora-help -p gina -p aurorafs -p fat32 -p lumen -p aurora-image -p aurora-wav -p nebula-web -p nebula-secure -p nebula-engine -p nebula-script -p nebula-media -p aurora-elf -p aurora-mutate -p xtask
cargo xtask test --disk ahci --smp 4 --net virtio
cargo xtask test --disk virtio --smp 4 --net virtio
cargo xtask test --disk nvme --smp 4 --net e1000e
```
`cargo xtask lint` runs Clippy over the host libraries, the kernel, the bootloader, and userland, each for its own target, and treats every warning as an error. The kernel harness boots the test disk twice to verify persistence. Also exercise the single-CPU configuration. Run root and userland `cargo fmt --all -- --check` before contributing.

## Robustness tests
Every parser of outside data (pictures, fonts, video, web pages, scripts, certificates, packages, programs, and FAT32 and AuroraFS volumes) has a test that feeds it thousands of corrupted copies of valid input and fails on any panic. Runs are reproducible, and a failure names the input that caused it. Set `AURORA_MUTATIONS=100` to run each one a hundred times longer while working on a parser.

## Troubleshooting
The userland workspace uses a custom target and linker script; build through xtask. If the compiler does not match, install the pinned toolchain rather than silently changing the baseline. Check serial output before treating a visible desktop as a complete test pass.
