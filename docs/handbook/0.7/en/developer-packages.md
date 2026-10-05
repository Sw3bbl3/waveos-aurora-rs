# GINA package format
A .gina package is a restricted, uncompressed ustar archive with manifest.toml at its root. The validator enforces paths, archive checksums, bounds, required resources, x86-64 architecture, SDK compatibility, and ABI 1. Packages are limited to 64 MiB and 1024 entries.

## Package a compiled app
1. Build a real WaveOS userland executable on the host.
2. Create a manifest using flat strings, integers, and string arrays:
```toml
format = 1
id = "dev.example.myapp"
name = "My App"
developer = "Example Developer"
version = "1.0.0"
architecture = "x86_64"
sdk = "0.7"
abi = 1
entry = "bin/app"
icon = ""
resources = []
extensions = []
```
3. Run `cargo xtask package manifest.toml app.elf MyApp.gina`.
4. Transfer the package to WaveOS and open it in GINA Apps.

## Native package commands
Use `constellation inspect PACKAGE`, `constellation install PACKAGE`, `constellation list`, and `constellation launch APP_ID`. `constellation new PROJECT APP_ID NAME` creates project sources; it does not install a compiler.

## Storage and update behavior
Payloads are staged under /Applications/APP_ID. An active registration selects a validated generation after writes are synced. /AppData/APP_ID survives updates and normal uninstall. Invalid updates leave the prior active app selected. Signatures, permission enforcement, and isolation between app-data directories remain future work.
