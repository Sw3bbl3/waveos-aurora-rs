import sys
out = sys.argv[1]
# name: (top colour, bottom colour, symbol svg drawn in a 256 grid, white strokes)
SW = 'fill="none" stroke="#fff" stroke-width="14" stroke-linecap="round" stroke-linejoin="round"'
FW = 'fill="#fff"'
icons = {
 "files": ("#5aa8ff", "#2f5bff", f'<path d="M64 92a14 14 0 0 1 14-14h34l16 16h50a14 14 0 0 1 14 14v58a14 14 0 0 1-14 14H78a14 14 0 0 1-14-14z" {SW}/><path d="M64 116h128" {SW}/>'),
 "nebula": ("#5b7bff", "#8e00e6", f'<ellipse cx="128" cy="120" rx="70" ry="30" transform="rotate(-24 128 120)" {SW}/><circle cx="128" cy="120" r="22" {FW}/><circle cx="182" cy="82" r="8" {FW}/>'),
 "terminal": ("#3a3f4c", "#12141b", f'<path d="M72 92l34 28-34 28" {SW}/><path d="M124 150h56" {SW}/>'),
 "notes": ("#ffc23d", "#ff8a00", f'<path d="M78 82h100M78 116h100M78 150h62" {SW}/>'),
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
 "shoal": ("#6f6cff", "#4b2bd6", '<g fill="#fff">' + "".join(f'<circle cx="{x}" cy="{y}" r="15"/>' for x, y in [(84,82),(128,82),(172,82),(106,120),(150,120),(84,158),(128,158),(172,158)]) + "</g>"),
 "doom": ("#d0342c", "#3a0a0c", f'<circle cx="128" cy="118" r="50" {SW}/><path d="M128 50v30M128 156v30M60 118h30M166 118h30" {SW}/><circle cx="128" cy="118" r="10" fill="#ffb02e"/>'),
 "trash": ("#59607a", "#2c3142", f'<path d="M80 92h96l-8 84a12 12 0 0 1-12 11h-56a12 12 0 0 1-12-11z" {SW}/><path d="M66 92h124M108 92V76h40v16" {SW}/>'),
}
for name, (top, bottom, sym) in icons.items():
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
<defs>
 <clipPath id="c"><rect width="256" height="256" rx="58"/></clipPath>
 <linearGradient id="bg" x1="0" y1="0" x2="0.35" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>
 <linearGradient id="band" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#000" stop-opacity=".16"/><stop offset="1" stop-color="#000" stop-opacity=".30"/></linearGradient>
 <radialGradient id="hi" cx=".3" cy=".08" r=".75"><stop offset="0" stop-color="#fff" stop-opacity=".42"/><stop offset=".55" stop-color="#fff" stop-opacity="0"/></radialGradient>
 <filter id="glow" x="-10%" y="-50%" width="120%" height="200%"><feGaussianBlur stdDeviation="3"/></filter>
 <filter id="sh" x="-20%" y="-20%" width="140%" height="140%"><feDropShadow dx="0" dy="4" stdDeviation="5" flood-color="#000" flood-opacity=".22"/></filter>
</defs>
<g clip-path="url(#c)">
 <rect width="256" height="256" fill="url(#bg)"/>
 <path d="M0 186c28-14 56-14 84 0s56 14 88 0 56-14 84 0V256H0z" fill="url(#band)"/>
 <path d="M0 186c28-14 56-14 84 0s56 14 88 0 56-14 84 0" fill="none" stroke="#fff" stroke-opacity=".55" stroke-width="5" filter="url(#glow)"/>
 <path d="M0 186c28-14 56-14 84 0s56 14 88 0 56-14 84 0" fill="none" stroke="#fff" stroke-opacity=".7" stroke-width="1.6"/>
 <rect width="256" height="256" fill="url(#hi)"/>
 <g filter="url(#sh)">{sym}</g>
 <rect x="1" y="1" width="254" height="254" rx="57" fill="none" stroke="#fff" stroke-opacity=".22" stroke-width="2"/>
</g>
</svg>'''
    open(f"{out}/{name}.svg", "w").write(svg)
print(len(icons))
