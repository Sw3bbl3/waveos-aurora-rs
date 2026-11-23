#!/usr/bin/env python3
"""Read a disposable Firefox via its explicitly enabled localhost BiDi port.

Start Firefox with --remote-debugging-port 9222 and forward that guest port
to localhost with QMP hostfwd_add. This tool never enables remote control in
normal browser launches. Uses only Python's standard library.
"""
import argparse
import base64
import hashlib
import json
import os
import socket
import struct
import time


class Firefox:
    def __init__(self, port=9222, timeout=45):
        self.socket = socket.create_connection(('127.0.0.1', port), timeout)
        self.stream = self.socket.makefile('rb')
        key = base64.b64encode(os.urandom(16)).decode()
        request = (f'GET /session HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n'
                   f'Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n'
                   'Sec-WebSocket-Version: 13\r\n\r\n')
        self.socket.sendall(request.encode())
        status = self.stream.readline().decode().strip()
        headers = {}
        while True:
            line = self.stream.readline().decode().strip()
            if not line:
                break
            k, v = line.split(':', 1)
            headers[k.lower()] = v.strip()
        expected = base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        if ' 101 ' not in status or headers.get('sec-websocket-accept') != expected:
            raise RuntimeError(f'WebSocket handshake failed: {status}')
        self.sequence = 0
        self.command('session.new', {'capabilities': {}})

    def send(self, data, opcode=1):
        mask = os.urandom(4)
        size = len(data)
        header = bytes([0x80 | opcode])
        header += bytes([0x80 | size]) if size < 126 else bytes([0x80 | 126])+struct.pack('!H', size) if size < 65536 else bytes([0x80 | 127])+struct.pack('!Q', size)
        self.socket.sendall(header+mask+bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def read(self, count):
        result = bytearray()
        while len(result) < count:
            chunk = self.stream.read(count-len(result))
            if not chunk:
                raise EOFError('Firefox closed the probe connection')
            result.extend(chunk)
        return bytes(result)

    def receive(self):
        message = bytearray()
        while True:
            a, b = self.read(2)
            size = b & 127
            if size == 126:
                size = struct.unpack('!H', self.read(2))[0]
            elif size == 127:
                size = struct.unpack('!Q', self.read(8))[0]
            if size > 8*1024*1024:
                raise RuntimeError('Unexpectedly large probe response')
            mask = self.read(4) if b & 128 else None
            data = self.read(size)
            if mask:
                data = bytes(x ^ mask[i % 4] for i, x in enumerate(data))
            if a & 15 == 8:
                raise EOFError('Firefox closed the probe connection')
            if a & 15 == 9:
                self.send(data, 10)
                continue
            if a & 15 in (0, 1):
                message.extend(data)
                if a & 128:
                    return json.loads(message)

    def command(self, method, params):
        self.sequence += 1
        self.send(json.dumps({'id': self.sequence, 'method': method, 'params': params}).encode())
        while True:
            response = self.receive()
            if response.get('id') == self.sequence:
                if response.get('type') == 'error':
                    raise RuntimeError(response)
                return response['result']

    def context(self):
        return self.command('browsingContext.getTree', {})['contexts'][0]['context']

    def evaluate(self, expression, context=None):
        response = self.command('script.evaluate', {'expression': expression,
            'target': {'context': context or self.context()}, 'awaitPromise': True})
        if response.get('type') == 'exception':
            raise RuntimeError(response)
        value = response['result']
        return value.get('value', value)

    def close(self):
        try:
            self.socket.settimeout(3)
            self.command('session.end', {})
        except (OSError, EOFError, RuntimeError):
            pass
        finally:
            self.stream.close()
            self.socket.close()


STATE = '''JSON.stringify({url:location.href,title:document.title,ready:document.readyState,
    nodes:document.querySelectorAll('*').length,ua:navigator.userAgent,
    codecs:document.querySelector('#movie_player')?.getStatsForNerds?.().codecs,
    video:[...document.querySelectorAll('video')].map(v=>({paused:v.paused,time:v.currentTime,
    width:v.videoWidth,height:v.videoHeight,ready:v.readyState,error:v.error?.message,
    quality:(q=>({total:q.totalVideoFrames,dropped:q.droppedVideoFrames}))(v.getVideoPlaybackQuality()),src:v.currentSrc})),
    avc:MediaSource.isTypeSupported('video/mp4; codecs="avc1.640028"'),
    vp9:MediaSource.isTypeSupported('video/webm; codecs="vp9"'),
    av1:MediaSource.isTypeSupported('video/mp4; codecs="av01.0.08M.08"')})'''


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=9222)
    parser.add_argument('--expression', default=STATE)
    parser.add_argument('--url')
    args = parser.parse_args()
    browser = Firefox(args.port)
    try:
        if args.url:
            browser.command('browsingContext.navigate', {'context': browser.context(), 'url': args.url, 'wait': 'none'})
        started = time.monotonic()
        result = browser.evaluate(args.expression)
        try:
            result = json.loads(result)
        except (TypeError, json.JSONDecodeError):
            pass
        print(json.dumps({'command_seconds': time.monotonic()-started, 'result': result}, indent=2))
    finally:
        browser.close()
