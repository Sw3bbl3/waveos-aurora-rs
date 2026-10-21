# Nebula script startup and collector fixes — October 10, 2026

This change fixes premature collection of objects retained by the browser or promise runtime and reduces script parsing/compilation work. It does not establish Safari-equivalent browsing or working YouTube video playback.

## Host measurements

Release build, same saved source before and after; median of seven samples per bundle, sorted by combined parse/compile duration. These timings exclude execution, networking, rendering, and destruction of the compiled program. They run on the macOS host, not in the emulated guest.

| Saved script | Bytes | Before parse + compile | After parse + compile |
|---|---:|---:|---:|
| TikTok `foryou-prefetch.687ae4eb.js` | 411,013 | 45.269 ms | 24.141 ms |
| YouTube `kevlar_base_module,kevlar_main_module,kevlar_base_sync_mod_chunk` | 10,875,499 | 3,022.217 ms | 559.394 ms |

The compiler now indexes scopes with at least 16 bindings, eliminating repeated linear searches through large bundles. The tokenizer narrows punctuation candidates and avoids temporary allocations for ASCII identifiers. Smaller scopes retain their existing lookup strategy.

Local inputs and logs: `target/nebula-performance/{compile-before,youtube-before,compile-after}.log`. Downloaded scripts are not checked into source control. SHA-256:

- TikTok: `102730682de6aef1070fc884fd132475c16857b9c587489a96483e3343eddd44`
- YouTube: `01b593deecb67b5885dba7b3c54c5bce4d997456b39db6b343c4112e3033d1f2`

## Correctness

Five regression cases failed before their respective fixes: animation-frame batch collection, an event collected during its microtask checkpoint, an active promise reaction, a throwing thenable, and rejection reporting that executes JavaScript. The last three reproduced `heap.rs: use of a collected object`. All pass with scoped roots; garbage collection remains enabled.

`cargo test -p nebula-script -p nebula-engine`: 77 tests passed; one documentation example ignored. Additional coverage checks large-scope shadowing/captures/redeclarations, longest-match punctuation, escaped/Unicode identifiers, and invalid numeric private names. `cargo xtask build` produces the updated Nebula and system image.

## Guest check and remaining limits

A separate 4 GiB QEMU guest disk at `target/nebula-performance/runtime.img` was used; the personal disk was not a validation target. A local test page allocated transient objects in promise handlers, a throwing thenable, a DOMContentLoaded microtask, and the first of two animation callbacks. All five completion markers appeared and the page displayed “5 checks passed.” Evidence: `target/nebula-performance/serial-final.log` and `gc.png`.

The final rebuilt image was booted again and passed all five markers; `serial-verified.log` records 4,069 MiB usable guest RAM and `gc-final.png` captures the result. The temporary VM and local fixture server were stopped after validation.

The live YouTube check recorded first paint at 2,201 ms and script-load completion at 10,819 ms, but reported `HTMLTemplateElement is not defined`. This is a failed functional YouTube check, not a successful page load or playback result. There is no guest before/after speed claim. The reported TikTok regex parsing error and unsupported JavaScript modules are not addressed by this change. Both browser compatibility and emulated execution remain limitations.

The live TikTok check completed its initial script-load phase in 7,168 ms and progressed past the delayed recommendation-cache callback without the reported collector panic. It still reported the regex/module errors and then a missing `HTMLMediaElement`. This is a short crash-path check, not complete TikTok functionality or a long-duration stability test. Guest resolution was 1280×800; the user's supplied log was 1920×1200, so those timings are not a controlled comparison.
