#!/usr/bin/env python3
"""Use the desktop the way a person does, over two boots, and check each step.

  AURORA_SESSION_DIR=target/sd tools/smoke_desktop.py OUTDIR [--arch ARCH] [--gpu GPU] [--smp N] [--pace F] [--keep-disk]

Without --arch and --gpu it boots what `cargo xtask run` boots on this
computer (on an Apple-silicon Mac: aarch64 on virtio-gpu); `--gpu std` is
the plain framebuffer (ramfb on aarch64).

Builds nothing: run `cargo xtask build [--arch ...]` first. It starts the
guest itself (`cargo xtask run --no-build --headless` on a fresh disk,
OUTDIR/disk.img), twice:

  boot 1  first-run setup typed in (a name and a password), Welcome closed,
          Terminal running a command, Files, Settings, Notes with a note
          typed in, the Notes window dragged by its title bar, then Shut Down
          from the top bar's menu (QEMU must exit by itself);
  boot 2  the login screen, the password typed, the desktop again; the
          Terminal checks that the note and the folder made in boot 1 are on
          disk, then Shut Down from the menu again.

Input is QMP's (the USB tablet and keyboard), so it goes through the same
drivers and focus rules as a person's. The checks are the serial log's
lines (programs started, their exit codes, the session) and, for the drag,
the screen itself. Screenshots of every step land in OUTDIR. Exit status 0
when every check passed.
"""
import argparse
import os
import re
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from smoke_play import ROOT, Guest  # noqa: E402

PASSWORD = "tidepool7"
NOTE_TITLE = "SmokeNote"
NOTE_BODY = "typed on the desktop"
FOLDER = "/Documents/SmokeDir"

# Characters `Guest.type` doesn't map itself.
SPECIAL = {"/": "slash", ".": "dot", "-": "minus", ":": "shift+semicolon", ">": "shift+dot", "_": "shift+minus"}


class Smoke:
    def __init__(self, a):
        self.a = a
        self.failures = []
        self.proc = None
        self.g = None
        self.boot = 0
        self.w, self.h = 0, 0

    # --- the guest -----------------------------------------------------

    def start(self, fresh):
        self.boot += 1
        self.log = os.path.join(self.a.out, f"boot{self.boot}.log")
        cmd = ["cargo", "xtask", "run", "--no-build", "--headless", "--no-open",
               "--no-sound", "--no-keyboard-capture", "--no-linux-root", "--smp", str(self.a.smp),
               "--disk-image", self.disk]
        if self.a.arch:
            cmd[3:3] = ["--arch", self.a.arch]
        if self.a.gpu:
            cmd += ["--gpu", self.a.gpu]
        if fresh:
            cmd.append("--fresh-disk")
        print(f"boot {self.boot}: {' '.join(cmd)}")
        out = open(self.log, "wb")
        # Its own process group: stopping it stops QEMU too, not only cargo.
        self.proc = subprocess.Popen(cmd, cwd=ROOT, stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
        # The QMP socket of an earlier run may linger until QEMU replaces it.
        time.sleep(3)
        g = Guest(self.log, self.a.out, 0, 0)
        self.g = g
        if not g.wait_log("Aurora desktop ready", timeout=300):
            self.fail("the guest reaches its desktop")
            self.stop()
            sys.exit("\n".join(self.failures))
        m = re.search(rb"gui: desktop composed in \d+ ms \((\d+)x(\d+)\)", open(self.log, "rb").read())
        self.w, self.h = (int(m.group(1)), int(m.group(2))) if m else (1024, 768)
        g.w, g.h = self.w, self.h
        print(f"boot {self.boot}: desktop ready at {self.w}x{self.h}")
        self.nap(1.5)

    def stop(self):
        if self.proc and self.proc.poll() is None:
            for sig, wait in ((signal.SIGTERM, 20), (signal.SIGKILL, 10)):
                try:
                    os.killpg(self.proc.pid, sig)
                    self.proc.wait(wait)
                    break
                except (ProcessLookupError, subprocess.TimeoutExpired):
                    pass

    def fail(self, what):
        self.failures.append(f"boot {self.boot}: {what}")
        print(f"FAILED: {what}", file=sys.stderr)

    def expect(self, pattern, mark, what, timeout=30):
        """Waits for a log line matching `pattern` (a regex) written after `mark`."""
        t0 = time.monotonic()
        rx = re.compile(pattern)
        while time.monotonic() - t0 < timeout:
            with open(self.log, "rb") as f:
                f.seek(mark)
                m = rx.search(f.read().decode(errors="replace"))
            if m:
                print(f"  ok: {what}")
                return m
            time.sleep(0.1)
        self.fail(f"{what} (no /{pattern}/ in the log)")
        return None

    def mark(self):
        return self.g.log_size()

    def shot(self, name):
        self.g.shot(f"{self.boot}-{name}")

    # --- input ----------------------------------------------------------

    def point(self, x, y):
        self.g.cmd("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / (self.w - 1))}},
            {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / (self.h - 1))}}])
        self.nap(0.05)

    def button(self, down):
        self.g.cmd("input-send-event", events=[{"type": "btn", "data": {"down": down, "button": "left"}}])
        self.nap(0.05)

    def click(self, x, y):
        self.point(x, y)
        self.nap(0.1)
        self.button(True)
        self.button(False)

    def drag(self, x1, y1, x2, y2, steps=12):
        self.point(x1, y1)
        self.nap(0.1)
        self.button(True)
        for i in range(1, steps + 1):
            self.point(x1 + (x2 - x1) * i // steps, y1 + (y2 - y1) * i // steps)
        self.nap(0.1)
        self.button(False)

    def nap(self, seconds):
        time.sleep(seconds * self.a.pace)

    def key(self, combo):
        codes = combo.split("+")
        ev = lambda down: [{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": c}}} for c in codes]
        self.g.cmd("input-send-event", events=ev(True))
        self.nap(0.05)
        self.g.cmd("input-send-event", events=list(reversed(ev(False))))
        self.nap(0.03)

    def type(self, text):
        for ch in text:
            if ch in SPECIAL:
                self.key(SPECIAL[ch])
            elif ch == "\n":
                self.key("ret")
            elif ch == " ":
                self.key("spc")
            elif ch.isupper():
                self.key("shift+" + ch.lower())
            else:
                self.key(ch)

    def seek(self, name):
        """Opens an app the way the README says: Ctrl+Space, its name, Enter."""
        mark = self.mark()
        self.key("ctrl+spc")
        self.nap(0.8)
        self.type(name)
        self.nap(1.0)
        self.key("ret")
        return self.expect(rf"started pid (\d+) '/System/Apps/{name}\.elf'", mark, f"{name} starts")

    def close(self, name, m):
        """Ctrl+W; the app must exit."""
        if not m:
            return
        mark = self.mark()
        self.key("ctrl+w")
        self.expect(rf"pid {m.group(1)} '{name}' exited", mark, f"{name} closes on Ctrl+W", timeout=15)
        self.nap(0.5)

    def run_in_terminal(self, line, program, code, what):
        mark = self.mark()
        self.type(line + "\n")
        self.expect(rf"pid \d+ '{program}' exited with code {code}\b", mark, what, timeout=20)

    def screen(self):
        """The screen as rows of (r, g, b) bytes (QEMU's PPM screendump)."""
        path = os.path.join(self.a.out, "screen.ppm")
        self.g.cmd("screendump", filename=os.path.abspath(path))
        data = open(path, "rb").read()
        m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+(\d+)\s", data)
        w, h = int(m.group(1)), int(m.group(2))
        px = data[m.end():]
        return w, h, px

    def power_menu(self, choice):
        """The top bar's account menu, its power button, then Shut Down or Restart."""
        mark = self.mark()
        self.click(self.w - 33, 27)
        self.nap(1.0)
        self.shot(f"menu")
        self.click(self.w - 132, 367)
        self.nap(1.0)
        self.shot("power-dialog")
        # The dialog opens over the front window: find its red Shut Down button.
        w, h, px = self.screen()
        red = [(i % w, i // w) for i in range(w * h)
               if px[i * 3] > 220 and px[i * 3 + 1] < 120 and px[i * 3 + 2] < 120]
        if len(red) < 200:
            self.fail("the power dialog shows a Shut Down button")
            return
        self.click(sum(x for x, _ in red) // len(red), sum(y for _, y in red) // len(red))
        self.expect(r"power: shutting down", mark, f"{choice} from the menu", timeout=30)

    # --- the two boots --------------------------------------------------

    def first_boot(self):
        self.start(fresh=True)
        g, cx, cy = self.g, self.w // 2, self.h // 2
        self.shot("01-setup")
        # Step 1: the name (the account name follows it), the password twice.
        mark = self.mark()
        self.type("Test User")
        self.key("tab")
        self.key("tab")
        self.type(PASSWORD)
        self.key("tab")
        self.type(PASSWORD)
        self.nap(0.3)
        self.shot("02-account")
        self.click(cx + 163, cy + 264)  # Continue
        self.nap(1.5)
        self.shot("03-looks")
        # Step 2: dark mode is the default; Mist (a light wallpaper); Start.
        self.click(cx, cy + 6)
        self.nap(0.3)
        self.click(cx + 163, cy + 252)  # Start
        self.expect(r"gui: created the account test", mark, "first-run setup creates the account")
        m = self.expect(r"started pid (\d+) '/System/Apps/Welcome\.elf'", mark, "Welcome opens")
        self.nap(2)
        self.shot("04-welcome")
        self.close("Welcome", m)

        # Terminal: a command, and a folder boot 2 looks for.
        m = self.seek("Terminal")
        self.nap(1.5)
        self.run_in_terminal("echo hello", "echo", 0, "echo runs in Terminal")
        self.run_in_terminal(f"mkdir {FOLDER}", "mkdir", 0, "mkdir runs in Terminal")
        self.nap(0.5)
        self.shot("05-terminal")
        self.close("Terminal", m)

        for app in ("Files", "Settings"):
            m = self.seek(app)
            self.nap(2)
            self.shot(f"06-{app.lower()}")
            self.close(app, m)

        # Notes: a new note, typed; its title names the file.
        m = self.seek("Notes")
        self.nap(2)
        self.type(f"{NOTE_TITLE}\n{NOTE_BODY}")
        self.nap(1)
        self.shot("07-notes")
        self.drag_check()
        # Shut down with Notes open: it must save first.
        self.power_menu("Shut Down")
        self.wait_exit()

    def drag_check(self):
        """Drags the only window by its title bar, down and part way back up; the
        bar must follow the pointer. (The desktop keeps a window's edges on the
        screen, so the moves are small enough for a window as tall as Notes
        opens; and a drag that ends near the top of the screen moors the window
        to a layout instead, so the second one stops short of where it began.)"""
        w, h, a = self.screen()
        stride = w * 3
        x = w // 2

        def top_edge(px):
            """The window's top edge: the first dark row under the top bar, in the middle column."""
            return next((y for y in range(56, h // 2)
                         if max(px[y * stride + x * 3: y * stride + x * 3 + 3]) < 70), None)

        top = top_edge(a)
        if top is None:
            self.fail("no window found to drag")
            return
        # Where the top edge must be after each drag (by the bar's own
        # position, so the check means the same at any screen size).
        title_y, dy = top + 16, 36
        for k, (y0, y1) in enumerate(((title_y, title_y + dy), (title_y + dy, title_y + dy - 20))):
            self.drag(w // 3, y0, w // 3, y1)
            self.nap(0.8)
            self.shot(f"08-dragged-{k}")
            _, _, b = self.screen()
            now, want = top_edge(b), top + (y1 - title_y)
            print(f"  drag {k}: the window's top edge is at {now}, expected {want}")
            if now is None or abs(now - want) > 2:
                self.fail("the window follows a title-bar drag")
                return
        print("  ok: the window follows title-bar drags")

    def wait_exit(self):
        try:
            self.proc.wait(60)
            print("  ok: QEMU exits after the shutdown")
        except subprocess.TimeoutExpired:
            self.fail("QEMU exits after the shutdown")
            self.stop()

    def second_boot(self):
        self.start(fresh=False)
        self.shot("01-login")
        mark = self.mark()
        self.type(PASSWORD + "\n")
        self.expect(r"users: session: uid 1000", mark, "the saved account logs in")
        self.nap(2)
        self.shot("02-desktop")
        m = self.seek("Terminal")
        self.nap(1.5)
        self.run_in_terminal(f"grep typed /Documents/Notes/{NOTE_TITLE}.txt", "grep", 0, "the note was saved")
        self.run_in_terminal(f"ls {FOLDER}", "ls", 0, "the folder made in Terminal was kept")
        self.shot("03-terminal")
        self.close("Terminal", m)
        self.power_menu("Shut Down")
        self.wait_exit()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out")
    ap.add_argument("--arch", default=None, help="default: this computer's (what `cargo xtask run` picks)")
    ap.add_argument("--gpu", default=None, help="default: the architecture's (virtio on aarch64, std on x86_64)")
    ap.add_argument("--smp", default=4, type=int)
    ap.add_argument("--keep-disk", action="store_true")
    ap.add_argument("--pace", default=1.0, type=float,
                    help="scale every pause and key press (0.2: five times faster, to shake out races)")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    s = Smoke(a)
    s.disk = os.path.abspath(os.path.join(a.out, "disk.img"))
    t0 = time.monotonic()
    try:
        s.first_boot()
        s.second_boot()
    finally:
        s.stop()
        if not a.keep_disk and os.path.exists(s.disk):
            os.remove(s.disk)
    print(f"screenshots and logs in {a.out} ({time.monotonic() - t0:.0f} s)")
    if s.failures:
        sys.exit("smoke test FAILED:\n  " + "\n  ".join(s.failures))
    print("smoke test passed")


if __name__ == "__main__":
    main()
