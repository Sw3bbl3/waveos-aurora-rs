#!/usr/bin/env python3
"""Drive a running WaveOS Aurora VM (started with `cargo xtask run`) over QMP.

Usage: tools/qmp.py STEP [STEP...]
  move:X,Y  click:X,Y  dclick:X,Y  rclick:X,Y  drag:X1,Y1,X2,Y2
  type:TEXT (\\n = Enter)  key:QCODE[+QCODE]  wheel:N  sleep:SECONDS
  shot:PATH (PNG screenshot)  wait-log:TEXT (wait for a serial log line, from target/serial.log)
  quit
Coordinates are screen pixels (1280x800).
"""
import json, os, socket, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
W, H = 1280, 800
SPECIAL = {" ": "spc", "\n": "ret", "-": "minus", ".": "dot", "/": "slash", "=": "equal", ",": "comma",
           ";": "semicolon", "'": "apostrophe", "`": "grave_accent", "[": "bracket_left", "]": "bracket_right",
           "\\": "backslash", ">": "shift+dot", "<": "shift+comma", "*": "shift+8", "+": "shift+equal",
           "_": "shift+minus", ":": "shift+semicolon", "|": "shift+backslash", "!": "shift+1", "~": "shift+grave_accent",
           "&": "shift+7", '"': "shift+apostrophe", "?": "shift+slash", "(": "shift+9", ")": "shift+0"}

sock = socket.socket(socket.AF_UNIX)
sock.connect(os.path.join(ROOT, "target/qemu-qmp.sock"))
f = sock.makefile("rw")

def rd():
    while True:
        m = json.loads(f.readline())
        if "return" in m or "error" in m:
            if "error" in m:
                print("QMP error:", m["error"].get("desc"), file=sys.stderr)
            return m

def cmd(execute, **args):
    f.write(json.dumps({"execute": execute, "arguments": args} if args else {"execute": execute}) + "\n")
    f.flush()
    return rd()

json.loads(f.readline())  # greeting
cmd("qmp_capabilities")

def ev(events): cmd("input-send-event", events=events)
def pos(x, y):
    ev([{"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / (W - 1))}},
        {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / (H - 1))}}])
    time.sleep(0.05)
def btn(down, b="left"):
    ev([{"type": "btn", "data": {"down": down, "button": b}}]); time.sleep(0.05)
def key(codes):
    ev([{"type": "key", "data": {"down": True, "key": {"type": "qcode", "data": c}}} for c in codes]); time.sleep(0.03)
    ev([{"type": "key", "data": {"down": False, "key": {"type": "qcode", "data": c}}} for c in reversed(codes)]); time.sleep(0.03)

for step in sys.argv[1:]:
    k, _, v = step.partition(":")
    if k == "move":
        pos(*map(int, v.split(",")))
    elif k in ("click", "dclick", "rclick"):
        x, y = map(int, v.split(",")); pos(x, y)
        b = "right" if k == "rclick" else "left"
        for _ in range(2 if k == "dclick" else 1):
            btn(True, b); btn(False, b)
    elif k == "drag":
        x1, y1, x2, y2 = map(int, v.split(",")); pos(x1, y1); btn(True)
        for i in range(1, 9):
            pos(x1 + (x2 - x1) * i // 8, y1 + (y2 - y1) * i // 8)
        btn(False)
    elif k == "type":
        for ch in v.replace("\\n", "\n"):
            c = SPECIAL.get(ch, ch)
            if c.startswith("shift+"): key(["shift", c[6:]])
            elif ch.isupper(): key(["shift", ch.lower()])
            else: key([c])
    elif k == "key":
        key(v.split("+"))
    elif k == "wheel":
        n = int(v); b = "wheel-down" if n > 0 else "wheel-up"
        for _ in range(abs(n)): btn(True, b); btn(False, b)
    elif k == "sleep":
        time.sleep(float(v))
    elif k == "shot":
        time.sleep(0.4); cmd("screendump", filename=os.path.abspath(v), format="png")
    elif k == "wait-log":
        log = os.path.join(ROOT, "target/serial.log")
        for _ in range(600):
            if os.path.exists(log) and v in open(log, errors="replace").read(): break
            time.sleep(0.1)
        else:
            sys.exit(f"timed out waiting for log line: {v}")
    elif k == "quit":
        cmd("quit")
    else:
        sys.exit(f"unknown step: {step}")
