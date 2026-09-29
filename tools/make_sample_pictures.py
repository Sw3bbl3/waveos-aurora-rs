#!/usr/bin/env python3
"""Generates the sample pictures in assets/home/Pictures (procedural, no inputs).

Run from the repository root: python3 tools/make_sample_pictures.py
"""
import math, os, struct, zlib

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "assets/home/Pictures")

def png(path, w, h, rows, alpha):
    raw = bytearray()
    for row in rows:
        raw.append(0)
        raw.extend(row)
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6 if alpha else 2, 0, 0, 0))
    data += chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(data)
    print(f"wrote {path} ({len(data) // 1024} KiB)")

def lerp(a, b, t):
    return a + (b - a) * t

def mix(c0, c1, t):
    return tuple(lerp(x, y, t) for x, y in zip(c0, c1))

def clamp8(v):
    return max(0, min(255, int(v)))

def northern_lights(w=1600, h=1000):
    rows = []
    ribbons = [  # (base, amplitude, frequency, phase, lower colour, upper colour, strength)
        (0.52, 0.08, 5.0, 0.3, (80, 255, 170), (120, 90, 255), 1.0),
        (0.40, 0.06, 7.5, 2.1, (70, 230, 210), (170, 80, 240), 0.7),
    ]
    for y in range(h):
        row = bytearray()
        v = y / h
        sky = mix((6, 10, 32), (30, 20, 62), v ** 1.5)
        for x in range(w):
            u = x / w
            c = list(sky)
            # Curtains: a bright lower edge with light rising softly above it.
            for base, amp, freq, ph, low, high, k in ribbons:
                edge = base + amp * math.sin(u * freq + ph) + 0.025 * math.sin(u * freq * 3.1 + ph * 2)
                d = edge - v
                glow = math.exp(d * 45) if d < 0 else math.exp(-d * 5.5)
                streaks = 0.72 + 0.28 * math.sin(u * 260 + 4 * math.sin(u * 23 + ph)) ** 2
                col = mix(low, high, min(1.0, max(0.0, d * 4)))
                for i in range(3):
                    c[i] += col[i] * glow * streaks * k * 0.85
            # Mountains.
            ridge = 0.80 + 0.05 * math.sin(u * 6.3 + 0.4) + 0.025 * math.sin(u * 17.9 + 1) + 0.01 * math.sin(u * 61)
            if v > ridge:
                c = list(mix((22, 24, 44), (8, 9, 18), min(1.0, (v - ridge) * 6)))
            # Stars.
            n = (x * 73856093 ^ y * 19349663) & 0xFFFF
            if n < 5 and v < 0.55:
                b = 120 + (n * 27) % 135
                c = [max(c[0], b), max(c[1], b), max(c[2], b)]
            row.extend(clamp8(ch) for ch in c)
        rows.append(row)
    png(os.path.join(OUT, "Northern Lights.png"), w, h, rows, False)

def dunes(w=1600, h=1000):
    rows = []
    for y in range(h):
        row = bytearray()
        v = y / h
        for x in range(w):
            u = x / w
            c = mix((255, 196, 140), (255, 120, 90), v * 0.8)
            sun = math.hypot(u - 0.72, (v - 0.32) * 1.6)
            if sun < 0.09:
                c = mix((255, 250, 220), c, (sun / 0.09) ** 4)
            for k in range(4):
                crest = 0.55 + k * 0.1 + 0.05 * math.sin(u * (4 + k) + k * 2.3) + 0.02 * math.sin(u * 17 + k)
                if v > crest:
                    depth = min(1.0, (v - crest) * 5)
                    base = [(214, 120, 84), (186, 92, 70), (150, 70, 62), (112, 52, 58)][k]
                    lit = 0.85 + 0.15 * math.sin(u * (4 + k) * 1.3 + k)
                    c = mix(tuple(ch * lit for ch in base), tuple(ch * 0.7 for ch in base), depth * 0.6)
            row.extend(clamp8(ch) for ch in c)
        rows.append(row)
    png(os.path.join(OUT, "Dunes.png"), w, h, rows, False)

def smoothstep(e0, e1, x):
    t = max(0.0, min(1.0, (x - e0) / (e1 - e0)))
    return t * t * (3 - 2 * t)

def aurora_mark(size=512):
    # The wave as a polyline, for anti-aliased distance tests.
    pts = [(0.2 + 0.6 * i / 200, 0.5 - 0.13 * math.sin(i / 200 * 4 * math.pi)) for i in range(201)]
    def dist(u, v):
        best = 1.0
        for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
            if abs(u - x0) > 0.05:
                continue
            dx, dy = x1 - x0, y1 - y0
            t = max(0, min(1, ((u - x0) * dx + (v - y0) * dy) / (dx * dx + dy * dy)))
            best = min(best, math.hypot(u - x0 - t * dx, v - y0 - t * dy))
        return best
    rows = []
    px = 1 / size
    for y in range(size):
        row = bytearray()
        for x in range(size):
            u, v = (x + 0.5) / size, (y + 0.5) / size
            # Rounded square with a diagonal gradient.
            dx = max(abs(u - 0.5) - 0.28, 0)
            dy = max(abs(v - 0.5) - 0.28, 0)
            a = 1 - smoothstep(-px, px, math.hypot(dx, dy) - 0.16)
            c = mix((124, 92, 255), (45, 212, 191), (u + v) / 2)
            c = mix(c, (255, 255, 255), 1 - smoothstep(0.028 - px, 0.028 + px, dist(u, v)))
            row.extend([clamp8(c[0]), clamp8(c[1]), clamp8(c[2]), clamp8(a * 255)])
        rows.append(row)
    png(os.path.join(OUT, "Aurora Logo.png"), size, size, rows, True)

if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    northern_lights()
    dunes()
    aurora_mark()
