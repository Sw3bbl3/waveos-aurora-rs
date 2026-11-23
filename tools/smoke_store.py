#!/usr/bin/env python3
"""Drive the Store in a running guest and take screenshots at each step.

Serve the demo repository and start a guest on a disposable disk:

  cargo xtask serve-repo --port 8123 &
  AURORA_SESSION_DIR=target/ss cargo xtask run --no-build --headless --no-open \\
      --no-sound --no-keyboard-capture --no-linux-root --fresh-disk \\
      --disk-image target/store-smoke.img > LOG

then: AURORA_SESSION_DIR=target/ss tools/smoke_store.py LOG OUTDIR [--port 8123]

Steps: open the Store (empty: no repository), add the demo repository from
its empty state, Discover, search for Doom, its page, Get (download and
install), Open it, Library, Remove, Updates, and Settings → Software Update.
The screenshots are for a person to look at; the script only checks that
the guest logged what each step should cause.
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
    time.sleep(4)
    failures = []

    def expect(text, start, what, timeout=60):
        if not g.wait_log(text, start, timeout=timeout):
            failures.append(what)
            print(f"FAILED: {what} (no '{text}' in the log)", file=sys.stderr)

    def seek(name):
        g.keys("ctrl+spc")
        time.sleep(0.8)
        g.type(name)
        time.sleep(0.6)
        g.keys("ret")

    # 1. The Store with no repository: its empty state.
    mark = g.log_size()
    seek("Store")
    expect("Store.elf", mark, "the Store starts")
    time.sleep(5)
    g.shot("01-empty")

    # 2. Add the demo repository from the empty state (needs the
    #    administrator: the first account is one, so no prompt).
    g.keys("tab")
    time.sleep(0.3)
    url = f"http://10.0.2.2:{a.port}/"
    for _ in range(3):
        g.keys("tab")
        time.sleep(0.2)
    g.type(url)
    time.sleep(0.3)
    g.keys("ret")
    time.sleep(8)
    g.shot("02-discover")

    # 3. Search.
    g.keys("ctrl+f")
    time.sleep(0.3)
    g.type("doom")
    time.sleep(1.5)
    g.shot("03-search")

    # 4. The app page (arrow to the first card, Enter).
    g.keys("down")
    time.sleep(0.4)
    g.keys("ret")
    time.sleep(2.5)
    g.shot("04-app-page")

    # 5. Get: download and install for this account.
    mark = g.log_size()
    g.keys("right")
    time.sleep(0.2)
    g.keys("ret")
    time.sleep(1.5)
    g.shot("05-downloading")
    time.sleep(10)
    g.shot("06-installed")

    # 6. Open it.
    mark = g.log_size()
    g.keys("ret")
    expect("org.waveos.doom", mark, "the installed app starts", timeout=30)
    time.sleep(4)
    g.shot("07-launched")
    g.keys("ctrl+w")
    time.sleep(2)

    # 7. Library, Updates, Software Update.
    for i, (keys, name) in enumerate([("esc", "08-back"), ("ctrl+f", None)]):
        g.keys(keys)
        time.sleep(1)
        if name:
            g.shot(name)
    g.keys("esc")
    time.sleep(1)
    seek("Software Update")
    time.sleep(4)
    g.shot("09-software-update")

    print(f"screenshots in {a.out}")
    if failures:
        sys.exit("failed: " + "; ".join(failures))


if __name__ == "__main__":
    main()
