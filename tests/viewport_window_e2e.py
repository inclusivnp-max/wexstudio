#!/usr/bin/env python3
"""Native half-million-character Unicode row: CPU samples, mesh counts and real GPU captures."""
from pathlib import Path
import hashlib,json,os,struct,subprocess,sys
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';P.mkdir();W=O/'window';W.mkdir();IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve()
n=int(os.environ.get('WEX_VIEWPORT_LINE_CHARS','500000'))
body=('// Actual long Kotlin line, native viewport profile '+('0123456789 Ж界👋e\u0301\t '*(n//10+100)))[:n]+' Ж界👋e\u0301 TAILEND\n';(P/'Large.kt').write_text(body);(P/'Tiny.kt').write_text('val tiny = 7\n');hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.iterdir()}
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_TOOLS_HOME':str(R/'.wex')};env.pop('WAYLAND_DISPLAY',None)
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),os.environ.get('WEX_VIEWPORT_FLAG','--viewport-smoke'),str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE),'project':str(P)})+'\n');code=proc.wait(timeout=300)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-8000:]
p=json.loads((W/'viewport-window.json').read_text());assert p['result']=='GPU_VIEWPORT_PROFILE_OK'and p['renderer']=='wgpu'and p['long_line_chars']>=500000 and not p['deployment_invoked']and p['rendered_rows']<100
for flag in ['native_mouse_tail_exact','ime_preedit_immutable','ime_commit_single_undo','parked_view_preserved','disk_source_unchanged']:assert p[flag],flag
assert len(p['warm_samples'])==6 and len(p['screenshots'])==({'--layout-smoke':13,'--gesture-smoke':15}.get(os.environ.get('WEX_VIEWPORT_FLAG'),12))
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])and s['native_capture_roundtrip_us']>0
for s in p['warm_samples']:assert s['source_vertices']>1000000 and s['submitted_vertices']>0 and s['editor_us']>0 and s['update_us']>=s['editor_us']
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()
(O/'viewport-e2e.json').write_text(json.dumps({'result':'NATIVE_VIEWPORT_PROFILE_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'measurement':'completed native update/editor CPU and request-to-GPU-screenshot event roundtrip, NOT compositor presentation/VSYNC latency','no_gradle_or_device_deployment':True},indent=2)+'\n')
print(f"NATIVE_VIEWPORT_PROFILE_OK: actual{p['long_line_chars']}-character Unicode row;native head/tail/mouse/selection/Paste/Undo/IME/tabs;6 CPU samples+{len(p['screenshots'])} genuine GPU capture roundtrips;protected bytes unchanged;no Gradle/deployment",flush=True)
