# Architecture and application development
FirstLight loads the Aster kernel and system archive through UEFI. Aster manages physical/virtual memory, scheduling, drivers, filesystems, networking, system calls, and the Lumen window server. Lumen remains in the kernel; application code runs in ring 3.

## Understand the layers
1. Read libs/abi for system-call numbers, events, and validated shared structures.
2. Use CoreKit for files, process/thread operations, preferences, networking, and IPC.
3. Implement the AuroraKit App trait for native windows. The runtime draws to shared surfaces and forwards input events.
4. Use Lumen’s drawing primitives and declarative controls. Stable control IDs preserve focus.

AuroraKit supports labels, headings, buttons, toggles, sliders, rows, columns, grids, scrolling, cards, and glass panels. Existing text editors remain separate controls. App materials are opt-in; existing opaque apps continue to work.

## Efficient wakeups
Apps can implement `next_wakeup(&Env) -> Option<u64>` to return an absolute uptime deadline. AuroraKit recalculates it after input, drawing, and ticks so newly started work runs promptly. Return `None` to retain `tick_interval` behavior. Keep deadlines stable across unrelated pointer events; return true from `tick` only when the view changed. Files and Settings use slower idle deadlines, with transfer polling limited to ten updates per second.

For disposable VM measurements, create `/Settings/runtime-metrics` before launching an app. AuroraKit then logs wakeups, events, guest CPU/memory counters, and process CPU/memory/thread counts every ten seconds. Remove the marker to disable collection on subsequent launches. `tools/benchmark_everyday.py` and `tools/summarize_everyday.py` collect and compare repeated idle/window-switching trials; composition timings are not end-to-end input latency.

## Develop an app
Create a host-buildable app in the userland workspace, implement title/size/draw and required input handlers, then build with xtask. Test resizing, keyboard focus, theme changes, unsaved-work handling, and process isolation. See [Packaging](developer-packages.md) for distribution.

## Limits and debugging
Use `cargo xtask run --gdb` and the host debugger for kernel investigation. The native Studio source-debugger workflow is unavailable. The SDK bundle does not yet contain a complete offline Rust sysroot or native host toolchain.
