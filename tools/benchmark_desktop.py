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
  ('typing', ['click:500,200','key:ctrl+a','type:Aurora desktop performance test.','sleep:1']),
  ('dragging', ['down:550,110','sleep:0.3','move:750,210','sleep:0.5','up:750,210','sleep:1','down:750,210','sleep:0.3','move:550,110','sleep:0.5','up:550,110','sleep:1']),
  ('switching', ['keydown:alt','sleep:0.2','key:tab','sleep:0.5','keyup:alt','sleep:1','keydown:alt','sleep:0.2','key:tab','sleep:0.5','keyup:alt','sleep:1','click:500,200','sleep:0.5']),
  ('settings', ['key:ctrl+spc','type:Settings','key:ret','sleep:1.5','key:ctrl+f','type:display','sleep:0.5','key:ctrl+w','sleep:1','click:500,200','sleep:0.5'])]:
  time.sleep(0.5)
  start=log.stat().st_size
  q(*steps)
  raw=log.read_bytes()[start:].decode(errors='replace')
  frames=[{'kind':m[0], 'pixels':int(m[1]), 'ms':int(m[2]), 'width':int(m[3]), 'height':int(m[4])} for m in re.findall(r'frame: ([\w-]+), (\d+), (\d+), (\d+)x(\d+)',raw)]
  q('shot:'+str(output.with_name(output.stem+'-'+str(rep)+'-'+name+'.png')))
  if rep: records.append({'repetition':rep,'workload':name,'frames':frames})
output.write_text(json.dumps({'warmup_repetitions':1,'measured_repetitions':5,'records':records},indent=2))
q('shot:'+str(output.with_suffix('.png')))
print(output)
