#!/usr/bin/env python3
"""Where the virtual processors spend their time: samples every vCPU's
instruction pointer through QEMU's monitor (`info registers -a`) while
WaveOS runs, and prints the share of samples in user code, the kernel's idle
loop and each kernel function (named from the kernel's symbols).

    python3 tools/sample_vcpus.py [--session DIR] [--kernel ELF] [--seconds N]
                                  [--interval MS] [--wait-for FILE TEXT]

DIR is the run's session folder (AURORA_SESSION_DIR, default target/), which
holds `qemu-monitor.sock`. With --wait-for, sampling starts once FILE (the
run's log) contains TEXT, e.g. "a program took the display". Under TCG a
sample costs the guest a little time, so keep the interval at 20 ms or more.
"""

import argparse
import bisect
import collections
import os
import re
import shutil
import socket
import subprocess
import sys
import time


def symbols(elf):
    """Sorted (address, name) of the kernel's functions."""
    nm = shutil.which("llvm-nm")
    if not nm:
        rustup = os.path.expanduser("~/.rustup/toolchains")
        for root, _, files in os.walk(rustup):
            if "llvm-nm" in files:
                nm = os.path.join(root, "llvm-nm")
                break
    if not nm:
        sys.exit("needs llvm-nm (rustup component add llvm-tools)")
    out = subprocess.run([nm, "--demangle", "-n", elf], capture_output=True, text=True).stdout
    syms = []
    for line in out.splitlines():
        parts = line.split(" ", 2)
        if len(parts) == 3 and parts[1] in "tTwW":
            name = re.sub(r"::h[0-9a-f]{16}$", "", parts[2])
            syms.append((int(parts[0], 16), name))
    syms.sort()
    return syms


def monitor(path):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(path)
    s.settimeout(5)
    read_until(s, b"(qemu) ")
    return s


def read_until(s, end):
    data = b""
    while not data.endswith(end):
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
    return data


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--session", default=os.environ.get("AURORA_SESSION_DIR", "target"))
    ap.add_argument("--kernel", default="target/x86_64-unknown-none/release/aster")
    ap.add_argument("--seconds", type=float, default=10)
    ap.add_argument("--interval", type=float, default=25, help="milliseconds between samples")
    ap.add_argument("--wait-for", nargs=2, metavar=("FILE", "TEXT"))
    ap.add_argument("--top", type=int, default=25)
    ap.add_argument("--raw", action="store_true", help="also list the commonest kernel addresses")
    args = ap.parse_args()

    if args.wait_for:
        path, text = args.wait_for
        while not (os.path.exists(path) and text in open(path, errors="replace").read()):
            time.sleep(0.2)
    # (After the wait: `cargo xtask test` builds the kernel it boots.)
    syms = symbols(args.kernel)
    addrs = [a for a, _ in syms]
    s = monitor(os.path.join(args.session, "qemu-monitor.sock"))
    counts = collections.Counter()
    raw = collections.Counter()
    total = 0
    end = time.time() + args.seconds
    while time.time() < end:
        s.sendall(b"info registers -a\n")
        text = read_until(s, b"(qemu) ").decode(errors="replace")
        for line in text.splitlines():
            m = re.search(r"RIP=([0-9a-f]{16}) .*CPL=(\d).*HLT=(\d)", line)
            if not m:
                continue
            rip, cpl, halted = m.groups()
            total += 1
            if halted == "1":
                counts["(halted: idle)"] += 1
                continue
            if cpl == "3":
                counts["(user)"] += 1
                continue
            a = int(rip, 16)
            raw[a] += 1
            i = bisect.bisect_right(addrs, a) - 1
            counts[syms[i][1] if i >= 0 else hex(a)] += 1
        time.sleep(args.interval / 1000)
    if not total:
        sys.exit("no samples")
    print(f"{total} samples")
    for name, n in counts.most_common(args.top):
        print(f"{100 * n / total:6.1f}%  {name}")
    if args.raw:
        for a, n in raw.most_common(args.top):
            print(f"{100 * n / total:6.1f}%  {a:#x}")


if __name__ == "__main__":
    main()
