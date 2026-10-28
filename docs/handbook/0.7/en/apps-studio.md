# Studio
Create and edit WaveUI app projects and simple layouts with the WaveSDK.

## Use the app
1. Open Studio and create a project under `/Documents/Projects`.
2. Edit handwritten Rust in `src/main.rs`. New projects start as a small [WaveUI](developer-waveui.md) app.
3. Use Design to edit `layouts/main.ui`; Studio writes the matching WaveUI code to `src/generated_ui.rs`.
4. Save the project. Export/package requires a real executable at `build/app.elf`.

## Expected result and troubleshooting
Native Rust/Cargo/LLVM, semantic IDE tooling, and a source debugger are not ported. Build & Run reports the missing toolchain; it does not simulate success. The bundled gallery is compiled on the host.

See [Desktop and windows](desktop-windows.md) for shared shortcuts and [Recovery](reference-recovery.md) if the app cannot open.
