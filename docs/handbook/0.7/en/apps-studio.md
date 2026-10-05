# Constellation Studio
Create and edit native app projects and simple layouts.

## Use the app
1. Open Constellation Studio and create a project under `/Documents/Projects`.
2. Edit handwritten Rust in `src/main.rs`.
3. Use Design to edit `layouts/main.ui`; generated code belongs in `src/generated_ui.rs`.
4. Save the project. Export/package requires a real executable at `build/app.elf`.

## Expected result and troubleshooting
Native Rust/Cargo/LLVM, semantic IDE tooling, and a source debugger are not ported. Build & Run reports the missing toolchain; it does not simulate success. The bundled gallery is compiled on the host.

See [Desktop and windows](desktop-windows.md) for shared shortcuts and [Recovery](reference-recovery.md) if the app cannot open.
