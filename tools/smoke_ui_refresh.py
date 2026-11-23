#!/usr/bin/env python3
"""Exercise hover chrome and every resize edge in an isolated QEMU session.
Requires Pillow for geometry checks. Start an otherwise empty 1280×800 guest,
then run this script. It starts a temporary tour practice window and records
screenshots; it does not open or write a user document.
"""
import argparse
import subprocess
import sys
import time
from pathlib import Path
from PIL import Image

ROOT=Path(__file__).resolve().parent.parent

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('output',type=Path)
    p.add_argument('--log',type=Path)
    args=p.parse_args();args.output.mkdir(parents=True,exist_ok=True)
    if args.log:
        deadline=time.monotonic()+60
        while not args.log.exists() or 'Aurora desktop ready' not in args.log.read_text(errors='replace'):
            if time.monotonic()>deadline:raise SystemExit('Guest did not reach desktop')
            time.sleep(0.5)
    def q(*steps):
        subprocess.run([sys.executable,str(ROOT/'tools/qmp.py'),*steps],check=True,stdout=subprocess.DEVNULL)
    def shot(name):
        path=(args.output/(name+'.png')).resolve();q('shot:'+str(path));return path
    # WayV > Take a Tour; the menu has four rows before its first separator.
    q('click:25,26','sleep:0.4','click:130,155','sleep:0.5')
    shot('tour-apps')
    q('click:1198,669','sleep:0.4','click:1198,669','sleep:0.5','move:300,112','sleep:0.3')
    shot('hover-controls')
    q('click:1198,669','sleep:0.4')
    # The shell deliberately gives practice a deterministic initial rectangle.
    rect=[36,96,540,324]
    def assert_frame(name):
        q('move:200,50','sleep:0.1')
        image=Image.open(shot(name)).convert('RGB');x,y,w,h=rect
        row=[image.getpixel((xx,y+31))==(32,38,48) for xx in range(1280)]
        runs=[];start=None
        for xx,on in enumerate(row+[False]):
            if on and start is None:start=xx
            if not on and start is not None:
                if xx-start>150:runs.append((start,xx))
                start=None
        assert any(abs(left-x)<=2 and abs(right-(x+w))<=2 for left,right in runs),(name,rect,runs)
        inside=image.getpixel((x+w//2,y+h-12))
        assert inside==(24,29,37),(name,'content bottom',rect,inside)
        if y+h+6<800:
            outside=image.getpixel((x+w//2,y+h+6))
            assert outside!=(24,29,37),(name,'window extends past expected bottom',rect)
    # Every corner, then each edge. The checks verify actual compositor output,
    # so a corner stolen by the hover control strip fails this test.
    cases=[('se',1,1,40,30),('nw',-1,-1,24,18),('ne',1,-1,30,-12),('sw',-1,1,-16,20),
           ('north',0,-1,0,-12),('south',0,1,0,18),('west',-1,0,-16,0),('east',1,0,24,0)]
    for name,horiz,vert,dx,dy in cases:
        x,y,w,h=rect
        px=x+3 if horiz<0 else x+w-4 if horiz>0 else x+w//2
        py=y+3 if vert<0 else y+h-4 if vert>0 else y+h//2
        q(f'move:{px},{py}','sleep:0.15');shot('cursor-'+name)
        q(f'drag:{px},{py},{px+dx},{py+dy}','sleep:0.5')
        if horiz<0:rect[0]+=dx;rect[2]-=dx
        elif horiz>0:rect[2]+=dx
        if vert<0:rect[1]+=dy;rect[3]-=dy
        elif vert>0:rect[3]+=dy
        assert_frame('resize-'+name)
    x,y,w,h=rect
    q(f'drag:{x+w-4},{y+h-4},{x+20},{y+30}','sleep:0.5')
    rect=[x,y,300,224]
    assert_frame('minimum-size')
    q(f'drag:{x+296},{y+220},1278,798','sleep:0.5')
    rect=[x,y,1272-x,792-y]
    assert_frame('screen-bounds')
    # The hover remains inside the window and keyboard focus can reveal it.
    q('key:alt+spc','sleep:0.3');shot('keyboard-controls')
    q('key:esc') # Escape skips the optional tour and removes practice.
    shot('tour-skipped')
    q('sleep:0.7','key:ctrl+spc','type:Files','key:ret','sleep:1.5','drag:1150,742,100,100','sleep:0.7')
    rect=[14,62,580,394] # minimum 580×360 content + reserved 34-point chrome
    assert_frame('native-client-minimum')
    # A click (or small hand jitter) must not restore an arranged window.
    q('key:meta_l+left','sleep:0.6','click:200,79','sleep:0.3')
    rect=[8,62,628,730]
    assert_frame('snapped-title-click')
    q('key:meta_l+up','sleep:0.6','down:300,79','move:302,80','up:302,80','sleep:0.3')
    rect=[8,62,1264,730]
    assert_frame('maximized-title-jitter')
    # A real drag restores the previous floating dimensions under the pointer.
    q('drag:300,79,380,140','sleep:0.5')
    rect=[247,123,580,394]
    assert_frame('drag-restores-window')
    q('key:ctrl+w')
    print('PASS: 4 corners, 4 edges, minimum size, screen bounds, directional cursors captured, hover chrome, keyboard access, skip, snapped/maximized title clicks, restore on drag')

if __name__=='__main__':main()
