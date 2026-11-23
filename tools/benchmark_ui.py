#!/usr/bin/env python3
"""Compare real UI workloads on an isolated 1280×800 QEMU guest.
Start the guest first, then pass its serial log and an output JSON path.
Uses Super-drag rather than depending on a particular title-bar layout.
Never run against a disk containing personal work: typing creates test content.
"""
import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FRAME = re.compile(r'frame: ([\w-]+), (\d+), (\d+), (\d+)x(\d+)')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('log', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--repetitions', type=int, default=3)
    args = parser.parse_args()
    def q(*steps):
        subprocess.run([sys.executable, str(ROOT/'tools/qmp.py'), *steps], check=True, stdout=subprocess.DEVNULL)
    deadline = time.monotonic() + 90
    while not args.log.exists() or 'Aurora desktop ready' not in args.log.read_text(errors='replace'):
        if time.monotonic() >= deadline:
            raise SystemExit('Guest did not reach desktop')
        time.sleep(0.5)
    # Same appearance, same apps, same input paths on baseline and candidate.
    q('key:ctrl+spc','type:Terminal','key:ret','sleep:1.5','type:theme dark','key:ret','sleep:0.5','type:exit','key:ret','sleep:0.7')
    q('key:ctrl+spc','type:Notes','key:ret','sleep:1.5','key:ctrl+spc','type:Files','key:ret','sleep:1.5','key:alt+tab','sleep:0.7')
    workloads = [
        ('pointer', ['glide:200,50,1000,50','glide:1000,50,200,50','sleep:0.3']),
        ('drag', ['keydown:meta_l','drag:600,350,680,390','drag:680,390,600,350','keyup:meta_l','sleep:0.4']),
        ('typing', ['click:600,350','key:ctrl+a','type:Quiet design. Clear feedback.','sleep:0.5']),
        ('switch', ['key:alt+tab','sleep:0.4','key:alt+tab','sleep:0.4']),
    ]
    records=[]
    for rep in range(args.repetitions + 1):
        for name, steps in workloads:
            start = args.log.stat().st_size
            t = time.monotonic()
            q(*steps)
            raw = args.log.read_bytes()[start:].decode(errors='replace')
            frames=[dict(kind=m[0],pixels=int(m[1]),ms=int(m[2])) for m in FRAME.findall(raw)]
            if rep:
                records.append(dict(repetition=rep,workload=name,wall_seconds=round(time.monotonic()-t,3),frames=frames))
        q('shot:'+str(args.output.with_name(args.output.stem+f'-{rep}.png').resolve()))
        print(f'Repetition {rep}: complete',flush=True)
    q('click:1260,500','sleep:3')
    start=args.log.stat().st_size
    q('sleep:10')
    idle=FRAME.findall(args.log.read_bytes()[start:].decode(errors='replace'))
    args.output.write_text(json.dumps(dict(viewport=[1280,800],warmup=1,measured=args.repetitions,records=records,idle_seconds=10,idle_frames=len(idle)),indent=2)+'\n')
    print(args.output)

if __name__=='__main__':
    main()
