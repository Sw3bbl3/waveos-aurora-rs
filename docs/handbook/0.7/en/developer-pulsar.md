# Pulsar and media development
Develop and test JavaScript and the media libraries on the host, then verify JavaScript in the actual guest. Prerequisites: the pinned Rust toolchain and the build setup in [Building and testing](developer-build.md).

## Run JavaScript
1. Open Terminal in WaveOS and run `js` for an interactive prompt, or `js /Documents/example.js` for a saved script.
2. On the host, run `cargo run --release -p nebula-script --example js -- FILE.js`. Add `--dump` to inspect bytecode.

Pulsar compiles to bytecode and interprets it with a garbage collector. Ordinary large-script functions can compile lazily. Property caches, interned identifiers and a cross-tab compiled-script cache reduce repeated work; they do not provide universal web compatibility. Host code must root retained JavaScript values across callbacks and microtasks with `Realm::with_roots`.

## Validate engine and media changes
1. Run `cargo test -p nebula-script -p nebula-engine -p nebula-media` on the host.
2. Use the CC0 MP4 and generated AAC fixtures under `libs/media/tests/fixtures`; their provenance is documented there. Test invalid and truncated input as well as valid decoding.
3. Build with `cargo xtask build`, boot a separate disposable disk and run your scripts with `js` in Terminal.

Expected result: the host suite passes and scripts behave the same in the guest. The web browsers, [Firefox](apps-firefox.md) (with its own JavaScript engine) and [NetSurf](apps-netsurf.md), are separate programs; the Nebula engine and Pulsar no longer run web pages in the guest. Re-run conformance tests after changing the runtime; historical test262 snapshots do not describe every newer change.

## Troubleshooting and limits
Keep script execution, parsing/compilation, layout, networking and media decoding measurements separate. Do not infer a browser responsiveness gain from a host compiler benchmark. JavaScript modules, BigInt, Intl, DRM and broad streaming-site compatibility remain incomplete. The native Rust compiler/debugger is still unavailable; JavaScript execution does not supply that toolchain.

See [Firefox](apps-firefox.md) for the web browser and [Validation](reference-validation.md) for measurement requirements.
