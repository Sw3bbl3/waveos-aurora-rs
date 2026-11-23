#!/usr/bin/env python3
"""Exercise a disposable Firefox YouTube tab via localhost BiDi and native QMP.

Start Firefox with its explicit remote-debugging flag and localhost forwarding.
No media or browser preferences are changed. This types a temporary search query,
scrolls the page, and plays/pauses its video muted. Never use a personal session.
rAF gaps describe main-thread scheduling, not displayed frames; video quality
counters describe the browser's own playback pipeline, not host presentation.
"""
import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path
from firefox_probe import Firefox, STATE

ROOT = Path(__file__).resolve().parent.parent
FRAME = re.compile(r'frame: ([\w-]+), (\d+), (\d+), (\d+)x(\d+)')
START = '''(()=>{window.__waveBench={start:performance.now(),last:0,gaps:[],events:[],maxScrollY:scrollY};
const b=window.__waveBench;function frame(t){if(window.__waveBench!==b)return;
if(b.last)b.gaps.push(t-b.last);b.last=t;b.maxScrollY=Math.max(b.maxScrollY,scrollY);requestAnimationFrame(frame)}requestAnimationFrame(frame);
const v=document.querySelector('video');if(v){const q=v.getVideoPlaybackQuality();b.videoStart={time:v.currentTime,total:q.totalVideoFrames,dropped:q.droppedVideoFrames};}if(v)for(const name of ['waiting','playing','stalled','pause','error'])
v.addEventListener(name,()=>{if(window.__waveBench===b)b.events.push({name,at:performance.now()-b.start,time:v.currentTime})},{once:true});
return true})()'''
END = '''JSON.stringify((()=>{const b=window.__waveBench;window.__waveBench=null;
const s=b.gaps.slice().sort((a,b)=>a-b),v=document.querySelector('video'),q=v?.getVideoPlaybackQuality();
return {duration_ms:performance.now()-b.start,raf:{frames:s.length,mean_ms:s.reduce((a,b)=>a+b,0)/s.length,
p95_ms:s[Math.floor(s.length*.95)],max_ms:s.at(-1),over50:s.filter(x=>x>50).length,over100:s.filter(x=>x>100).length},
events:b.events,scrollY,maxScrollY:b.maxScrollY,videoStart:b.videoStart,search:document.querySelector('input[name="search_query"]')?.value,
video:v?{paused:v.paused,time:v.currentTime,width:v.videoWidth,height:v.videoHeight,ready:v.readyState,
total:q.totalVideoFrames,dropped:q.droppedVideoFrames,buffered:[...Array(v.buffered.length)].map((_,i)=>[v.buffered.start(i),v.buffered.end(i)]),
quality:document.querySelector('#movie_player')?.getPlaybackQuality?.()}:null}})())'''

def q(*steps):
    subprocess.run([sys.executable,str(ROOT/'tools/qmp.py'),*steps],check=True,stdout=subprocess.DEVNULL)

def cpu(pid):
    raw=subprocess.check_output(['ps','-o','time=','-p',str(pid)],text=True).strip()
    parts=raw.split(':')
    return sum(float(p)*60**i for i,p in enumerate(reversed(parts)))

def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('output',type=Path)
    ap.add_argument('--log',type=Path,required=True)
    ap.add_argument('--pid',type=int,required=True)
    ap.add_argument('--play-seconds',type=int,default=20)
    a=ap.parse_args(); browser=Firefox(timeout=60); result={'instrumented':True,'viewport':[1280,800],'phases':[]}
    def evaluate(js):
        value=browser.evaluate(js)
        try:return json.loads(value)
        except (TypeError,json.JSONDecodeError):return value
    def phase(name,action):
        browser.evaluate(START); offset=a.log.stat().st_size;c=cpu(a.pid);t=time.monotonic()
        action()
        data=evaluate(END)
        elapsed=time.monotonic()-t; used=cpu(a.pid)-c
        frames=FRAME.findall(a.log.read_bytes()[offset:].decode(errors='replace'))
        data.update(name=name,host_seconds=elapsed,qemu_cpu_seconds=used,qemu_cpu_percent=100*used/elapsed,
            compositor={'frames':len(frames),'full':sum(f[0]=='full' for f in frames),'paint_ms':sum(int(f[2]) for f in frames)})
        result['phases'].append(data);a.output.write_text(json.dumps(result,indent=2)+'\n');print(name,flush=True)
    try:
        deadline=time.monotonic()+120
        while not browser.evaluate("document.readyState==='complete' && !!document.querySelector('input[name=search_query]') && (document.querySelector('video')?.videoWidth||0)>0 && document.querySelectorAll('*').length>3000"):
            if time.monotonic()>deadline:raise RuntimeError('YouTube did not reach the comparable loaded state')
            time.sleep(2)
        time.sleep(10)  # let dynamic watch-page hydration settle on both runs
        result['state']=evaluate(STATE)
        browser.evaluate("document.querySelector('video')?.pause();window.scrollTo({top:0,behavior:'instant'})")
        phase('paused_idle',lambda:time.sleep(8))
        focused=browser.evaluate('''(()=>{const e=document.querySelector('input[name="search_query"]');if(!e)return false;e.focus();return true})()''')
        if not focused:raise RuntimeError('YouTube search input unavailable; wait for page to finish loading')
        phase('typing',lambda:q('key:ctrl+a','type:waveos youtube test','sleep:2'))
        q('key:esc','key:tab')
        phase('scroll',lambda:q('move:1100,650','wheel:10','sleep:1','wheel:-10','sleep:2'))
        result['scroll_verified']=result['phases'][-1]['maxScrollY']>0
        result['play_start']=evaluate('''(async()=>{const v=document.querySelector('video');v.muted=true;
return JSON.stringify(await Promise.race([v.play().then(()=>({playing:true})).catch(e=>({error:String(e)})),
new Promise(r=>setTimeout(()=>r({error:'play timeout'}),10000))]))})()''')
        phase('playback',lambda:time.sleep(a.play_seconds))
        browser.evaluate("document.querySelector('video')?.pause()")
        q('shot:'+str(a.output.with_suffix('.png').resolve()))
        a.output.write_text(json.dumps(result,indent=2)+'\n')
    except Exception as error:
        result['error']=str(error)
        a.output.write_text(json.dumps(result,indent=2)+'\n')
        raise
    finally:browser.close()

if __name__=='__main__':main()
