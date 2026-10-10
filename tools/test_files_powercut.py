#!/usr/bin/env python3
"""Abrupt QEMU-stop/reboot checks of Files' AuroraFS publication protocol.
Usage: test_files_powercut.py DISPOSABLE_CLONED_DISK
Build first. No other WaveOS VM may be running. Logs go to target/everyday.
The test uses exclusive fixture directories; existing user data is untouched.
"""
import pathlib
import socket
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
DISK = pathlib.Path(sys.argv[1]).resolve()
if not DISK.is_file() or DISK == (ROOT / 'target/waveos-aurora.img').resolve():
    raise SystemExit('Supply an existing disposable cloned disk, not the default disk')
probe = socket.socket(socket.AF_UNIX)
try:
    probe.connect(str(ROOT / 'target/qemu-qmp.sock'))
except OSError:
    pass
else:
    raise SystemExit('Stop the existing WaveOS VM before running this test')
finally:
    probe.close()
OUT = ROOT / 'target/everyday/candidate'
OUT.mkdir(parents=True, exist_ok=True)


def q(*steps):
    subprocess.run([sys.executable, str(ROOT / 'tools/qmp.py'), *steps], check=True, stdout=subprocess.DEVNULL)


def wait_text(log, text, process, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        raw = log.read_text(errors='replace')
        if 'FILES POWER CUT FAIL' in raw:
            raise RuntimeError(raw[raw.index('FILES POWER CUT FAIL'):])
        if text in raw:
            return
        if process.poll() is not None:
            raise RuntimeError('VM exited before ' + text)
        time.sleep(.05)
    raise RuntimeError('Timed out waiting for ' + text)


for phase in range(3):
    for action in ['prepare', 'verify']:
        log = OUT / f'powercut-{phase}-{action}.log'
        with log.open('w') as stream:
            process = subprocess.Popen([
                'cargo', 'xtask', 'run', '--no-build', '--headless', '--no-sound',
                '--net', 'none', '--disk-image', str(DISK),
            ], cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT)
            try:
                wait_text(log, 'Aurora desktop ready', process)
                q('sleep:1', 'key:ctrl+w', 'sleep:0.4', 'key:ctrl+spc', 'sleep:0.3',
                  'type:Terminal', 'key:ret', 'sleep:1.5',
                  f'type:/System/Apps/Files.elf --powercut-{action} {phase}', 'key:ret')
                marker = 'READY' if action == 'prepare' else 'PASS'
                wait_text(log, f'FILES POWER CUT {marker} phase={phase}', process)
                if action == 'verify':
                    wait_text(log, "'Files' exited with code 0", process)
                # QMP quit stops the VM without a guest shutdown/sync request.
                q('quit')
                process.wait(timeout=10)
                print(f'phase {phase} {action}: passed ({log})', flush=True)
            finally:
                if process.poll() is None:
                    q('quit')
                    process.wait(timeout=10)
