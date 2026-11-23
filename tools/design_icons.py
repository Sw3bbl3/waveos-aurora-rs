import sys
from pathlib import Path
out = sys.argv[1]
Path(out).mkdir(parents=True, exist_ok=True)
# name: (top colour, bottom colour, symbol svg drawn in a 256 grid, white strokes)
SW = 'fill="none" stroke="#fff" stroke-width="14" stroke-linecap="round" stroke-linejoin="round"'
FW = 'fill="#fff"'
icons = {
 "files": ("#5aa8ff", "#2f5bff", f'<path d="M64 92a14 14 0 0 1 14-14h34l16 16h50a14 14 0 0 1 14 14v58a14 14 0 0 1-14 14H78a14 14 0 0 1-14-14z" {SW}/><path d="M64 116h128" {SW}/>'),
 "nebula": ("#5b7bff", "#8e00e6", f'<ellipse cx="128" cy="120" rx="70" ry="30" transform="rotate(-24 128 120)" {SW}/><circle cx="128" cy="120" r="22" {FW}/><circle cx="182" cy="82" r="8" {FW}/>'),
 "terminal": ("#3a3f4c", "#12141b", f'<path d="M72 92l34 28-34 28" {SW}/><path d="M124 150h56" {SW}/>'),
 "notes": ("#e9b83f", "#c48a24", f'<rect x="65" y="55" width="126" height="146" rx="20" {SW}/><path d="M90 91h76M90 124h76M90 157h48" {SW}/>'),
 "calculator": ("#ff8a4c", "#f0432b", '<path d="M90 70v40M70 90h40M146 90h40M74 130l32 32M74 162l32-32M146 138h40M146 154h40" fill="none" stroke="#fff" stroke-width="13" stroke-linecap="round"/>'),
 "clock": ("#2b3566", "#0b1022", f'<circle cx="128" cy="118" r="58" {SW}/><path d="M128 118V84M128 118l26 18" {SW}/><circle cx="128" cy="118" r="7" fill="#ff9f0a"/>'),
 "paint": ("#ff5c93", "#ff9e4a", f'<path d="M160 70l26 26-62 62-30 4 4-30z" {SW}/><path d="M146 84l26 26" {SW}/>'),
 "lens": ("#3de0c8", "#0e8f86", f'<rect x="62" y="74" width="132" height="96" rx="18" {SW}/><path d="M70 160l38-38 26 26 18-16 34 28" {SW}/><circle cx="160" cy="104" r="10" {FW}/>'),
 "pulse": ("#34d97a", "#0a8f5c", f'<path d="M58 124h34l16-38 24 76 18-52 12 14h36" {SW}/>'),
 "settings": ("#98a2b8", "#566075", '<g fill="#fff"><circle cx="128" cy="120" r="46"/>' + "".join(f'<rect x="118" y="56" width="20" height="30" rx="7" transform="rotate({a} 128 120)"/>' for a in range(0, 360, 45)) + '</g><circle cx="128" cy="120" r="18" fill="#6f7a90"/>'),
 "learn": ("#7b7dff", "#7c3aed", f'<path d="M128 92c-18-12-40-14-62-10v80c22-4 44-2 62 10 18-12 40-14 62-10V82c-22-4-44-2-62 10z" {SW}/><path d="M128 92v80" {SW}/>'),
 "studio": ("#8b5cf6", "#0891b2", f'<path d="M98 88l-34 32 34 32M158 88l34 32-34 32M140 76l-24 88" {SW}/>'),
 "installer": ("#38bdf8", "#0369c9", f'<path d="M70 98l58-26 58 26v56l-58 26-58-26z" {SW}/><path d="M128 102v48M108 132l20 20 20-20" {SW}/>'),
 "welcome": ("#7e95ff", "#b84bff", f'<path d="M58 132c18-26 38-26 54 0s36 26 54 0 30-22 40-8" {SW}/><path d="M168 66v24M156 78h24" fill="none" stroke="#fff" stroke-width="10" stroke-linecap="round"/>'),
 "shoal": ("#8275de", "#6357bd", '<g fill="#fff">' + "".join(f'<circle cx="{x}" cy="{y}" r="15"/>' for x, y in [(84,82),(128,82),(172,82),(106,120),(150,120),(84,158),(128,158),(172,158)]) + "</g>"),
 "doom": ("#d0342c", "#3a0a0c", f'<circle cx="128" cy="118" r="50" {SW}/><path d="M128 50v30M128 156v30M60 118h30M166 118h30" {SW}/><circle cx="128" cy="118" r="10" fill="#ffb02e"/>'),
 "trash": ("#59607a", "#2c3142", f'<path d="M80 92h96l-8 84a12 12 0 0 1-12 11h-56a12 12 0 0 1-12-11z" {SW}/><path d="M66 92h124M108 92V76h40v16" {SW}/>'),
}
# Reuse the established symbols, quiet the materials, and add only a fine
# waterline. There are no glossy highlights, bloom filters, or symbol shadows.
icons["voyage"] = ("#5a91d9", "#356ab4", f'<circle cx="128" cy="126" r="66" {SW}/><path d="M149 101l-10 38-32 13 10-38z" fill="#fff"/><path d="M128 58v12M128 182v12M60 126h12M184 126h12" {SW}/>')
icons["wvos"] = ("#8d86de", "#5d58aa", f'<path d="M55 114c20-38 40-38 60 0s40 38 62 0 28-26 32-18" {SW}/><path d="M55 148c20-28 40-28 60 0s40 28 62 0" fill="none" stroke="#fff" stroke-opacity=".55" stroke-width="10" stroke-linecap="round"/>')
for name, (top, bottom, sym) in icons.items():
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
<defs>
 <clipPath id="c"><rect x="2" y="2" width="252" height="252" rx="57"/></clipPath>
 <linearGradient id="bg" x1="0" y1="0" x2="0.35" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>
</defs>
<g clip-path="url(#c)">
 <rect width="256" height="256" fill="url(#bg)"/>
 <path d="M0 211c42-10 60-10 94 0s66 10 96 0 44-10 66-2v47H0z" fill="#000" fill-opacity=".055"/>
 <path d="M0 211c42-10 60-10 94 0s66 10 96 0 44-10 66-2" fill="none" stroke="#fff" stroke-opacity=".20" stroke-width="1.5"/>
 <g>{sym}</g>
 <rect x="3" y="3" width="250" height="250" rx="56" fill="none" stroke="#fff" stroke-opacity=".15" stroke-width="2"/>
</g>
</svg>'''
    Path(out, f"{name}.svg").write_text(svg)
print(f"Generated {len(icons)} source icons in {out}")
