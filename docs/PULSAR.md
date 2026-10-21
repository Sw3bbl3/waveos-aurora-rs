# Pulsar, the JavaScript engine

Pulsar is the JavaScript engine for Nebula, WaveOS's web browser. It is a `no_std` Rust library (`libs/script`, package `nebula-script`) with no dependencies beyond `libm`: developed and tested on the host, and linked into the browser and the `js` Terminal command.

## How it works

| Stage | File | What it does |
|---|---|---|
| Lexer | `lexer.rs` | Tokens, regex/division disambiguation, template literals, numeric separators |
| Parser | `parser.rs` | ES2023 scripts to a syntax tree; records which names nested functions capture |
| Compiler | `compiler.rs` | Static scope resolution, bytecode for a stack machine; `finally` and iterator cleanup inlined at every exit |
| VM | `vm.rs` | Frames without Rust recursion for JS-to-JS calls; generators and async functions suspend by moving their frame into the heap |
| Heap | `heap.rs` | Mark-and-sweep with weak maps as ephemerons; collects only at safe points |
| Realm | `realm.rs` | The specification's abstract operations: Get, Set, DefineOwnProperty, conversions, equality, iteration |
| Built-ins | `builtins/` | Object, Function, Array, String, Number, Boolean, Symbol, Math, JSON, Error, Map/Set/WeakMap/WeakSet, Promise, generators, async iteration, iterator helpers, ArrayBuffer, typed arrays, DataView, RegExp, Date, Reflect, console |
| RegExp | `regexp.rs` | Our own backtracking matcher: lookbehind, named groups, Unicode mode, sticky and indices flags |

A runaway script is stopped by an instruction budget (it can't catch the error), so a page can't freeze the browser.

## Status

**test262:** 35,192 tests pass: 94.5% of the 37,255 tests that run. [Results by area](pulsar-test262.md).

Regular expressions search with [regress](https://github.com/ridiculousfish/regress) (MIT/Apache-2.0, the engine Boa uses): full Unicode property escapes, case folding and the `v` flag. Pulsar's own matcher remains for the Annex B syntax regress refuses and for sticky (`y`) matches.

Our own regression suite (`libs/script/tests/js`) compares output byte for byte with Node, and runs every script a second time with the collector running every 64 allocations.

| Milestone | Scope | State |
|---|---|---|
| 1 | Language core: lexer, parser, compiler, VM, collector, core built-ins, `js` command, test262 runner | Done |
| 2 | Symbols, iterators, generators, Map/Set, Promise, async/await, async generators, RegExp, Date | Done |
| 3 | The DOM in Nebula: document and element bindings, events, timers, storage | Done; checked in WaveOS (QEMU) with the bundled demo, Wikipedia and Hacker News |
| 4 | `fetch`, layout geometry, cookies, performance | Done: see Performance below |
| 5 | `Proxy`, lazy compilation, property inline caches, a compiled-code cache, regress for RegExp | Done; `BigInt`, `Intl` and modules are next |

## Performance

Pulsar is an interpreter: no JIT, and so no machine code made at run time. `libs/script/bench` has small benchmarks. `cargo run --release -p nebula-script --example js -- --dump file.js` lists the bytecode a script compiles to.

Large compiler scopes use an indexed name lookup while retaining declaration order; small scopes keep a linear lookup. The tokenizer dispatches punctuation by its first character and reads ordinary ASCII identifiers without an intermediate string allocation. `cargo run --release -p nebula-script --example compile_bench -- FILE.js` measures parsing and compilation without executing the script. See [October 10 validation](VALIDATION-2026-10-10-NEBULA.md) for bundle measurements and their limits.

Large page scripts (64 KB and more) compile lazily: the parser skips the body of each ordinary function, recording every name it might use so the enclosing function knows which of its variables closures capture, and the function compiles on its first call against a snapshot of the scopes around it. A function wrapped in parentheses (`(function () { … })()`) is parsed at once, as it is almost always called straight away. YouTube's 10.6 MB application bundle parses and compiles in 343 ms instead of 562 ms on an M2; most of the difference is code the page never runs. Early errors inside a deferred body are reported when the function is first called.

Property reads, method lookups and writes on a name constant go through a per-instruction cache: the object that held the name last time and the entry index, or its prototype's. A hit is checked against the key at that entry, so a stale cache only falls back to a full lookup. Identifiers are interned while tokenizing, compiler scope indexes and name sets are hashed, and `RegExp` programs are compiled once per pattern and flags. Strings built by repeated `+=` are ropes, flattened when first read.

`Realm::code_cache` keeps compiled scripts between pages (Nebula shares one among its tabs), so reloading or returning to a site skips parsing and compiling its scripts; global-name caches are reset before a script runs in a new realm.

`realm.profile` samples where scripts spend their time (the `browse` example prints it with `PROFILE=1`).

Host code retaining JavaScript values across callbacks or microtasks must root them. `Realm::with_roots` keeps a batch alive until the callback returns, including error returns. Active promise jobs, thenable resolvers, rejection reports, dispatched events/listener snapshots, and animation-frame batches use this protection after leaving their traced queues.

| Benchmark | What it does | Before | Now |
|---|---|---:|---:|
| `loop.js` | 3 million iterations of arithmetic on a top-level `let` | 0.50 s | 0.18 s |
| `props.js` | 500,000 class instances, each calling a method | 0.41 s | 0.27 s |
| `arrays.js` | `map`, `filter`, `reduce` and `sort` over 200,000 numbers | 0.33 s | 0.14 s |
| `strings.js` | building, joining and splitting 100,000 strings | 0.10 s | 0.09 s |

Measured on an Apple silicon Mac, best of three runs. What changed:
- Top-level variables and global functions are found through a per-instruction cache instead of by name.
- Short strings in compiled code and built-in property names are interned, so most property lookups match by pointer.
- Ordinary objects skip the exotic-object checks when reading properties.
- Calls between JavaScript functions take their arguments where they already are on the stack.
- `sort` is a natural merge sort, so data that is already in order, or reversed, needs one comparison per element.
- `x % y` on integers no longer calls `fmod`.
- `i++` whose value is unused compiles like `++i`.

## In the browser

`libs/surf/src/script.rs` binds the DOM; `libs/surf/src/js/prelude.js` adds web APIs written in JavaScript (URL, URLSearchParams, the observers, fetch, XMLHttpRequest, AbortController, FormData, TextEncoder and TextDecoder); `crypto.getRandomValues` and `crypto.randomUUID` draw on the system's random source. Nebula fetches scripts, including ones that scripts insert, and network requests on worker threads, and runs everything on its UI thread.

Each page gets a fresh realm. Scripts see clicks, keys, typing and form submission before Nebula acts on them, and preventDefault() stops Nebula's default action. localStorage is kept per site in `/Settings/Nebula/Storage`. Cookies follow RFC 6265 (domains, paths, expiry, Secure, HttpOnly, the `__Secure-` and `__Host-` prefixes): pages, their requests and `document.cookie` share one jar (`libs/web/src/cookie.rs`), and cookies with an expiry date are kept in `/Settings/Nebula/Cookies`. The `nebula_javascript` preference (0 or 1) turns scripts off.

A test runs jQuery 3.7.1 against the DOM (`libs/surf/tests/script.rs`). `cargo run -p nebula-engine --example page -- page.html URL` runs a saved page's scripts on the host and reports what they did.

The DOM includes comment nodes, `NodeFilter`, `TreeWalker` and `NodeIterator`, `DOMException`, `Image` with `load`/`error` events, `naturalWidth`/`naturalHeight` and `complete`, `Text` and `Comment` inheriting from `CharacterData`, and legacy `CustomEvent.initCustomEvent`. `MutationObserver` reports `childList`, `attributes` (with `attributeFilter` and old values) and `characterData` changes on a node or its subtree, delivered in a microtask; the document records changes only while an observer is connected. `getComputedStyle` answers from the cascade (colours as `rgb()`, lengths in px, used sizes from layout, `::before`/`::after`, custom properties), computed on demand from the live document. `<style>` elements that scripts add, edit or remove take effect. Tabs on the same site share localStorage and get `storage` events.

The 2D canvas draws paths (lines, quadratic and cubic curves, arcs, ellipses, rounded rectangles) filled under the non-zero or even-odd rule and stroked with widths, caps, joins and dashes, with Lyra's exact-area anti-aliasing; transforms, `globalAlpha`, seven compositing operators, `clip()`, linear and radial gradients, `fillText`/`measureText` with the system fonts, `drawImage` from canvases and decoded images, and `ImageData`. Budgets: 1 million pixels per canvas, 2 million per page, 128 canvases. WebGL returns null. Open `file:///System/Library/Nebula/browser-apis.html` (eight checks) and `canvas-demo.html`.

## Not yet supported

Proxy, BigInt (and so BigInt64Array), resizable and shared ArrayBuffers, direct `eval` (eval always runs in the global scope), modules, Intl, Unicode normalisation (`normalize` returns its input), and locale-aware collation beyond an approximation.

In the browser: there's no CORS, so `fetch` and `XMLHttpRequest` send cookies only to the page's own origin (as `credentials: "same-origin"` does) and other sites' responses never carry the user's cookies; there's no SameSite enforcement, so cookies also go with requests for other sites' images and scripts; `IntersectionObserver` reports every element as visible once; `dataset` is read-only (it would need Proxy or exotic host objects, which Pulsar lacks); `<link>` stylesheets that scripts insert aren't fetched; MutationObserver has no transient observers for removed subtrees; NodeIterator doesn't adjust to removed reference nodes. Canvas patterns, shadows and filters aren't drawn, text ignores rotation and skew, and strokes under a skewed or unevenly scaled transform have an even width. The captured YouTube bootstrap replay (animations, web components, SPF) runs without uncaught errors; this is initialization only, not YouTube or video playback.

## Try it

```sh
cargo run -p nebula-script --example js               # a REPL on the host
cargo run -p nebula-script --example js -- file.js    # run a file
cargo test -p nebula-script -p nebula-engine          # unit, golden and DOM tests
cargo xtask test262                                   # the conformance suite (fetches it once)
```

In WaveOS, type `js` in Terminal for a prompt, `js file.js` to run a file, or `js -e "1 + 1"`.
