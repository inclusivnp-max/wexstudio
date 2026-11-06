#!/usr/bin/env python3
"""Actual native explorer/Cancel/Retry typing while a single worker reads/parses real files."""
from pathlib import Path
import hashlib,json,os,struct,subprocess,sys,time
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();P=O/'project';P.mkdir();W=O/'window';W.mkdir()
(P/'Host.kt').write_text('// Original host\nval host = 1\n');(P/'Tiny.kt').write_text('val tiny=7\n');(P/'Invalid.kt').write_bytes(b'\xff invalid UTF-8\n')
for name,lines in [('Large.kt',500000),('Medium.kt',60000)]:
 with (P/name).open('w')as f:
  f.write('// Real disk file, no synthetic parser delay\n')
  for n in range(lines):f.write(f'private val property{n} = {n}\n')
hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.iterdir()};traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_TOOLS_HOME':str(R/'.wex')};env.pop('WAYLAND_DISPLAY',None)
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),'--file-open-smoke',str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);deadline=time.monotonic()+220
 while not(W/'error-visible').exists():
  assert proc.poll()is None,'window exited before error frame';assert time.monotonic()<deadline,'native error frame missing';time.sleep(.04)
 (P/'Invalid.kt').write_text('// external repair\nval repaired = 7\n');(W/'repair-ready').write_text('External bytes repaired\n');code=proc.wait(timeout=220)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-6000:];p=json.loads((W/'open-window.json').read_text());assert p['result']=='GPU_ASYNC_FILE_OPEN_OK'and p['typed_while_real_parser_active']and p['cancel_acknowledged']and p['latest_wins_acknowledged']and p['dirty_parked_preserved']and p['two_undo_steps_restored']and p['disk_host_unchanged']and p['error_retained_last_good']and p['retry_loaded_actual_repaired_file'];assert p['lines']>60000 and p['rendered_rows']<200 and len(p['screenshots'])==7
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items()if Path(f).name!='Invalid.kt')and(P/'Invalid.kt').read_text()=='// external repair\nval repaired = 7\n'
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
assert not marker.exists();(O/'file-open-e2e.json').write_text(json.dumps({'result':'NATIVE_ASYNC_FILE_OPEN_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_device_deployment':True},indent=2)+'\n');print('NATIVE_ASYNC_FILE_OPEN_OK: actual explorer clicks; active host edited while real500k-line parser ran; Cancel acknowledged; latest click cancels old candidate; dirty cached buffer/2 Undo steps survive; invalid UTF8 last-good/error/external repair/Retry; real60k-line file+CtrlEnd/virtualized; 7 genuine GPU PNGs; no synthetic parse delay, Gradle or deployment',flush=True)
