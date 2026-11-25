#!/usr/bin/env python3
"""Local video fixture for the guest browsers: test clips and an MSE player
page that measures playback and posts the results back.

    python3 tools/video_fixture.py --directory DIR --prepare   # make the clips (needs ffmpeg)
    python3 tools/video_fixture.py --directory DIR [--port N] [--results FILE]  # serve DIR

The clips are made, not downloaded: a moving test pattern with a little
noise (so the bit rates are like YouTube's at each size), 30 frames a
second, a key frame every two seconds, and a continuous 440 Hz tone (gaps in
it are easy to find in a recording of the sound card). Each codec is in the
container YouTube serves it in, video and sound in separate files, as Media
Source Extensions players fetch them: H.264 and AAC in fragmented MP4, VP9
and Opus in WebM, AV1 in fragmented MP4.

The page (`tools/video_mse.html`, served as /mse.html) plays the cases one
after another in one document and POSTs JSON to /result; the server appends
each to FILE (default DIR/results.jsonl).
"""
import argparse
import http.server
import json
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SECONDS = 30
# Bit rates near YouTube's for 30 frames a second (kbit/s).
RATES = {
    'h264': {360: 700, 720: 2500, 1080: 4500},
    'vp9': {360: 450, 720: 1500, 1080: 2800},
    'av1': {360: 350, 720: 1100, 1080: 2000},
}
SIZES = {360: '640x360', 720: '1280x720', 1080: '1920x1080'}
LEVEL = {360: '3.0', 720: '3.1', 1080: '4.0'}


def clips():
    """(file name, ffmpeg arguments after the input) for every clip."""
    out = []
    for res, size in SIZES.items():
        source = f'testsrc2=size={size}:rate=30,noise=alls=6:allf=t'
        common = ['-f', 'lavfi', '-i', source, '-t', str(SECONDS), '-an', '-pix_fmt', 'yuv420p', '-g', '60']
        rate = lambda codec: [f'-b:v', f"{RATES[codec][res]}k", '-maxrate', f"{RATES[codec][res] * 3 // 2}k",
                              '-bufsize', f"{RATES[codec][res] * 2}k"]
        frag = ['-movflags', '+frag_keyframe+empty_moov+default_base_moof', '-f', 'mp4']
        out.append((f'h264-{res}.mp4', common + ['-c:v', 'libx264', '-preset', 'medium', '-profile:v', 'high',
                                                  '-level', LEVEL[res]] + rate('h264') + frag))
        out.append((f'vp9-{res}.webm', common + ['-c:v', 'libvpx-vp9', '-deadline', 'good', '-cpu-used', '4',
                                                 '-row-mt', '1', '-tile-columns', '2'] + rate('vp9') + ['-f', 'webm']))
        out.append((f'av1-{res}.mp4', common + ['-c:v', 'libsvtav1', '-preset', '8'] + rate('av1')[:2] + frag))
    tone = ['-f', 'lavfi', '-i', f'sine=frequency=440:sample_rate=48000:duration={SECONDS}', '-af',
            'volume=0.25,aformat=channel_layouts=stereo', '-vn']
    out.append(('aac.mp4', tone + ['-c:a', 'aac', '-ar', '44100', '-b:a', '128k',
                                   '-movflags', '+frag_keyframe+empty_moov+default_base_moof', '-frag_duration', '2000000', '-f', 'mp4']))
    out.append(('opus.webm', tone + ['-c:a', 'libopus', '-b:a', '128k', '-f', 'webm']))
    return out


def prepare(directory):
    ffmpeg = shutil.which('ffmpeg')
    if not ffmpeg:
        sys.exit('making the clips needs ffmpeg with libx264, libvpx-vp9, libsvtav1 and libopus')
    directory.mkdir(parents=True, exist_ok=True)
    for name, args in clips():
        path = directory / name
        if path.exists() and path.stat().st_size > 0:
            continue
        print('making', name, flush=True)
        subprocess.run([ffmpeg, '-hide_banner', '-loglevel', 'error', '-y', *args, str(path)], check=True)
    # The same video as bare streams, for the page's WebCodecs decoding
    # benchmark (no demuxer in the way): H.264 in Annex B with access unit
    # delimiters, VP9 and AV1 in IVF.
    for res in SIZES:
        for src, dst, extra in [(f'h264-{res}.mp4', f'h264-{res}.h264', ['-bsf:v', 'h264_mp4toannexb,h264_metadata=aud=insert', '-f', 'h264']),
                                (f'vp9-{res}.webm', f'vp9-{res}.ivf', ['-f', 'ivf']),
                                (f'av1-{res}.mp4', f'av1-{res}.ivf', ['-f', 'ivf'])]:
            if not (directory / dst).exists():
                subprocess.run([ffmpeg, '-hide_banner', '-loglevel', 'error', '-y', '-i', str(directory / src), '-c', 'copy',
                                *extra, str(directory / dst)], check=True)


def serve(directory, port, results):
    lock = threading.Lock()

    class Handler(http.server.SimpleHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def __init__(self, *a, **k):
            super().__init__(*a, directory=str(directory), **k)

        def log_message(self, *_):
            pass

        def end_headers(self):
            self.send_header('Cache-Control', 'no-store')
            super().end_headers()

        def do_GET(self):
            page = {'/mse.html': ('video_mse.html', 'text/html; charset=utf-8'),
                    '/forward.py': ('video_forward.py', 'text/plain')}.get(self.path.split('?')[0])
            if page:
                body = (ROOT / page[0]).read_bytes()
                self.send_response(200)
                self.send_header('Content-Type', page[1])
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            super().do_GET()

        def do_POST(self):
            length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(length)
            try:
                record = json.loads(body)
            except json.JSONDecodeError:
                record = {'kind': 'bad', 'body': body[:200].decode(errors='replace')}
            record['host_time'] = time.time()
            with lock, results.open('a') as f:
                f.write(json.dumps(record) + '\n')
            self.send_response(204)
            self.send_header('Access-Control-Allow-Origin', '*')
            self.send_header('Content-Length', '0')
            self.end_headers()

    http.server.ThreadingHTTPServer.allow_reuse_address = True
    server = http.server.ThreadingHTTPServer(('0.0.0.0', port), Handler)
    print(f'serving {directory} on port {port}', flush=True)
    server.serve_forever()


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--directory', type=Path, required=True)
    p.add_argument('--port', type=int, default=18766)
    p.add_argument('--prepare', action='store_true')
    p.add_argument('--results', type=Path)
    args = p.parse_args()
    if args.prepare:
        prepare(args.directory)
    else:
        serve(args.directory, args.port, args.results or args.directory / 'results.jsonl')


if __name__ == '__main__':
    main()
