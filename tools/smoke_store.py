#!/usr/bin/env python3
"""Drive the Store in a running guest and take screenshots at each step.

Serve the demo repository (slowly, so downloads can be seen) and start a
guest on a disposable disk:

  cargo xtask serve-repo --port 8123 --slow &
  AURORA_SESSION_DIR=target/ss cargo xtask run --no-build --headless --no-open \\
      --no-sound --no-keyboard-capture --no-linux-root --fresh-disk \\
      --disk-image target/store-smoke.img > LOG

then: AURORA_SESSION_DIR=target/ss tools/smoke_store.py LOG OUTDIR [--port 8123]

Steps: first-start setup, the Store with no repository (its empty state),
adding the demo repository there, Discover, search, an app's page, Get
(downloading, then installed), Open, the Library, Remove, Updates (with the
demo WaveOS update), and Settings → Software Update. Everything after the
setup is done with the keyboard. The screenshots are for a person to look
at; the script checks the log for what each step should cause.
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from smoke_play import Guest  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log")
    ap.add_argument("out")
    ap.add_argument("--port", default="8123")
    ap.add_argument("--res", default="1280x800")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    w, h = map(int, a.res.split("x"))
    g = Guest(a.log, a.out, w, h)
    if not g.wait_log("Aurora desktop ready", timeout=180):
        sys.exit("the guest did not reach the desktop")
    time.sleep(3)
    failures = []

    def expect(text, start, what, timeout=60):
        if not g.wait_log(text, start, timeout=timeout):
            failures.append(what)
            print(f"FAILED: {what} (no '{text}' in the log)", file=sys.stderr)

    def click(x, y):
        g.cmd("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / (w - 1))}},
            {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / (h - 1))}}])
        time.sleep(0.1)
        for down in (True, False):
            g.cmd("input-send-event", events=[{"type": "btn", "data": {"down": down, "button": "left"}}])
            time.sleep(0.05)

    special = {" ": "spc", "\n": "ret", ":": "shift+semicolon", "/": "slash", ".": "dot", "-": "minus"}

    def type_text(text):
        for ch in text:
            if ch in special:
                g.keys(special[ch])
            elif ch.isupper():
                g.keys("shift+" + ch.lower())
            else:
                g.keys(ch)

    def seek(name):
        g.keys("ctrl+spc")
        time.sleep(0.8)
        g.type(name)
        time.sleep(1)
        g.keys("ret")

    # First-start setup (a fresh disk): a name, then Start.
    g.type("Test User")
    click(803, 664)
    time.sleep(2)
    click(803, 652)
    time.sleep(5)
    g.keys("esc")
    time.sleep(1)

    # 1. The Store with no repository: its empty state, the address field focused.
    mark = g.log_size()
    seek("Store")
    expect("Store.elf", mark, "the Store starts")
    time.sleep(6)
    g.shot("01-empty")

    # 2. Add the demo repository; Discover fills in.
    mark = g.log_size()
    type_text(f"http://10.0.2.2:{a.port}/\n")
    time.sleep(8)
    g.shot("02-discover")

    # 3. Search (Ctrl+F), then the first result's page.
    g.keys("ctrl+f")
    g.type("doom")
    time.sleep(1.5)
    g.shot("03-search")
    g.keys("down")
    time.sleep(0.4)
    g.keys("ret")
    time.sleep(3)
    g.shot("04-app-page")

    # 4. Get (the page's first target), downloading slowly, then installed.
    g.keys("down")
    time.sleep(0.3)
    g.keys("ret")
    time.sleep(2.5)
    g.shot("05-downloading")
    time.sleep(25)
    g.shot("06-installed")

    # 5. Open it (the same button now says Open).
    mark = g.log_size()
    g.keys("ret")
    expect("started pid", mark, "the installed app starts", timeout=30)
    time.sleep(5)
    g.shot("07-launched")
    g.keys("ctrl+w")
    time.sleep(2)

    # 6. The Library, then Remove (the first row's trash button).
    g.keys("pgup")
    time.sleep(2)
    g.shot("08-library")
    click(1109, 253)
    time.sleep(1)
    g.shot("09-remove-confirm")
    click(1087, 253)
    time.sleep(3)
    g.shot("09-removed")

    # 7. Updates (the demo WaveOS update shows), then Software Update.
    g.keys("pgup")
    time.sleep(2)
    g.shot("10-updates")
    seek("Software Update")
    time.sleep(10)
    g.shot("11-software-update")
    g.keys("ctrl+w")
    time.sleep(2)

    print(f"screenshots in {a.out}")
    if failures:
        sys.exit("failed: " + "; ".join(failures))


if __name__ == "__main__":
    main()
