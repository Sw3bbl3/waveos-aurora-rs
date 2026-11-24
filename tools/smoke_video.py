#!/usr/bin/env python3
"""Video playback in the guest browsers, measured: run through
`cargo xtask video-check`, on the disposable disk in --output.

For each browser it boots the guest (sound recorded to a WAV file), opens
the browser on the fixture's MSE page (tools/video_mse.html), presses its
Start button and waits until the page has posted a result for every case.
Meanwhile it samples where the virtual processors are (user code, the
kernel, idle) through QEMU's monitor. Afterwards it looks for gaps in the
recorded tone, and checks each case against the minimums given:

    --min-fps N        presented (or decoded) frames per second
    --max-drift MS     A/V drift (presented frame against the media clock)
    --max-gaps N       gaps in the sound per case

Writes --output/video-results.json and prints a table.
"""
import argparse
import json
import os
import re
import socket
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PORT = 18766
# The page is loaded from 127.0.0.1 (a secure context, which WebCodecs
# needs), forwarded to the host by tools/video_forward.py in the guest.
BASE = f'http://127.0.0.1:{PORT}'
FORWARD = f'wget -qO /tmp/forward.py http://10.0.2.2:{PORT}/forward.py; python3 /tmp/forward.py {PORT} & '
BROWSERS = {
    # The launchers, with the browsers' own media logs on the system log.
    'firefox': "sh -c '" + FORWARD + "MOZ_LOG=cubeb:3,PlatformDecoderModule:3 exec /System/Linux/bin/firefox \"{url}\" 2>/dev/kmsg'",
    'voyage': "sh -c '" + FORWARD + "GST_DEBUG=2,alsa*:3,oss*:3 exec /System/Linux/bin/voyage \"{url}\" 2>/dev/kmsg'",
}


def session():
    return Path(os.environ.get('AURORA_SESSION_DIR', ROOT / 'target'))


def qmp(*steps):
    subprocess.run([sys.executable, str(ROOT / 'tools/qmp.py'), *steps], check=True, stdout=subprocess.DEVNULL)


class Sampler(threading.Thread):
    """Samples every vCPU through the monitor: (host time, user, kernel, idle)."""

    def __init__(self):
        super().__init__(daemon=True)
        self.samples = []
        self.stop = False

    def run(self):
        try:
            s = socket.socket(socket.AF_UNIX)
            s.connect(str(session() / 'qemu-monitor.sock'))
            s.settimeout(10)
            self.read(s)
            while not self.stop:
                s.sendall(b'info registers -a\n')
                text = self.read(s).decode(errors='replace')
                user = kernel = idle = 0
                for m in re.finditer(r'RIP=[0-9a-f]{16} .*?CPL=(\d).*?HLT=(\d)', text):
                    if m.group(2) == '1':
                        idle += 1
                    elif m.group(1) == '3':
                        user += 1
                    else:
                        kernel += 1
                self.samples.append((time.time(), user, kernel, idle))
                time.sleep(0.1)
        except OSError:
            pass

    @staticmethod
    def read(s):
        data = b''
        while not data.endswith(b'(qemu) '):
            chunk = s.recv(65536)
            if not chunk:
                break
            data += chunk
        return data

    def split(self, start, end):
        rows = [r for r in self.samples if start <= r[0] <= end]
        total = sum(r[1] + r[2] + r[3] for r in rows)
        if not total:
            return None
        return {k: round(100 * sum(r[i] for r in rows) / total, 1) for i, k in ((1, 'user'), (2, 'kernel'), (3, 'idle'))}


def tone_gaps(wav, windows):
    """Gaps (runs of near-silence of 10 ms or more) in the recorded tone
    within each (start, end) window of seconds into the recording."""
    data = wav.read_bytes()
    if data[:4] != b'RIFF':
        return None
    pos, fmt, pcm = 12, None, None
    while pos + 8 <= len(data):
        cid, size = data[pos:pos + 4], struct.unpack('<I', data[pos + 4:pos + 8])[0]
        body = data[pos + 8:pos + 8 + size]
        if cid == b'fmt ':
            fmt = struct.unpack('<HHIIHH', body[:16])
        elif cid == b'data':
            pcm = body if size else data[pos + 8:]
        pos += 8 + size + (size & 1)
    if not fmt or pcm is None or fmt[5] != 16:
        return None
    channels, rate = fmt[1], fmt[2]
    frame = 2 * channels
    win = rate // 100  # 10 ms
    levels = []
    for at in range(0, len(pcm) - frame * win, frame * win):
        chunk = memoryview(pcm)[at:at + frame * win].cast('h')
        levels.append(max(abs(min(chunk)), abs(max(chunk))))
    out = []
    for start, end in windows:
        a, b = int(start * 100), min(int(end * 100), len(levels))
        seg = levels[a:b]
        loud = [i for i, v in enumerate(seg) if v > 1000]
        if not loud:
            out.append({'tone_s': 0, 'gaps': 0, 'gap_ms': 0})
            continue
        seg = seg[loud[0]:loud[-1] + 1]
        gaps, gap_ms, run = 0, 0, 0
        for v in seg + [32767]:
            if v <= 1000:
                run += 1
            else:
                if run:
                    gaps += 1
                    gap_ms += run * 10
                run = 0
        out.append({'tone_s': round(len(seg) / 100, 2), 'gaps': gaps, 'gap_ms': gap_ms})
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--clips', type=Path, default=ROOT / 'target/video-check/clips')
    p.add_argument('--browsers', default='firefox,voyage')
    p.add_argument('--cases', default='h264-360,h264-720,h264-1080,vp9-360,vp9-720,vp9-1080,av1-360,av1-720,av1-1080')
    p.add_argument('--secs', type=int, default=20)
    p.add_argument('--decode', default='h264-360,h264-720,h264-1080,vp9-360,vp9-720,vp9-1080,av1-360,av1-720,av1-1080',
                   help='WebCodecs decoding benchmarks to run first (empty: none)')
    p.add_argument('--muted', action='store_true')
    p.add_argument('--net', default='virtio')
    p.add_argument('--smp', default='4')
    p.add_argument('--min-fps', type=float, default=0)
    p.add_argument('--max-drift', type=float, default=0)
    p.add_argument('--max-gaps', type=int, default=-1)
    p.add_argument('--label', default='', help='a name for this run in the results')
    args = p.parse_args()
    out = args.output.resolve()
    clips = args.clips.resolve()
    results_file = out / 'results.jsonl'
    serial = out / 'serial.log'
    fixture = subprocess.Popen([sys.executable, str(ROOT / 'tools/video_fixture.py'), '--directory', str(clips), '--port', str(PORT),
                                '--results', str(results_file)],
                               stdout=(out / 'fixture.log').open('w'), stderr=subprocess.STDOUT)
    guest = None
    summary = {'label': args.label, 'browsers': {}}

    def log():
        return serial.read_text(errors='replace') if serial.exists() else ''

    def records(offset):
        if not results_file.exists():
            return []
        lines = results_file.read_text().splitlines()[offset:]
        return [json.loads(line) for line in lines if line.strip()]

    def wait_for(predicate, timeout, what):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            value = predicate()
            if value:
                return value
            if guest is not None and guest.poll() is not None:
                raise RuntimeError('QEMU exited; see serial.log')
            time.sleep(0.5)
        raise TimeoutError('timed out waiting for ' + what)

    def boot(wav):
        nonlocal guest
        probe = socket.socket(socket.AF_UNIX)
        probe.settimeout(.5)
        try:
            probe.connect(str(session() / 'qemu-qmp.sock'))
        except OSError:
            pass
        else:
            raise RuntimeError('another VM is using this session folder')
        finally:
            probe.close()
        before = len(log())
        stream = serial.open('a')
        guest = subprocess.Popen([str(ROOT / 'target/debug/xtask'), 'run', '--no-build', '--no-refresh', '--headless',
                                  '--no-keyboard-capture', '--audio-wav', str(wav), '--smp', args.smp, '--net', args.net,
                                  '--disk-image', str(out / 'session.img')],
                                 cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT)
        wait_for(lambda: 'Aurora desktop ready' in log()[before:], 180, 'the desktop')
        time.sleep(1)
        marker = out / 'account-ready'
        if not marker.exists():
            # The same disposable account as web-check's.
            qmp('click:510,378', 'type:Web Review', 'click:506,466', 'type:wave-test', 'click:734,466', 'type:wave-test',
                'click:827,599', 'click:802,666', 'sleep:0.5', 'click:804,651', 'sleep:1.5', 'click:830,683', 'sleep:0.7')
            marker.write_text('Disposable fixture account; password wave-test\n')

    def quit_guest():
        nonlocal guest
        if guest and guest.poll() is None:
            try:
                qmp('quit')
                guest.wait(timeout=10)
            except (subprocess.TimeoutExpired, subprocess.CalledProcessError):
                guest.kill()
                guest.wait(timeout=5)
        guest = None

    failures = []
    try:
        for browser in args.browsers.split(','):
            wav = out / f'audio-{browser}.wav'
            wav.unlink(missing_ok=True)
            boot(wav)
            boot_time = time.time()
            sampler = Sampler()
            sampler.start()
            offset = len(records(0))
            url = (f'{BASE}/mse.html?cases={args.cases}&secs={args.secs}&tag={browser}&decode={args.decode}'
                   + ('&muted=1' if args.muted else ''))
            launched = time.time()
            qmp('key:ctrl+spc', 'type:Terminal', 'key:ret', 'sleep:1.5', 'type:' + BROWSERS[browser].format(url=url), 'key:ret')
            ready = wait_for(lambda: next((r for r in records(offset) if r.get('kind') == 'ready'), None), 400, browser + ' page')
            page_s = round(ready['host_time'] - launched, 1)
            time.sleep(3)
            qmp('shot:' + str(out / f'{browser}-ready.png'))
            # Start: the button covers the page.
            qmp('click:640,500')
            budget = len(args.cases.split(',')) * (args.secs + 150) + len(args.decode.split(',')) * 120 + 60
            try:
                wait_for(lambda: any(r.get('kind') == 'done' for r in records(offset)), budget, browser + ' cases')
            except TimeoutError as e:
                failures.append(f'{browser}: {e}')
            qmp('shot:' + str(out / f'{browser}-end.png'))
            sampler.stop = True
            got = records(offset)
            cases = [r for r in got if r.get('kind') == 'case']
            caps = next((r for r in got if r.get('kind') == 'caps'), None)
            decodes = [r for r in got if r.get('kind') == 'decode']
            # Where each case's measured stretch falls in the recording.
            windows = []
            for r in cases:
                end = r['host_time'] - boot_time
                windows.append((max(0, end - r.get('wall_s', 0) - 2), end))
            gaps = tone_gaps(wav, windows) if wav.exists() and not args.muted else None
            for i, r in enumerate(cases):
                end = r['host_time']
                r['cpu'] = sampler.split(end - r.get('wall_s', 0), end)
                if gaps:
                    r['audio'] = gaps[i]
                fps = r.get('presented_fps') or r.get('decoded_fps') or 0
                if r.get('status') != 'played':
                    if r.get('status') != 'unsupported':
                        failures.append(f"{browser} {r['case']}: {r.get('status')} {r.get('errors')}")
                    continue
                if args.min_fps and fps < args.min_fps:
                    failures.append(f"{browser} {r['case']}: {fps} frames/s < {args.min_fps}")
                if args.max_drift and abs(r.get('drift_ms_mean') or 0) > args.max_drift:
                    failures.append(f"{browser} {r['case']}: A/V drift {r.get('drift_ms_mean')} ms")
                if args.max_gaps >= 0 and gaps and gaps[i]['gaps'] > args.max_gaps:
                    failures.append(f"{browser} {r['case']}: {gaps[i]['gaps']} gaps in the sound")
            summary['browsers'][browser] = {'page_s': page_s, 'caps': caps, 'decode': decodes, 'cases': cases,
                                            'serial_bytes': len(log())}
            serial_log = log()
            summary['browsers'][browser]['media_log'] = [
                line for line in serial_log.splitlines()
                if re.search(r'cubeb|PlatformDecoder|FFmpeg|alsa|ALSA|oss|GStreamer|gst|audio|snd', line)][-200:]
            quit_guest()
            (out / 'video-results.json').write_text(json.dumps(summary, indent=2) + '\n')
    finally:
        quit_guest()
        fixture.terminate()
        (out / 'video-results.json').write_text(json.dumps(summary, indent=2) + '\n')
    for browser, b in summary['browsers'].items():
        for r in b.get('decode', []):
            print(f"{browser:8} decode {r['case']:10} {r.get('status', ''):12} {r.get('fps', ''):>7} frames/s "
                  f"({r.get('frames', '')} of {r.get('submitted', '')} in {r.get('ms', '')} ms) {r.get('error', '')}")
    print(f"{'browser':8} {'case':10} {'status':9} {'ttff':>6} {'dec/s':>6} {'pres/s':>6} {'drop':>5} {'rate':>5} "
          f"{'drift':>6} {'stall':>5} {'gaps':>4} {'user':>5} {'kern':>5} {'idle':>5}")
    for browser, b in summary['browsers'].items():
        for r in b['cases']:
            cpu = r.get('cpu') or {}
            audio = r.get('audio') or {}
            print(f"{browser:8} {r['case']:10} {r.get('status', ''):9} {r.get('ttff_ms', ''):>6} {r.get('decoded_fps', ''):>6} "
                  f"{r.get('presented_fps', ''):>6} {r.get('dropped_frames', ''):>5} {r.get('clock_rate', ''):>5} "
                  f"{r.get('drift_ms_mean', ''):>6} {r.get('stall_seconds', ''):>5} {audio.get('gaps', ''):>4} "
                  f"{cpu.get('user', ''):>5} {cpu.get('kernel', ''):>5} {cpu.get('idle', ''):>5}")
    print(out / 'video-results.json')
    if failures:
        for f in failures:
            print('FAIL', f)
        raise SystemExit(1)


if __name__ == '__main__':
    main()
