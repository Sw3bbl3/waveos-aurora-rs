#!/usr/bin/env python3
"""Local HTTP/HTTPS fixtures for the real guest browser. No external accounts.
The generated test CA stays under the requested output directory.
"""
import argparse
import gzip
import hashlib
import http.server
import json
import ssl
import secrets
import subprocess
import threading
import time
import urllib.parse
from email.parser import BytesParser
from email.policy import default
from pathlib import Path

PAYLOAD = b'WaveOS browser transfer check.\n' * 1024
STYLE = '<style>body{font:18px sans-serif;margin:32px;line-height:1.5}button,input{font:inherit;padding:8px}a{margin-right:20px}section{height:64px;border-bottom:1px solid #ccd}header{position:sticky;top:0;background:white;padding:10px}</style>'

def page(title, body):
    return f'<!doctype html><meta charset=utf-8><title>{title}</title>{STYLE}<h1>{title}</h1>{body}'.encode()

def certificates(directory):
    directory.mkdir(parents=True, exist_ok=True)
    ca, key = directory/'ca.pem', directory/'ca.key'
    # Regenerate short-lived test certificates on every check invocation.
    def openssl(*args):
        subprocess.run(['openssl', *map(str,args)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    openssl('req','-x509','-newkey','rsa:2048','-nodes','-keyout',key,'-out',ca,'-days','2','-subj','/CN=WaveOS local browser test CA','-addext','basicConstraints=critical,CA:TRUE')
    for name, san in [('server','IP:10.0.2.2,DNS:localhost,IP:127.0.0.1'),('wrong','DNS:wrong.test')]:
        request, leafkey, cert = directory/(name+'.csr'), directory/(name+'.key'), directory/(name+'.pem')
        openssl('req','-new','-newkey','rsa:2048','-nodes','-keyout',leafkey,'-out',request,'-subj','/CN=WaveOS browser fixture')
        ext=directory/(name+'.ext');ext.write_text('subjectAltName='+san+'\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n')
        openssl('x509','-req','-in',request,'-CA',ca,'-CAkey',key,'-CAcreateserial','-out',cert,'-days','2','-extfile',ext)
    return ca

def serve(directory, port, tls_port, wrong_port):
    lock = threading.Lock()
    events = directory/'requests.jsonl'
    retry_requests = set()
    # Earlier fixture data must not satisfy this run's persistence checks.
    token = secrets.token_hex(12)
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self, *_): pass
        def record(self, **extra):
            with lock:
                with events.open('a') as out:
                    out.write(json.dumps(dict(time=time.monotonic(), method=self.command, path=self.path, cookie=self.headers.get('Cookie',''), **extra))+'\n')
        def reply(self, body, status=200, headers=None, chunked=False):
            self.send_response(status)
            self.send_header('Content-Type','text/html; charset=utf-8')
            for k,v in (headers or {}).items(): self.send_header(k,v)
            self.send_header('Transfer-Encoding','chunked') if chunked else self.send_header('Content-Length',str(len(body)))
            self.end_headers()
            try:
                if chunked:
                    for i in range(0,len(body),113):
                        part=body[i:i+113];self.wfile.write(f'{len(part):x}\r\n'.encode()+part+b'\r\n')
                    self.wfile.write(b'0\r\n\r\n')
                else: self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError): pass
        def do_GET(self):
            self.record()
            path=urllib.parse.urlsplit(self.path).path
            if path=='/retry':
                with lock:
                    first=self.path not in retry_requests
                    retry_requests.add(self.path)
                if first:
                    self.send_response(200);self.send_header('Content-Length','100000');self.end_headers()
                    self.wfile.write(b'<html>');self.close_connection=True;return
                return self.reply(page('Retry succeeded','<p>The next request completed.</p>'))
            if path=='/redirect': return self.reply(b'',302,{'Location':'/basic'})
            if path in ('/','/basic','/gzip'):
                body=page('WaveOS web check', '''<p>CSS, UTF-8: Français — Québec ✓</p><button id=counter onclick="this.textContent='Clicked';document.title='Counter passed'">Try JavaScript</button>
                <input placeholder="Type here"><p><a href=/login>Sign in</a><a href=/private>Session</a><a href=/upload>Upload</a><a href=/download>Download</a><a href=/large-download>Large download</a><a href=/scroll>Scroll</a><a href=/slow>Slow response</a></p>''')
                if path=='/gzip': return self.reply(gzip.compress(body),headers={'Content-Encoding':'gzip'},chunked=True)
                return self.reply(body)
            if path=='/login': return self.reply(page('Fixture sign-in','<form method=post action=/login><input name=user value=review><input name=password type=password value=wave-test><button>Sign in</button></form>'))
            if path=='/private':
                ok=('wave_session='+token) in self.headers.get('Cookie','')
                return self.reply(page('Signed in' if ok else 'Sign-in required','<p>Session restored.</p>' if ok else '<a href=/login>Sign in</a>'),200 if ok else 401)
            if path=='/upload': return self.reply(page('Upload a fixture','<form method=post enctype=multipart/form-data action=/upload><input type=file name=file><button>Upload</button></form>'))
            if path=='/storage': return self.reply(page('Storage check','<button onclick="localStorage.setItem(\'wavecheck\',\'saved\');document.title=\'Storage saved\'">Save local data</button><script>if(localStorage.getItem("wavecheck")==="saved")document.title="Storage restored";</script>'.replace("'saved'",repr(token)).replace('==="saved"','==="'+token+'"')))
            if path=='/cache':
                if self.headers.get('If-None-Match')=='"wave-check"': return self.reply(b'',304,{'ETag':'"wave-check"'})
                return self.reply(page('Cached page','<p>This response is cacheable.</p>'),headers={'ETag':'"wave-check"','Cache-Control':'max-age=3600'})
            if path=='/slow':
                time.sleep(6); return self.reply(page('Slow page finished','<p>Loading finished.</p>'))
            if path=='/disconnect':
                self.send_response(200);self.send_header('Content-Length','100000');self.end_headers()
                self.wfile.write(b'<html>');self.close_connection=True;return
            if path in ('/download','/large-download'):
                data=PAYLOAD if path=='/download' else PAYLOAD*256
                self.send_response(200);self.send_header('Content-Type','application/octet-stream')
                self.send_header('Content-Disposition','attachment; filename="wave-check.txt"');self.send_header('Content-Length',str(len(data)));self.end_headers()
                try:
                    for i in range(0,len(data),16384):
                        self.wfile.write(data[i:i+16384]);self.wfile.flush()
                        if path=='/large-download': time.sleep(.02)
                except (BrokenPipeError,ConnectionResetError): pass
                return
            if path=='/scroll':
                rows=''.join(f'<section>Row {i}: a responsive page with selectable text.</section>' for i in range(600))
                return self.reply(page('Scroll performance','<header><input placeholder="Type while loading"><button onclick="this.textContent=\'Changed\'">Small update</button></header>'+rows))
            self.reply(page('Missing page','<p>HTTP errors remain ordinary web pages.</p>'),404)
        def do_POST(self):
            n=int(self.headers.get('Content-Length','0'))
            if n>16*1024*1024: return self.reply(b'too large',413)
            body=self.rfile.read(n)
            path=urllib.parse.urlsplit(self.path).path
            self.record(bytes=n,sha256=hashlib.sha256(body).hexdigest())
            if path=='/login': return self.reply(b'',303,{'Location':'/private','Set-Cookie':'wave_session='+token+'; HttpOnly; SameSite=Lax; Path=/; Max-Age=86400'})
            if path=='/upload':
                message=BytesParser(policy=default).parsebytes(
                    ('Content-Type: '+self.headers.get('Content-Type','')+'\r\nMIME-Version: 1.0\r\n\r\n').encode()+body)
                files=[part.get_payload(decode=True) for part in message.walk() if part.get_filename()]
                valid=files==[PAYLOAD]
                self.record(upload_valid=valid)
                return self.reply(page('Upload complete' if valid else 'Upload mismatch',
                    f'<p>Received {n} bytes; downloaded fixture matches: {valid}.</p>'),200 if valid else 400)
            self.reply(page('Missing page',''),404)
    servers=[]
    for number,name in [(port,None),(tls_port,'server'),(wrong_port,'wrong')]:
        server=http.server.ThreadingHTTPServer(('127.0.0.1',number),Handler);server.daemon_threads=True
        if name:
            context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(directory/(name+'.pem'),directory/(name+'.key'))
            server.socket=context.wrap_socket(server.socket,server_side=True)
        threading.Thread(target=server.serve_forever,daemon=True).start();servers.append(server)
    print(json.dumps(dict(http=port,https=tls_port,wrong_certificate=wrong_port,download_sha256=hashlib.sha256(PAYLOAD).hexdigest())),flush=True)
    try:
        while True: time.sleep(1)
    except KeyboardInterrupt: pass
    finally:
        for server in servers: server.shutdown()

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory',type=Path,required=True);parser.add_argument('--prepare',action='store_true')
    parser.add_argument('--port',type=int,default=18765);parser.add_argument('--tls-port',type=int,default=18443);parser.add_argument('--wrong-port',type=int,default=18444)
    args=parser.parse_args()
    if args.prepare: print(certificates(args.directory))
    else: serve(args.directory,args.port,args.tls_port,args.wrong_port)
