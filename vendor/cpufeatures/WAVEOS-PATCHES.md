Vendored from cpufeatures 0.2.17 (MIT OR Apache-2.0), the RustCrypto
crates' CPU feature detection.

WaveOS changes, in `src/aarch64.rs` only:

- On `target_os = "none"` (WaveOS programs for aarch64) detection asks a
  function the program's runtime installs with `aarch64::set_detector`
  (corekit does, at start-up; see `corekit::sys::cpu_has`). The crate had
  no detection there and answered "no" for every feature, so the `aes`
  and `polyval` crates could never use the ARMv8 AES and PMULL
  instructions in WaveOS, however the processor was. Without a detector it
  still answers "no", and every crate keeps its constant-time portable
  code.

x86 and the other targets are unchanged. `userland/Cargo.toml` patches the
crate with this copy; the main workspace (kernel, host tools) uses the
published crate, whose behaviour on its targets is the same.
