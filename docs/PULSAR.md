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

**test262:** 34,415 tests pass: 94.3% of the 36,484 tests that run. [Results by area](pulsar-test262.md).

Our own regression suite (`libs/script/tests/js`) compares output byte for byte with Node, and runs every script a second time with the collector running every 64 allocations.

| Milestone | Scope | State |
|---|---|---|
| 1 | Language core: lexer, parser, compiler, VM, collector, core built-ins, `js` command, test262 runner | Done |
| 2 | Symbols, iterators, generators, Map/Set, Promise, async/await, async generators, RegExp, Date | Done |
| 3 | The DOM in Nebula: document and element bindings, events, timers, storage | Done; checked in WaveOS (QEMU) with the bundled demo, Wikipedia and Hacker News |
| 4 | `fetch`, layout geometry, cookies, performance | Done: see Performance below; inline caches for property access are next |

## Performance

Pulsar is an interpreter: no JIT, and so no machine code made at run time. `libs/script/bench` has small benchmarks. `cargo run --release -p nebula-script --example js -- --dump file.js` lists the bytecode a script compiles to.

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

## Not yet supported

Proxy, BigInt (and so BigInt64Array), resizable and shared ArrayBuffers, direct `eval` (eval always runs in the global scope), modules, Intl, Unicode normalisation (`normalize` returns its input), and locale-aware collation beyond an approximation.

In the browser: there's no CORS, so `fetch` and `XMLHttpRequest` send cookies only to the page's own origin (as `credentials: "same-origin"` does) and other sites' responses never carry the user's cookies; there's no SameSite enforcement, so cookies also go with requests for other sites' images and scripts; `MutationObserver` never reports; `IntersectionObserver` reports every element as visible once; `getComputedStyle` returns inline styles; dataset is read-only.

## Try it

```sh
cargo run -p nebula-script --example js               # a REPL on the host
cargo run -p nebula-script --example js -- file.js    # run a file
cargo test -p nebula-script -p nebula-engine          # unit, golden and DOM tests
cargo xtask test262                                   # the conformance suite (fetches it once)
```

In WaveOS, type `js` in Terminal for a prompt, `js file.js` to run a file, or `js -e "1 + 1"`.
