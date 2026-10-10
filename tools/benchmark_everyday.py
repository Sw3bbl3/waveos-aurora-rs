#!/usr/bin/env python3
"""Collect five idle/interaction trials after warm-up on a disposable QEMU disk.
Enable /Settings/runtime-metrics before opening Files/Settings. Start at an idle
1920x1080 or 1280x800 fresh desktop and supply the actual resolution.
The collector closes the initial Welcome window and verifies both app launches.
Usage: benchmark_everyday.py SERIAL_LOG OUTPUT_JSON WIDTH HEIGHT
"""
import json, pathlib, re, statistics, subprocess, sys, time
root=pathlib.Path(__file__).resolve().parent.parent
log,out=map(pathlib.Path,sys.argv[1:3]);width,height=map(int,sys.argv[3:5])
def q(*steps):
 subprocess.run([sys.executable,str(root/'tools/qmp.py'),f'res:{width},{height}',*steps],check=True,stdout=subprocess.DEVNULL)
def parse(raw):
 runtime=[]
 for name,elapsed,ticks,events,cpu,memory,tasks,frames in re.findall(r'runtime: (.*?), elapsed=(\d+), ticks=(\d+), events=(\d+), cpu_ms=(\d+), memory=(\d+), tasks=(\d+), frames=(\d+)',raw):
  runtime.append(dict(name=name,elapsed_ms=int(elapsed),ticks=int(ticks),events=int(events),cpu_ms=int(cpu),memory=int(memory),tasks=int(tasks),frames=int(frames)))
 samples=[dict(kind=k,pixels=int(p),ms=int(ms),width=int(w),height=int(h)) for k,p,ms,w,h in re.findall(r'frame: ([\w-]+), (\d+), (\d+), (\d+)x(\d+)',raw)]
 processes=[dict(name=n,cpu_ms=int(c),memory_kib=int(m),threads=int(t)) for n,c,m,t in re.findall(r'runtime-process: (.*?), cpu_ms=(\d+), memory_kib=(\d+), threads=(\d+)',raw)]
 for sample in runtime:
  sample['timing_valid']=9000<=sample['elapsed_ms']<=12000
 return dict(runtime=runtime,frames=samples,processes=processes)
deadline=time.monotonic()+30
while 'Aurora desktop ready' not in log.read_text(errors='replace'):
 if time.monotonic()>deadline:raise SystemExit('Guest desktop did not become ready')
 time.sleep(.2)
time.sleep(1)
q('key:ctrl+w','sleep:0.5')
for app in ['Files','Settings']:
 start=log.stat().st_size
 q('key:ctrl+spc','sleep:0.3','type:'+app,'key:ret','sleep:2')
 if '/System/Apps/'+app+'.elf' not in log.read_bytes()[start:].decode(errors='replace'):
  raise SystemExit(app+' did not launch: discard this run and start at a clean desktop')
records=[]
for rep in range(6):
 start=log.stat().st_size
 time.sleep(22)
 sample=parse(log.read_bytes()[start:].decode(errors='replace'))
 if rep:records.append(dict(repetition=rep,workload='idle',**sample))
 start=log.stat().st_size
 q('keydown:alt','key:tab','keyup:alt','sleep:0.5','keydown:alt','key:tab','keyup:alt','sleep:0.5')
 if rep:records.append(dict(repetition=rep,workload='switching',**parse(log.read_bytes()[start:].decode(errors='replace'))))
 # Do not leave a blinking caret in an idle trial.
 # No text entry here: blinking carets would contaminate idle comparisons.
 time.sleep(2)
out.parent.mkdir(parents=True,exist_ok=True)
out.write_text(json.dumps(dict(resolution=[width,height],cpus=4,ram_mib=512,warmup=1,repetitions=5,records=records),indent=2))
q('shot:'+str(out.with_suffix('.png')))
print(out)
