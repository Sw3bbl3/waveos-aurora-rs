#!/usr/bin/env python3
"""Build offline runtime photographs and thumbnails from licensed source masters.
Requires Pillow. Never upscales, recolors, generates, or downloads an image.
"""
import hashlib
import json
from pathlib import Path
from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / 'assets/wallpaper-sources'

def main():
    records = json.loads((SOURCES / 'manifest.json').read_text())
    for entry in records:
        path = SOURCES / (entry['id'] + '.jpg')
        image = ImageOps.exif_transpose(Image.open(path)).convert('RGB')
        assert image.width >= 3840 and image.height >= 2400, f"Small master: {path}"
        # A 4096×2560 master also covers 3840×2160 with a centered crop.
        scale = min(image.width / 4096, image.height / 2560)
        assert scale >= 1, f"Master needs upscaling: {path}"
        runtime = ImageOps.fit(image, (4096, 2560), method=Image.Resampling.LANCZOS)
        for directory, size, quality in [('wallpapers',(4096,2560),88),('wallpaper-thumbs',(480,300),85)]:
            out = ROOT / 'assets' / directory / (entry['id'] + '.jpg')
            out.parent.mkdir(exist_ok=True)
            picture = runtime if size == runtime.size else runtime.resize(size, Image.Resampling.LANCZOS)
            picture.save(out, 'JPEG', quality=quality, optimize=True, progressive=False)
        entry['sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
        print(f"{entry['name']}: {image.width}×{image.height} → 4096×2560")
    (SOURCES/'manifest.json').write_text(json.dumps(records,indent=2)+'\n')
    credits = ['# WAVEOS photographic wallpapers', '',
        'These real photographs are bundled under the Unsplash License. Their individual source pages were checked for free use (not Unsplash+) on 31 October 2026.', '',
        'License: https://unsplash.com/license', '',
        'The license permits downloading, copying, modifying, distributing, and commercial use. It excludes selling unmodified images and compiling a competing image service.', '',
        'Source masters retain the native pixel dimensions and are downloaded as JPEG at quality 95. Runtime derivatives use a centered 16:10 crop and JPEG compression; thumbnails are resized from those crops. No recoloring or generated imagery is used. Both appearances display the same photograph.', '',
        '| Wallpaper | Photographer | Source | Native dimensions |',
        '| --- | --- | --- | --- |']
    for e in records:
        credits.append(f"| {e['name']} | {e['photographer']} | [{e['id']}]({e['page']}) | {e['width']} × {e['height']} |")
    credits += ['', 'Exact download URLs and source-file hashes are recorded in `wallpaper-sources/manifest.json`.', '']
    (ROOT/'assets/WALLPAPER-CREDITS.md').write_text('\n'.join(credits))

if __name__ == '__main__':
    main()
