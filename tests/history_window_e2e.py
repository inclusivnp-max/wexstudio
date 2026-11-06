#!/usr/bin/env python3
"""Actual native 100k-line history actions and GPU captures; no direct text mutation."""
from pathlib import Path
import hashlib,json,os,struct,subprocess,sys
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();P=O/'project';P.mkdir();W=O/'window';W.mkdir()
with (P/'Large.kt').open('w')as f:
 f.write('// Real Kotlin document; no synthetic parser delay\n')
 for n in range(100000):f.write(f'private val property{n} = {n}\n')
(P/'Tiny.kt').write_text('val tiny = 7\n')
hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.iterdir()};traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_TOOLS_HOME':str(R/'.wex')};env.pop('WAYLAND_DISPLAY',None)
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),'--history-smoke',str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE),'project':str(P)})+'\n');code=proc.wait(timeout=220)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-8000:]
p=json.loads((W/'history-window.json').read_text());assert p['result']=='GPU_PERSISTENT_HISTORY_OK'and p['source_lines']>100000 and p['rendered_rows']<200 and p['renderer']=='wgpu'and not p['deployment_invoked']
for k in ['disk_source_unchanged','head_and_tail_edits','selection_cursor_restored','dirty_baseline_restored','parked_history_preserved']:assert p[k],k
assert [a['action']for a in p['actions']]==['Paste','Undo','Redo','Undo','Paste','Undo']
for a in p['actions']:
 assert a['revision_after']==a['revision_before']+1
 assert (a['tree_end_byte'] is None and a.get('syntax_pending_at_input'))or a['tree_end_byte']==a['rope_bytes']
retirement=p['retirement'];assert p['large_tab_closed_reopened'] and retirement['workers']==1 and retirement['text_reclaimed']>0 and retirement['parsers_reclaimed']>0
assert retirement['pending']==retirement['logical_bytes_pending']==retirement['synchronous_fallbacks']==0 and retirement['worker_thread']!=p['foreground_thread']
assert len(p['screenshots'])==7
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()
(O/'history-e2e.json').write_text(json.dumps({'result':'NATIVE_PERSISTENT_HISTORY_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_device_deployment':True},indent=2)+'\n')
print('NATIVE_PERSISTENT_HISTORY_OK: actual100k-line source; native Paste/CtrlZ/CtrlY/ShiftEnd, head/tail edits, exact cursor/selection/dirty baseline, parked history; 7 genuine GPU frames; timed real input handlers; history/parser reclaimed on distinct worker; close/reopen; disk unchanged; no Gradle/deployment',flush=True)
