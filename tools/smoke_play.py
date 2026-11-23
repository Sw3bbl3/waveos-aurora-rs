#!/usr/bin/env python3
"""Drive Wave Play in a running guest and capture it mid-motion.

Start the guest first (a disposable disk; it opens windows and launches apps):

  AURORA_SESSION_DIR=target/ps cargo xtask run --no-build --headless --no-open \\
      --no-sound --no-keyboard-capture --no-linux-root --disk-image DISK > LOG

then: AURORA_SESSION_DIR=target/ps tools/smoke_play.py LOG OUTDIR [--res WxH] [--only STEP,...]

There is no virtual USB controller on hosts whose QEMU lacks usb-redir, so
this uses the keyboard controller (Super+G connects it; arrows are the
d-pad, Enter A, Escape B, Home the home button, End Menu, Insert View),
which goes through the same gamepad layer as a USB one. Screenshots land in
OUTDIR; the transitions' frame timings (the `play:` lines the compositor
logs) are printed at the end and written to OUTDIR/timings.txt.
"""
import argparse
import json
import os
import re
import socket
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class Guest:
    def __init__(self, log, out, w, h):
        self.log, self.out, self.w, self.h = log, out, w, h
        self.slow = 1
        path = os.path.join(os.environ.get("AURORA_SESSION_DIR", os.path.join(ROOT, "target")), "qemu-qmp.sock")
        for _ in range(120):  # QEMU may still be starting
            try:
                sock = socket.socket(socket.AF_UNIX)
                sock.connect(path)
                break
            except OSError:
                time.sleep(0.5)
        else:
            sys.exit(f"no QMP socket at {path}")
        self.f = sock.makefile("rw")
        json.loads(self.f.readline())
        self.cmd("qmp_capabilities")

    def cmd(self, execute, **args):
        self.f.write(json.dumps({"execute": execute, "arguments": args} if args else {"execute": execute}) + "\n")
        self.f.flush()
        while True:
            m = json.loads(self.f.readline())
            if "return" in m or "error" in m:
                if "error" in m:
                    print("QMP error:", m["error"].get("desc"), file=sys.stderr)
                return m

    def keys(self, combo, hold=0.05):
        codes = combo.split("+")
        ev = lambda down: [{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": c}}} for c in codes]
        self.cmd("input-send-event", events=ev(True))
        time.sleep(hold)
        self.cmd("input-send-event", events=list(reversed(ev(False))))
        time.sleep(0.03)

    def type(self, text):
        for ch in text:
            if ch == "\n":
                self.keys("ret")
            elif ch == " ":
                self.keys("spc")
            elif ch.isupper():
                self.keys("shift+" + ch.lower())
            else:
                self.keys(ch)

    def shot(self, name):
        path = os.path.join(self.out, name + ".png")
        self.cmd("screendump", filename=os.path.abspath(path), format="png")
        return path

    def burst(self, prefix, seconds, every=0.07):
        """Screenshots as fast as QEMU gives them, for `seconds` (longer in slow motion)."""
        seconds, every = seconds * self.slow, every * (4 if self.slow > 1 else 1)
        t0, k = time.monotonic(), 0
        while time.monotonic() - t0 < seconds:
            self.shot(f"{prefix}-{k:02d}")
            k += 1
            time.sleep(every)
        return k

    def log_size(self):
        return os.path.getsize(self.log)

    def wait_log(self, text, start=0, timeout=30):
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout:
            with open(self.log, "rb") as f:
                f.seek(start)
                if text in f.read().decode(errors="replace"):
                    return True
            time.sleep(0.1)
        return False


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log")
    ap.add_argument("out")
    ap.add_argument("--res", default="1280x800")
    ap.add_argument("--only", default="")
    ap.add_argument("--slow", action="store_true", help="slow motion (x8, Super+Shift+G) for mid-animation shots")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    w, h = map(int, a.res.split("x"))
    g = Guest(a.log, a.out, w, h)
    if not g.wait_log("Aurora desktop ready", timeout=120):
        sys.exit("the guest did not reach the desktop")
    only = set(filter(None, a.only.split(",")))
    want = lambda s: not only or s in only
    start = g.log_size()
    if a.slow:
        g.keys("meta_l+shift+g")
        g.slow = 8

    def press(combo, wait=None, timeout=40, hold=0.05):
        """Presses keys; with `wait`, until the compositor logs that text."""
        mark = g.log_size()
        g.keys(combo, hold=hold)
        if wait and not g.wait_log(wait, mark, timeout=timeout * g.slow):
            print(f"note: no '{wait}' after {combo}", file=sys.stderr)

    if want("desktop"):
        # Two windows to transform away and bring back.
        g.keys("ctrl+spc"); time.sleep(0.6); g.type("Notes\n"); time.sleep(3)
        g.keys("ctrl+spc"); time.sleep(0.6); g.type("Files\n"); time.sleep(3)
        g.shot("01-desktop")

    if want("enter"):
        # Connect the keyboard controller: the notice, then the morph.
        mark = g.log_size()
        g.keys("meta_l+g")
        time.sleep(0.35)
        g.shot("02-toast")
        g.wait_log("entering Wave Play", mark, timeout=30 * g.slow)
        g.burst("03-morph-in", 1.4, 0.03)
        g.wait_log("Wave Play settled", mark, timeout=30 * g.slow)
        time.sleep(0.8)
        g.shot("04-home")

    if want("navigate"):
        for k in range(3):
            # Doom is alone in Games: these bump against the end of the row.
            press("right"); time.sleep(0.8)
            g.shot(f"05-bump-{k}")
        press("down", "focus move")
        g.shot("06-row-down")
        press("right", "focus move"); press("right", "focus move")
        g.shot("07-right")
        g.keys("right", hold=1.6); time.sleep(0.6); g.shot("07-held-right")
        press("up", "focus move")
        g.shot("08-back-up")
        press("down", "focus move")

    if want("search"):
        press("tab"); time.sleep(0.8); g.shot("09-search")
        for _ in range(3):
            g.keys("right")
        press("ret"); time.sleep(0.5)  # D
        for _ in range(11):
            g.keys("right")
        press("ret"); time.sleep(0.8)  # O
        g.shot("09-search-do")
        press("down"); time.sleep(0.8); g.shot("09-search-result")
        press("esc"); time.sleep(0.3); press("esc"); time.sleep(1.0)
        g.shot("09-search-closed")

    if want("overlay"):
        mark = g.log_size()
        g.keys("home")
        g.burst("10-overlay", 0.6, 0.03)
        g.wait_log("quick overlay opening", mark, timeout=20 * g.slow)
        time.sleep(0.3)
        g.shot("10-overlay")
        g.keys("down"); time.sleep(0.2); g.keys("down"); time.sleep(0.6); g.shot("11-overlay-volume")
        g.keys("right"); time.sleep(0.8); g.shot("12-overlay-louder")
        g.keys("esc"); time.sleep(1.2 * g.slow); g.shot("13-overlay-closed")

    if want("launch"):
        # The first app in the Apps row.
        mark = g.log_size()
        g.keys("ret")
        g.burst("14-launch", 1.2, 0.03)
        g.wait_log("launch zoom", mark, timeout=30 * g.slow)
        time.sleep(1.5)
        g.shot("15-app-full-screen")
        mark = g.log_size()
        g.keys("home")
        g.wait_log("quick overlay opening", mark, timeout=20 * g.slow)
        time.sleep(0.4)
        g.shot("16-overlay-over-app")
        mark = g.log_size()
        g.keys("down"); time.sleep(0.3); g.keys("ret")  # Wave Play Home
        g.burst("17-shrink", 1.0, 0.03)
        g.wait_log("back home", mark, timeout=20 * g.slow)
        time.sleep(1.0)
        g.shot("18-home-with-running")

    if want("exit"):
        mark = g.log_size()
        g.keys("insert")
        g.burst("19-morph-out", 1.3, 0.03)
        g.wait_log("desktop settled", mark, timeout=30 * g.slow)
        time.sleep(0.8)
        g.shot("20-desktop-again")

    if want("interrupt"):
        # Into Wave Play, back out, and change our mind half way.
        mark = g.log_size()
        g.keys("ctrl+spc"); time.sleep(0.6); g.type("Wave Play"); time.sleep(0.8); g.keys("ret")
        g.wait_log("Wave Play settled", mark, timeout=40 * g.slow)
        time.sleep(0.5)
        mark = g.log_size()
        g.keys("insert")
        g.wait_log("back to the desktop", mark, timeout=20)
        time.sleep(0.25 * g.slow)
        g.shot("21-reverse-a")
        g.keys("insert")
        g.burst("22-reversed", 0.8, 0.03)
        g.wait_log("Wave Play settled", mark, timeout=30 * g.slow)
        time.sleep(0.5)
        g.shot("23-back-in")
        mark = g.log_size()
        g.keys("insert")
        g.wait_log("desktop settled", mark, timeout=30 * g.slow)

    if want("disconnect"):
        mark = g.log_size()
        g.keys("ctrl+spc"); time.sleep(0.6); g.type("Wave Play"); time.sleep(0.8); g.keys("ret")
        g.wait_log("Wave Play settled", mark, timeout=40 * g.slow)
        mark = g.log_size()
        g.keys("meta_l+g")  # unplug: back to the desktop
        time.sleep(0.25)
        g.shot("24-unplugged")
        g.wait_log("desktop settled", mark, timeout=30 * g.slow)
        time.sleep(0.5)
        g.shot("25-after-unplug")

    with open(a.log, "rb") as f:
        f.seek(start)
        text = f.read().decode(errors="replace")
    lines = [l for l in text.splitlines() if re.search(r"\b(play|lumen):", l)]
    with open(os.path.join(a.out, "timings.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
