#!/usr/bin/env python3
"""Window-open smoke checks; screenshots and serial logs still require review."""
import pathlib, subprocess, sys
root=pathlib.Path(__file__).resolve().parent.parent
out=pathlib.Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=True)
for name in ['Files','Nebula','Notes','Calculator','Settings','Preview','Paint','Clock','Activity Monitor','Constellation Studio','GINA Apps','About']:
    subprocess.run([sys.executable,str(root/'tools/qmp.py'),'key:ctrl+spc','type:'+name,'key:ret','sleep:1.5','shot:'+str(out/(name.replace(' ','-')+'.png')),'key:ctrl+w','sleep:0.8'],check=True)
