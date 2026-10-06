# Pulsar, the JavaScript engine

Pulsar is the JavaScript engine for Nebula, WaveOS's web browser. Like the browser's HTML and CSS engine, it is a `no_std` Rust library (`libs/script`, package `nebula-script`), developed and tested on the host and linked into the browser and the `js` Terminal command.

## Status

In development. Nothing runs yet; this page is updated, with measured results, at every milestone.

| Milestone | Scope | State |
|---|---|---|
| 1 | Language core: lexer, parser, bytecode compiler, VM, garbage collector, core built-ins, `js` command, test262 runner | In progress |
| 2 | Symbols, iterators, generators, Map/Set, Promise, async/await, RegExp, Date | Planned |
| 3 | The DOM in Nebula: document and element bindings, events, timers | Planned |
| 4 | `fetch`, geometry, cookies, performance | Planned |
