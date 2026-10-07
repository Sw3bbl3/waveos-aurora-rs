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
| Built-ins | `builtins/` | Object, Function, Array, String, Number, Boolean, Symbol, Math, JSON, Error, Map/Set/WeakMap/WeakSet, Promise, generators, async iteration, RegExp, Date, Reflect, console |
| RegExp | `regexp.rs` | Our own backtracking matcher: lookbehind, named groups, Unicode mode, sticky and indices flags |

A runaway script is stopped by an instruction budget (it can't catch the error), so a page can't freeze the browser.

## Status

**test262:** 31,017 tests pass: 88.1% of the 35,196 tests that run. [Results by area](pulsar-test262.md).

Our own regression suite (`libs/script/tests/js`) compares output byte for byte with Node, and runs every script a second time with the collector running every 64 allocations.

| Milestone | Scope | State |
|---|---|---|
| 1 | Language core: lexer, parser, compiler, VM, collector, core built-ins, `js` command, test262 runner | Done |
| 2 | Symbols, iterators, generators, Map/Set, Promise, async/await, async generators, RegExp, Date | Done |
| 3 | The DOM in Nebula: document and element bindings, events, timers, storage | Bindings done and tested on the host; browser integration in progress |
| 4 | `fetch`, layout geometry, cookies, performance | Planned |

## Not yet supported

Proxy, BigInt, typed arrays and ArrayBuffer, `with`, direct `eval` (eval always runs in the global scope), modules, Intl, Unicode normalisation (`normalize` returns its input), and locale-aware collation beyond an approximation.

## Try it

```sh
cargo run -p nebula-script --example js               # a REPL on the host
cargo run -p nebula-script --example js -- file.js    # run a file
cargo test -p nebula-script -p nebula-engine          # unit, golden and DOM tests
cargo xtask test262                                   # the conformance suite (fetches it once)
```

In WaveOS, type `js` in Terminal for a prompt, `js file.js` to run a file, or `js -e "1 + 1"`.
