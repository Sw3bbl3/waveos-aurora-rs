#!/usr/bin/env python3
"""Repeat deterministic guest workloads, recording serial frame samples by stage.
Run a headless VM with matching settings first; LOG is its redirected stdout.
Each workload gets one unreported warm-up followed by five measured repetitions.
"""
import json, pathlib, re, subprocess, sys, time
root = pathlib.Path(__file__).resolve().parent.parent
log, output = map(pathlib.Path, sys.argv[1:3])
def q(*steps):
 subprocess.run([sys.executable, str(root/'tools/qmp.py'), *steps], check=True, stdout=subprocess.DEVNULL)
def wait_ready():
 for _ in range(180):
  if log.exists() and 'Aurora desktop ready' in log.read_text(errors='replace'): return
  time.sleep(1)
 raise SystemExit('desktop did not boot')
wait_ready();time.sleep(3)
q('key:ctrl+spc','type:Notes','key:ret','sleep:2')
# Fresh seeded desktop, with Notes focused. Identical coordinates and workload
# on baseline and candidate. Discard initial app startup from measured samples.
records=[]
for rep in range(6):
 for name, steps in [
  ('typing', ['type:Aurora desktop performance test.','sleep:1']),
  ('dragging', ['drag:550,110,750,210','drag:750,210,550,110','sleep:1']),
  ('switching', ['key:alt+tab','sleep:0.3','key:alt+tab','sleep:0.5']),
  ('settings', ['key:ctrl+spc','type:Settings','key:ret','sleep:0.5','key:ctrl+f','type:display','sleep:0.5','key:ctrl+w','sleep:0.5'])]:
  start=log.stat().st_size
  q(*steps)
  raw=log.read_bytes()[start:].decode(errors='replace')
  frames=[{'kind':m[0], 'pixels':int(m[1]), 'ms':int(m[2]), 'width':int(m[3]), 'height':int(m[4])} for m in re.findall(r'frame: ([\w-]+), (\d+), (\d+), (\d+)x(\d+)',raw)]
  if rep: records.append({'repetition':rep,'workload':name,'frames':frames})
output.write_text(json.dumps({'warmup_repetitions':1,'measured_repetitions':5,'records':records},indent=2))
q('shot:'+str(output.with_suffix('.png')))
print(output)
