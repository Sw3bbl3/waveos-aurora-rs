#!/usr/bin/env python3
"""Runs in the guest during `cargo xtask video-check`: forwards
127.0.0.1:PORT to the host's fixture (10.0.2.2:PORT), so the test page is a
secure context (http://127.0.0.1 is one; http://10.0.2.2 isn't) and gets
WebCodecs, which browsers offer only to secure contexts."""
import socket
import sys
import threading

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18766


def pipe(a, b):
    try:
        while True:
            data = a.recv(262144)
            if not data:
                break
            b.sendall(data)
    except OSError:
        pass
    for s in (a, b):
        try:
            s.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(('127.0.0.1', PORT))
server.listen(32)
while True:
    client, _ = server.accept()
    try:
        upstream = socket.create_connection(('10.0.2.2', PORT))
    except OSError:
        client.close()
        continue
    threading.Thread(target=pipe, args=(client, upstream), daemon=True).start()
    threading.Thread(target=pipe, args=(upstream, client), daemon=True).start()
