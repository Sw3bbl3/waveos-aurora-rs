#!/usr/bin/env python3
"""Package a clean, tested 0.7.0 checkout; never reads the personal desktop disk."""
import argparse, gzip, hashlib, json, pathlib, shutil, subprocess, zipfile

root = pathlib.Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser()
parser.add_argument('--ci-run', required=True)
args = parser.parse_args()

def git(*args):
    return subprocess.check_output(['git', *args], cwd=root, text=True).strip()

def digest(path):
    with open(path, 'rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

if git('status', '--porcelain'):
    raise SystemExit('Commit source changes before packaging.')
commit = git('rev-parse', 'HEAD')
run = json.loads(subprocess.check_output(['gh', 'run', 'view', args.ci_run,
    '--json', 'headSha,conclusion,url'], cwd=root, text=True))
if run['headSha'] != commit or run['conclusion'] != 'success':
    raise SystemExit('The exact release commit must have a successful CI run.')
evidence = root / 'target/release-0.7-evidence'
for name in ['release-img-serial.log', 'release-iso-serial.log']:
    if 'Aurora desktop ready' not in (evidence/name).read_text(errors='replace'):
        raise SystemExit(f'Missing successful artifact boot: {name}')
for spec in ['ahci-1-virtio', 'ahci-4-virtio', 'virtio-4-virtio', 'nvme-4-virtio', 'nvme-4-e1000e']:
    if (evidence/f'release-kernel-{spec}.log').read_text().count('test result: ok. 33 passed') != 2:
        raise SystemExit(f'Missing two-boot local validation: {spec}')
out = root / 'target/release-0.7.0'
out.mkdir(exist_ok=True)
base = 'WaveOS-Aurora-0.7.0'
img = out/f'{base}-x86_64.img.gz'
with open(root/'target/waveos-aurora-usb.img', 'rb') as source, open(img, 'wb') as dest:
    with gzip.GzipFile(filename='', fileobj=dest, mode='wb', mtime=0) as compressed:
        shutil.copyfileobj(source, compressed)
iso = out/f'{base}-x86_64.iso'
shutil.copyfile(root/'target/waveos-aurora.iso', iso)
handbook = out/f'{base}-handbook-en-fr-CA.zip'
with zipfile.ZipFile(handbook, 'w', zipfile.ZIP_DEFLATED) as archive:
    for path in sorted((root/'target/help').rglob('*')):
        if path.is_file():
            archive.write(path, pathlib.Path('WaveOS-Handbook-0.7.0')/path.relative_to(root/'target/help'))
artifacts = [img, iso, handbook]
provenance = {
    'version': '0.7.0', 'tag': 'v0.7.0', 'commit': commit, 'ci': run,
    'rustc': subprocess.check_output(['rustc', '-Vv'], cwd=root, text=True),
    'disk_source': 'fresh assets/home sample data; personal disk excluded',
    'uncompressed_img_sha256': digest(root/'target/waveos-aurora-usb.img'),
    'kernel_sha256': digest(root/'target/esp/aurora/kernel.elf'),
    'system_sha256': digest(root/'target/esp/aurora/system.tar'),
    'artifacts': {p.name: {'sha256': digest(p), 'bytes': p.stat().st_size} for p in artifacts},
}
validation = out/f'{base}-validation.zip'
with zipfile.ZipFile(validation, 'w', zipfile.ZIP_DEFLATED) as archive:
    archive.writestr('provenance.json', json.dumps(provenance, indent=2)+'\n')
    report = (root/'docs/releases/0.7.0/VALIDATION.md').read_text()
    for lang in ['en', 'fr-CA']:
        report = report.replace(f'../../handbook/0.7/{lang}/reference-validation.md', f'measurements-{lang}.md')
    archive.writestr('VALIDATION.md', report.replace('(evidence/)', '(measurements/)'))
    for lang in ['en', 'fr-CA']:
        archive.write(root/f'docs/handbook/0.7/{lang}/reference-validation.md', f'measurements-{lang}.md')
    for path in sorted((root/'docs/releases/0.7.0/evidence').glob('*.json')):
        archive.write(path, 'measurements/'+path.name)
    for pattern in ['release-*.log', 'baseline-final-serial.log', 'candidate-final-serial.log',
                    'qa-refined-serial.log', 'artifact-checks.json']:
        for path in sorted(evidence.glob(pattern)):
            archive.write(path, 'logs/'+path.name)
    for pattern in ['baseline-final-*-*.png', 'candidate-final-*-*.png', 'release-*.png',
                    'learn-dark-start-fixed.png', 'learn-screenshot-rendered.png', 'learn-code-final.png',
                    'learn-fr-persisted.png', 'persistence-reboot.png', 'gina-check-final.png',
                    'gina-installed-launch.png', 'notes-snap-left.png', 'notes-drag-restored.png',
                    'switcher-*.png', 'controls-slider-*.png', 'settings-accessibility-keyboard.png']:
        for path in sorted(evidence.glob(pattern)):
            archive.write(path, 'screenshots/'+path.name)
artifacts.append(validation)
(out/'SHA256SUMS').write_text(''.join(f'{digest(p)}  {p.name}\n' for p in artifacts))
print(out)
