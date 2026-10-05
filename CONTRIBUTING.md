# Contributing to WaveOS

Preserve existing working-tree changes and persistent disks. Never use a personal VM disk as a release artifact.

For each system, feature, or application change:

1. Update the relevant English and Canadian French handbook articles together. Keep existing guide URLs working.
2. Add concise user-facing update notes using `docs/releases/TEMPLATE.md`. The first 0.7 release includes the full handbook; later notes describe the delta.
3. Record executed tests and any limitations. Performance claims require a named baseline, identical workload/configuration, warm-up, at least five measured repetitions, raw samples, median/p95 and a direct evidence link. Distinguish guest composition time, client work, memory and end-to-end latency.
4. Run `cargo xtask docs`, both workspace formatting checks, the relevant host tests, and required kernel/boot CI. Never remove checks to make a release appear green.
5. Publish only from an identified tested commit. Re-download artifacts and verify their SHA-256 checksums after publication.

Use the dated toolchain in `rust-toolchain.toml`. Native compiler/debugger features are unfinished; host-built examples are not proof of native compilation.
