#!/usr/bin/env python3
"""Real Voyage compatibility and timing checks on a disposable full guest.
Run through `cargo xtask web-check`; uses only the image in --output.
"""
import argparse
import json
import os
import re
import socket
import statistics
import subprocess
import sys
import time
import urllib.parse
from pathlib import Path
from web_fixture import PAYLOAD

ROOT=Path(__file__).resolve().parent.parent
FRAME=re.compile(r'frame: ([\w-]+), (\d+), (\d+), (\d+)x(\d+)')
PUBLIC=['https://duckduckgo.com/?q=rust','https://en.wikipedia.org/wiki/Rust_(programming_language)',
        'https://github.com/Sw3bbl3/waveos-aurora-rs','https://developer.mozilla.org/en-US/','https://www.bbc.com/news']

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True)
    p.add_argument('--net',default='virtio');p.add_argument('--baseline',action='store_true');p.add_argument('--local-only',action='store_true')
    p.add_argument('--firefox',action='store_true');p.add_argument('--repetitions',type=int,default=5)
    p.add_argument('--benchmark-only',action='store_true');p.add_argument('--public-only',action='store_true')
    p.add_argument('--connection-only',action='store_true')
    p.add_argument('--only',help='Comma-separated local check names for a focused regression rerun')
    args=p.parse_args();out=args.output.resolve();out.mkdir(parents=True,exist_ok=True)
    assert 1<=args.repetitions<=10
    serial=out/'serial.log'; guest=None
    result={'network':args.net,'baseline':args.baseline,'repetitions':args.repetitions,'checks':[],'public':[],'navigation':[],'workloads':[],'startup':[]}
    fixture=subprocess.Popen([sys.executable,str(ROOT/'tools/web_fixture.py'),'--directory',str(out/'fixture')],stdout=(out/'fixture.log').open('w'),stderr=subprocess.STDOUT)
    def q(*steps):
        subprocess.run([sys.executable,str(ROOT/'tools/qmp.py'),*steps],check=True,stdout=subprocess.DEVNULL)
    def set_link(up):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5);connection.connect(str(Path(os.environ.get('AURORA_SESSION_DIR',ROOT/'target'))/'qemu-qmp.sock'))
            stream=connection.makefile('rwb');json.loads(stream.readline())
            for command in [{'execute':'qmp_capabilities'},
                            {'execute':'set_link','arguments':{'name':'net0','up':up}}]:
                stream.write(json.dumps(command).encode()+b'\n');stream.flush()
                while True:
                    reply=json.loads(stream.readline())
                    if 'error' in reply:raise RuntimeError(reply['error'])
                    if 'return' in reply:break
    def log():return serial.read_text(errors='replace') if serial.exists() else ''
    def events(offset=0,prefix='[voyage] '):
        values=[]
        for line in log()[offset:].splitlines():
            if prefix in line:
                try:values.append(json.loads(line.split(prefix,1)[1]))
                except json.JSONDecodeError:pass
        return values
    def wait_for(predicate,timeout=60):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            value=predicate()
            if value:return value
            if guest is not None and guest.poll() is not None:raise RuntimeError('QEMU exited; see serial.log')
            time.sleep(.15)
        raise TimeoutError('Timed out waiting for guest evidence')
    def shot(name):q('shot:'+str(out/(name+'.png')))
    def boot():
        nonlocal guest
        # A live QMP socket belongs to another run; never silently take it over.
        probe=socket.socket(socket.AF_UNIX);probe.settimeout(.5)
        try:
            probe.connect(str(Path(os.environ.get('AURORA_SESSION_DIR',ROOT/'target'))/'qemu-qmp.sock'))
        except (OSError,TimeoutError):pass
        else:raise RuntimeError('Another VM is running. Stop that VM before web-check.')
        finally:probe.close()
        stream=serial.open('a');before=len(log())
        guest=subprocess.Popen([str(ROOT/'target/debug/xtask'),'run','--no-build','--no-refresh','--headless','--no-sound','--no-keyboard-capture','--net',args.net,'--disk-image',str(out/'session.img')],cwd=ROOT,stdout=stream,stderr=subprocess.STDOUT)
        wait_for(lambda:'Aurora desktop ready' in log()[before:],90)
        time.sleep(1)
    def quit_guest():
        nonlocal guest
        if guest and guest.poll() is None:
            try:q('quit');guest.wait(timeout=8)
            except (subprocess.TimeoutExpired,subprocess.CalledProcessError):guest.kill();guest.wait(timeout=5)
        guest=None
    def terminal(command):
        q('key:ctrl+spc','type:Terminal','key:ret','sleep:1','type:'+command,'key:ret')
    def open_browser(url):
        before=len(log());started=time.monotonic()
        terminal('/System/Apps/Voyage.elf --waveos-benchmark '+url)
        event=wait_for(lambda:next((e for e in events(before) if e.get('event') in ('finished','failed')),None),120)
        result['startup'].append({'url':url,'wall_seconds':round(time.monotonic()-started,3),**event})
        return before
    def matches(uri,expected):
        a,b=urllib.parse.urlsplit(uri or ''),urllib.parse.urlsplit(expected)
        return a.hostname==b.hostname and a.path.rstrip('/')==b.path.rstrip('/')
    def navigate(uri,expected=None,timeout=90,fail=False,probe=True):
        before=len(log());t=time.monotonic()
        q('key:ctrl+l','type:'+uri,'key:ret')
        event=wait_for(lambda:next((e for e in events(before) if e.get('event') in ('finished','failed') and matches(e.get('url'),expected or uri)),None),timeout)
        item={'url':uri,'wall_seconds':round(time.monotonic()-t,3),**event};result['navigation'].append(item)
        if fail:
            assert event['event']=='failed',item
        else:
            assert event['event']=='finished',item
            # A load signal can precede the web process's final focus/layout
            # messages. Wait for a real DOM probe before the next interaction.
            if probe:
                wait_for(lambda:any(e.get('sequence')==event['sequence'] and e.get('page',{}).get('ready')=='complete'
                                    for e in events(before,'[voyage-probe] ')),30)
        return before,event
    def probe_title(before,title):
        return wait_for(lambda:next((e for e in events(before,'[voyage-probe] ') if e.get('page',{}).get('title')==title),None),30)
    def title_event(before,title):
        return wait_for(lambda:any(e.get('title')==title for e in events(before)),30)
    def check(name,operation):
        if args.only and name not in args.only.split(','):return
        try:
            operation();result['checks'].append({'name':name,'status':'passed'});print('PASS',name,flush=True)
        except Exception as error:
            result['checks'].append({'name':name,'status':'failed','error':str(error)});print('FAIL',name,str(error),flush=True)
            shot('failure-'+name)
            q('key:esc')
        (out/'results.json').write_text(json.dumps(result,indent=2)+'\n')
    base='http://10.0.2.2:18765'
    def reconnect():
        navigate(base+'/basic');at=len(log())
        try:
            set_link(False)
            wait_for(lambda:'net: eth0: link down' in log()[at:],10)
            navigate(base+'/basic?offline=1',timeout=20,fail=True);shot('link-disconnected')
        finally:
            at=len(log());set_link(True)
        wait_for(lambda:'dhcp: eth0:' in log()[at:],20)
        probe_title(navigate(base+'/basic?restored=1')[0],'WaveOS web check')
        shot('link-restored')
    try:
        boot()
        marker=out/'account-ready'
        if not args.baseline and not marker.exists():
            q('click:510,378','type:Web Review','click:506,466','type:wave-test','click:734,466','type:wave-test',
              'click:827,599','click:802,666','sleep:0.5','click:804,651','sleep:1.5','click:830,683','sleep:0.7')
            marker.write_text('Disposable fixture account; password wave-test\n')
        if args.benchmark_only:
            # Cold means a new browser/WebKit process, not cleared on-disk
            # caches. Revisit runs in that same process. Report separately.
            for rep in range(args.repetitions):
                probe_title(open_browser(base+'/scroll'),'Scroll performance')
                def workload(name,*steps):
                    start=len(log());q(*steps,'sleep:0.5')
                    frames=FRAME.findall(log()[start:]);times=[int(f[2]) for f in frames]
                    result['workloads'].append(dict(repetition=rep,phase=name,frames=len(frames),pixels=sum(int(f[1]) for f in frames),full_frames=sum(f[0]=='full' for f in frames),total_paint_ms=sum(times),mean_paint_ms=round(statistics.mean(times),2) if times else 0,max_paint_ms=max(times,default=0)))
                for phase in ['new-process','revisit']:
                    if phase=='revisit':probe_title(navigate(base+'/scroll')[0],'Scroll performance')
                    workload(phase,'move:700,500','wheel:10','sleep:0.4','wheel:-10','sleep:0.4')
                workload('small-update','click:380,297')
                q('click:170,297','sleep:1')
                workload('typing','type:waveos browser check')
                q('click:750,400','sleep:1')
                workload('pointer','glide:500,500,900,600','glide:900,600,500,500')
                workload('dragging','keydown:meta_l','drag:700,500,760,480','drag:760,480,700,500','keyup:meta_l')
                shot('scroll-'+str(rep))
                q('click:750,400','move:1250,500','sleep:2');at=len(log());q('sleep:5')
                idle=FRAME.findall(log()[at:])
                result.setdefault('idle',[]).append({'frames':len(idle),'seconds':5,
                    'full_frames':sum(f[0]=='full' for f in idle),'pixels':sum(int(f[1]) for f in idle),
                    'total_paint_ms':sum(int(f[2]) for f in idle)})
                q('key:ctrl+w','sleep:3')
                print('New process and revisit',rep+1,'complete',flush=True)
            return
        if not args.baseline:
            before=len(log());terminal('netcheck --serial --clock-check')
            check('utc-zone-invariance',lambda:wait_for(lambda:'CLOCK CHECK PASS' in log()[before:],30))
            q('type:exit','key:ret','sleep:1')
            before=len(log());terminal('netcheck --serial '+base+'/basic')
            wait_for(lambda:'NETCHECK ' in log()[before:],45)
            check('native-connection',lambda: (_ for _ in ()).throw(AssertionError('NETCHECK failed')) if ('NETCHECK PASS' not in log()[before:]) != (args.net=='none') else None)
            q('type:exit','key:ret','sleep:1')
            if not args.local_only and args.net!='none':
                before=len(log());terminal('netcheck --serial https://example.com/')
                check('native-public-dns-and-tls',lambda:wait_for(lambda:'NETCHECK PASS' in log()[before:],60))
                q('type:exit','key:ret','sleep:1')
        before=open_browser(base+'/basic')
        if args.net=='none':
            check('offline-error-page',lambda:wait_for(lambda:any(e.get('event')=='failed' for e in events(before)),40))
            shot('offline-error');return
        if args.connection_only:
            check('link-loss-and-reconnect',reconnect)
            return
        if not args.public_only:
            check('initial-page-and-javascript',lambda:probe_title(before,'WaveOS web check'))
            def javascript():
                at=len(log());q('key:tab','key:spc');title_event(at,'Counter passed')
            check('javascript-button',javascript)
            check('redirect',lambda:probe_title(navigate(base+'/redirect',base+'/basic')[0],'WaveOS web check'))
            check('gzip-and-chunking',lambda:probe_title(navigate(base+'/gzip')[0],'WaveOS web check'))
            check('verified-https',lambda:probe_title(navigate('https://10.0.2.2:18443/basic')[0],'WaveOS web check'))
            def invalid_certificate():
                _,e=navigate('https://10.0.2.2:18444/basic',fail=True);assert e.get('tls',0)!=0,e
                shot('certificate-error')
            check('invalid-certificate-rejected',invalid_certificate)
            check('recover-after-error',lambda:probe_title(navigate(base+'/basic')[0],'WaveOS web check'))
            def retry():
                for method,steps in [('keyboard',['key:ctrl+r']),('button',['click:124,125'])]:
                    navigate(base+'/retry?case='+method,fail=True);q('sleep:1');at=len(log());q(*steps)
                    wait_for(lambda:any(e.get('event')=='finished' and matches(e.get('url'),base+'/retry') for e in events(at)),30)
                    probe_title(at,'Retry succeeded')
            check('retry-keyboard-and-button',retry)
            def storage():
                navigate(base+'/storage');at=len(log());q('click:130,289');title_event(at,'Storage saved')
                probe_title(navigate(base+'/storage')[0],'Storage restored')
            check('local-storage',storage)
            def sign_in():
                navigate(base+'/login');at=len(log());q('key:tab','key:tab','key:tab','key:ret');title_event(at,'Signed in')
            check('sign-in-and-cookie',sign_in)
            def download():
                saved=[]
                for _ in range(2):
                    at=len(log());q('key:ctrl+l','type:'+base+'/download','key:ret')
                    event=wait_for(lambda:next((e for e in events(at,'[voyage-download] ') if e.get('event')=='finished'),None),45)
                    assert event['bytes']==len(PAYLOAD),event
                    saved.append(event['path'])
                assert len(set(saved))==2,saved
                result['downloads']=saved
                shot('downloads')
            check('downloads-and-duplicate-names',download)
            # Dismiss the two completed rows, preserving a stable document focus.
            q('click:1148,175','click:1148,175')
            def upload():
                navigate(base+'/upload');at=len(log())
                path=(result.get('downloads') or ['/Downloads/wave-check.txt'])[0]
                q('click:130,289','sleep:2','key:ctrl+l','sleep:0.5','type:'+path,'key:ret',
                  'sleep:2','key:alt+o','sleep:2','click:500,289')
                title_event(at,'Upload complete')
            check('upload',upload)
            def cancel_download():
                navigate(base+'/basic');at=len(log());q('key:ctrl+l','type:'+base+'/large-download','key:ret')
                wait_for(lambda:any(e.get('event')=='started' for e in events(at,'[voyage-download] ')),30)
                q('click:1148,175')
                wait_for(lambda:any(e.get('event')=='failed' and e.get('cancelled') for e in events(at,'[voyage-download] ')),30)
                q('click:1148,175');shot('download-cancelled')
            check('download-cancellation',cancel_download)
            def cancellation():
                q('key:ctrl+l','type:'+base+'/slow','key:ret','sleep:0.5')
                at=len(log());q('key:ctrl+r')
                wait_for(lambda:any(e.get('event')=='started' for e in events(at)),20)
                at=len(log());q('key:esc')
                wait_for(lambda:any(e.get('event')=='cancelled' for e in events(at)),20)
                probe_title(navigate(base+'/basic')[0],'WaveOS web check')
            check('stop-and-recover',cancellation)
            check('interrupted-connection',lambda:navigate(base+'/disconnect',fail=True))
            check('link-loss-and-reconnect',reconnect)
            for rep in range(0 if args.only else args.repetitions):
                for phase in ['navigation','revisit']:
                    before,_=navigate(base+'/scroll');probe_title(before,'Scroll performance')
                    start=len(log());q('move:700,500','wheel:10','sleep:0.4','wheel:-10','sleep:0.4')
                    frames=FRAME.findall(log()[start:]);times=[int(f[2]) for f in frames]
                    result['workloads'].append(dict(repetition=rep,phase=phase,frames=len(frames),full_frames=sum(f[0]=='full' for f in frames),mean_paint_ms=round(statistics.mean(times),2) if times else 0,max_paint_ms=max(times,default=0)))
                print('Benchmark repetition',rep+1,'complete',flush=True)
            shot('scroll-page')
            navigate(base+'/cache');q('move:1250,500','sleep:2');at=len(log());q('sleep:5')
            result['idle_frames']=len(FRAME.findall(log()[at:]));result['idle_seconds']=5
            check('persistent-session-before-reboot',lambda:probe_title(navigate(base+'/private')[0],'Signed in'))
            q('key:ctrl+w','sleep:2');quit_guest();boot()
            before=open_browser(base+'/private')
            check('session-across-reboot',lambda:probe_title(before,'Signed in'))
            check('storage-across-reboot',lambda:probe_title(navigate(base+'/storage')[0],'Storage restored'))
        if not args.local_only:
            for uri in PUBLIC:
                try:
                    before,event=navigate(uri,timeout=120,probe=False)
                    q('sleep:1')
                    probe=next((e.get('page') for e in reversed(events(before,'[voyage-probe] ')) if e.get('sequence')==event['sequence']),None)
                    result['public'].append({'url':uri,'status':'loaded','event':event,'page':probe})
                except Exception as error:result['public'].append({'url':uri,'status':'failed','error':str(error)})
                shot('public-'+str(len(result['public'])))
                print('Public site:',result['public'][-1]['status'],uri,flush=True)
        if args.firefox:
            q('key:ctrl+w','sleep:2');terminal('/System/Apps/Firefox.elf '+base+'/basic');q('sleep:35');shot('firefox-comparison')
            result['firefox_comparison']='Screenshot captured; inspect rendering manually.'
    finally:
        quit_guest();fixture.terminate()
        try:fixture.wait(timeout=5)
        except subprocess.TimeoutExpired:fixture.kill();fixture.wait()
        (out/'results.json').write_text(json.dumps(result,indent=2)+'\n')
        failures=[c for c in result['checks'] if c['status']=='failed']
        print(out/'results.json',flush=True)
        if failures:raise SystemExit(f'{len(failures)} local browser checks failed')

if __name__=='__main__':main()
