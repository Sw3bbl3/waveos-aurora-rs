#!/usr/bin/env python3
"""Initial Learn baseline: one warm-up and five repetitions, offline, English."""
import json, pathlib, re, subprocess, sys, time
root = pathlib.Path(__file__).resolve().parent.parent
log, out = map(pathlib.Path, sys.argv[1:3])
def q(*steps):
    subprocess.run([sys.executable, str(root/'tools/qmp.py'), *steps], check=True, stdout=subprocess.DEVNULL)
records=[]
for rep in range(6):
    for stage, steps in [
        ('startup', ['key:ctrl+spc','type:Learn','key:ret','sleep:2']),
        ('search', ['key:ctrl+f','type:building','sleep:1']),
        ('scroll', ['key:ret','sleep:1','key:pgdn','sleep:1','key:pgup','sleep:1'])]:
        start=log.stat().st_size
        q(*steps)
        raw=log.read_bytes()[start:].decode(errors='replace')
        metrics=[{'metric': m[0], 'ms': int(m[1])} for m in re.findall(r'learn-perf ([\w-]+) (\d+) ms',raw)]
        if rep: records.append({'repetition':rep,'stage':stage,'samples':metrics})
    if rep == 5: q('shot:'+str(out.with_suffix('.png')))
    q('key:ctrl+w','sleep:1')
out.write_text(json.dumps({'warmup_repetitions':1,'measured_repetitions':5,'records':records},indent=2))
print(out)
