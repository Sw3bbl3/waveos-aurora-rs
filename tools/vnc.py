#!/usr/bin/env python3
"""Look at what a VM's display sends a VNC client: the hardware cursor
image, and the screen size as the host sees it; or resize the "window"
the way a VNC viewer does (which a virtio-gpu guest sees as its host
window changing size).

Start the VM with a VNC server on a Unix socket, e.g.
  AURORA_QEMU_ARGS="-vnc unix:$PWD/target/vnc.sock" cargo xtask run --gpu virtio --headless ...

Usage: tools/vnc.py SOCKET STEP [STEP...]
  cursor:OUT.png   wait for the cursor shape (Cursor pseudo-encoding), save it, print size and hot spot
  size             print the framebuffer size
  resize:W,H       ask for a new desktop size (SetDesktopSize); print the size that comes back
  shot:OUT.png     save the framebuffer (raw encoding)
"""
import socket, struct, sys, time, zlib

CURSOR, DESKTOP_SIZE, EXT_DESKTOP_SIZE, RAW = -239, -223, -308, 0


def png(path, w, h, rgba):
    rows = b"".join(b"\0" + rgba[y * w * 4:(y + 1) * w * 4] for y in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    data += chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
    open(path, "wb").write(data)


class Vnc:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.settimeout(30)
        self.s.connect(path)
        self.recv(12)
        self.s.sendall(b"RFB 003.008\n")
        n = self.recv(1)[0]
        types = self.recv(n)
        assert 1 in types, "VNC server wants a password"
        self.s.sendall(b"\x01")
        assert struct.unpack(">I", self.recv(4))[0] == 0, "security handshake failed"
        self.s.sendall(b"\x01")  # shared
        self.w, self.h = struct.unpack(">HH", self.recv(4))
        self.recv(16)
        self.recv(struct.unpack(">I", self.recv(4))[0])
        # 32-bit little-endian true colour: B, G, R, X in memory.
        self.s.sendall(struct.pack(">B3xBBBBHHHBBB3x", 0, 32, 24, 0, 1, 255, 255, 255, 16, 8, 0))
        encs = [RAW, CURSOR, DESKTOP_SIZE, EXT_DESKTOP_SIZE]
        self.s.sendall(struct.pack(">BxH", 2, len(encs)) + b"".join(struct.pack(">i", e) for e in encs))
        self.cursor = None
        self.pixels = None

    def recv(self, n):
        out = b""
        while len(out) < n:
            d = self.s.recv(n - len(out))
            if not d:
                raise EOFError
            out += d
        return out

    def request(self, incremental):
        self.s.sendall(struct.pack(">BBHHHH", 3, incremental, 0, 0, self.w, self.h))

    def update(self):
        """Reads one server message; handles framebuffer updates."""
        t = self.recv(1)[0]
        if t == 2:  # bell
            return
        if t == 1:  # colour map
            self.recv(1)
            _, n = struct.unpack(">HH", self.recv(4))
            self.recv(n * 6)
            return
        if t == 3:  # cut text
            self.recv(3)
            self.recv(struct.unpack(">I", self.recv(4))[0])
            return
        assert t == 0, f"unexpected message {t}"
        self.recv(1)
        (n,) = struct.unpack(">H", self.recv(2))
        for _ in range(n):
            x, y, w, h, enc = struct.unpack(">HHHHi", self.recv(12))
            if enc == RAW:
                data = self.recv(w * h * 4)
                if self.pixels is None or len(self.pixels) != self.w * self.h * 4:
                    self.pixels = bytearray(self.w * self.h * 4)
                for row in range(h):
                    o = ((y + row) * self.w + x) * 4
                    self.pixels[o:o + w * 4] = data[row * w * 4:(row + 1) * w * 4]
            elif enc == CURSOR:
                data = self.recv(w * h * 4)
                mask = self.recv((w + 7) // 8 * h)
                rgba = bytearray()
                for i in range(w * h):
                    b, g, r = data[i * 4], data[i * 4 + 1], data[i * 4 + 2]
                    on = mask[(i // w) * ((w + 7) // 8) + (i % w) // 8] & (0x80 >> (i % w % 8))
                    rgba += bytes([r, g, b, 255 if on else 0])
                self.cursor = (w, h, x, y, bytes(rgba))
            elif enc in (DESKTOP_SIZE, EXT_DESKTOP_SIZE):
                self.w, self.h = w, h
                if enc == EXT_DESKTOP_SIZE:
                    screens = self.recv(4)[0]
                    self.recv(16 * screens)
                self.pixels = None
            else:
                raise RuntimeError(f"unexpected encoding {enc}")

    def until(self, done, seconds=20):
        end = time.time() + seconds
        self.request(0)
        while time.time() < end:
            self.update()
            if done():
                return True
            self.request(1)
        return False


v = Vnc(sys.argv[1])
for step in sys.argv[2:]:
    k, _, arg = step.partition(":")
    if k == "size":
        print(f"size {v.w}x{v.h}")
    elif k == "cursor":
        v.cursor = None
        if not v.until(lambda: v.cursor is not None):
            sys.exit("no cursor shape arrived")
        w, h, hx, hy, rgba = v.cursor
        png(arg, w, h, rgba)
        opaque = sum(1 for i in range(3, len(rgba), 4) if rgba[i])
        print(f"cursor {w}x{h}, hot spot ({hx}, {hy}), {opaque} opaque pixels -> {arg}")
    elif k == "resize":
        w, h = map(int, arg.split(","))
        v.until(lambda: True, 5)
        v.s.sendall(struct.pack(">BxHHBx", 251, w, h, 1) + struct.pack(">IHHHHI", 0, 0, 0, w, h, 0))
        ok = v.until(lambda: (v.w, v.h) == (w, h), 60)
        print(f"size {v.w}x{v.h}" + ("" if ok else f" (asked for {w}x{h})"))
    elif k == "shot":
        v.until(lambda: v.pixels is not None, 30)
        px = v.pixels
        rgba = bytearray(len(px))
        rgba[0::4], rgba[1::4], rgba[2::4], rgba[3::4] = px[2::4], px[1::4], px[0::4], b"\xff" * (len(px) // 4)
        png(arg, v.w, v.h, bytes(rgba))
        print(f"saved {v.w}x{v.h} -> {arg}")
