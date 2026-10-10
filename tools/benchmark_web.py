#!/usr/bin/env python3
"""Page-load timings in Nebula, from the guest's own measurements.

Start a VM first (`cargo xtask run --headless`, serial output to
target/serial.log), then: tools/benchmark_web.py OUT.json [--res W,H] [URL...]

Opens Nebula, loads each URL twice (cold, then a warm revisit after visiting
the others), and reads the `[nebula] timing` and `[nebula] images` lines
Nebula logs. Pages that don't finish within the timeout are recorded as such.
"""
import json, os, re, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LOG = os.path.join(ROOT, "target/serial.log")
SITES = [
    "https://example.com/",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://news.ycombinator.com/",
    "https://text.npr.org/",
    "https://lite.cnn.com/",
    "https://www.rust-lang.org/",
    "https://www.gnu.org/",
    "https://html.duckduckgo.com/html/?q=rust",
    "https://developer.mozilla.org/en-US/",
    "https://github.com/Sw3bbl3/waveos-aurora-rs",
    "https://old.reddit.com/",
    "https://www.bbc.com/news",
]
TIMEOUT = 180

args = sys.argv[1:]
res = "1920,1200"
if "--res" in args:
    i = args.index("--res")
    res = args[i + 1]
    del args[i:i + 2]
out = args[0]
urls = args[1:] or SITES


def qmp(*steps):
    subprocess.run([sys.executable, os.path.join(ROOT, "tools/qmp.py"), f"res:{res}", *steps], check=True)


def log_lines():
    with open(LOG, "rb") as f:
        return f.read().decode("utf-8", "replace").splitlines()


def parse(line):
    fields = dict(re.findall(r"(\w+)=(\S+)", line))
    return {k: (int(re.match(r"\d+", v).group()) if re.match(r"\d+", v) else v) for k, v in fields.items()}


def host(u):
    h = re.sub(r"^https?://", "", u).split("/")[0].lower()
    return h[4:] if h.startswith("www.") else h


def load(url):
    before = len(log_lines())
    qmp("key:ctrl+l", "type:" + url + "\n")
    start = time.time()
    timing = images = None
    while time.time() - start < TIMEOUT:
        new = log_lines()[before:]
        for line in new:
            if "[nebula] timing " in line and timing is None and \
                    host(line.split("[nebula] timing ", 1)[1].split()[0]) == host(url):
                timing = parse(line.split("[nebula] timing ", 1)[1])
                timing["final_url"] = line.split("[nebula] timing ", 1)[1].split()[0]
            if "[nebula] images " in line and timing is not None and images is None and \
                    host(line.split("[nebula] images ", 1)[1].split()[0]) == host(url):
                images = parse(line.split("[nebula] images ", 1)[1])
        if timing and (images or time.time() - start > 30):
            break
        time.sleep(0.5)
    return {"url": url, "timing": timing, "images": images}


qmp("key:ctrl+spc", "sleep:1", "type:Nebula", "sleep:1", "type:\n", "sleep:6")
results = {"cold": [], "warm": []}
for phase in ("cold", "warm"):
    for url in urls:
        r = load(url)
        results[phase].append(r)
        t = r["timing"] or {}
        print(f"{phase:4} {url:60} total={t.get('total', 'timeout')} images={(r['images'] or {}).get('total', '-')}", flush=True)
qmp("key:ctrl+w")
with open(out, "w") as f:
    json.dump(results, f, indent=1)
