#!/usr/bin/env python3
"""Summarize recorded idle wakeups and composition frames, retaining limits.
Usage: summarize_everyday.py baseline.json candidate.json
"""
import collections,json,math,statistics,sys

def summarize(path):
 data=json.load(open(path));idle=collections.defaultdict(list);process=collections.defaultdict(list)
 frames=[];excluded=0;cpu=[];memory=[];tasks=[]
 for row in data['records']:
  if row['workload']=='switching':frames.extend(f['ms'] for f in row['frames'])
  if row['workload']!='idle':continue
  previous={}
  for s in row['runtime']:
   valid=9000<=s['elapsed_ms']<=12000
   if not valid:excluded+=1
   if valid and s['events']==0:
    idle[s['name']].append(s['ticks']*1000/s['elapsed_ms'])
    memory.append(s['memory']);tasks.append(s['tasks'])
   old=previous.get(s['name'])
   if old and valid and 9000<=old['elapsed_ms']<=12000 and old['events']==0 and s['events']==0:
    delta=s['cpu_ms']-old['cpu_ms']
    if 0<=delta<=4*s['elapsed_ms']:cpu.append(100*delta/(4*s['elapsed_ms']))
   previous[s['name']]=s
  for s in row.get('processes',[]):process[s['name']].append(s)
 frames.sort()
 return dict(resolution=data['resolution'],repetitions=data['repetitions'],
  idle={name:dict(samples=len(values),median_wakeups_per_second=round(statistics.median(values),3)) for name,values in idle.items()},
  idle_guest_cpu_percent_median=round(statistics.median(cpu),3) if cpu else None,
  idle_cpu_sample_count=len(cpu),excluded_clock_discontinuities=excluded,
  median_guest_memory_bytes=statistics.median(memory) if memory else None,
  max_guest_tasks=max(tasks) if tasks else None,
  processes={name:dict(median_memory_kib=statistics.median(v['memory_kib'] for v in values),max_threads=max(v['threads'] for v in values)) for name,values in process.items()},
  switching=dict(frames=len(frames),median_composition_ms=statistics.median(frames),p95_composition_ms=frames[math.ceil(.95*len(frames))-1]))

before,after=map(summarize,sys.argv[1:3]);reduction={}
for name,stat in before['idle'].items():
 if name in after['idle'] and stat['median_wakeups_per_second']:
  reduction[name]=round(100*(1-after['idle'][name]['median_wakeups_per_second']/stat['median_wakeups_per_second']),2)
print(json.dumps(dict(baseline=before,candidate=after,idle_wakeup_reduction_percent=reduction,
 caveats=['Samples spanning VM clock discontinuities are excluded from wakeup rates.',
 'CPU values are descriptive guest counters; sparse samples and host load do not establish an overall CPU-saving percentage.',
 'Composition time excludes client drawing and input latency. No locked frame-rate claim.',
 'Files adds a tab row: its default window is 880x580 versus 880x540 in the baseline.']),indent=2))
