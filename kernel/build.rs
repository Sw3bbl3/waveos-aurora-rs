//! Kernel build script: passes the linker script.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=--script={dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=build.rs");
}
