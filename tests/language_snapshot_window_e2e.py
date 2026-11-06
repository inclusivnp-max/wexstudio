#!/usr/bin/env python3
"""Actual K2 + GPU Rope publication, large active/dirty parked buffers and immediate F12."""
from pathlib import Path
import hashlib,json,os,shutil,struct,subprocess,sys
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=False)
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();P=O/'project';shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));W=O/'window';W.mkdir()
source=P/'app/src/main/kotlin/dev/wex/android';large=source/'Large.kt';tiny=source/'Tiny.kt'
with large.open('w')as f:
 f.write('val snapshotProof = snapshotTarget()\n')
 for n in range(100000):f.write(f'// actual background snapshot line {n}\n')
with tiny.open('w')as f:
 f.write('fun snapshotTarget() = 7\n')
 for n in range(10000):f.write(f'// parked buffer source line {n}\n')
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_TOOLS_HOME':str(R/'.wex'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve()
with(O/'resolve.json').open('w')as log:
 x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,stdout=log,stderr=subprocess.PIPE,text=True,timeout=180)
(O/'resolve.log').write_text(x.stderr);(O/'resolve.exit').write_text(str(x.returncode)+'\n');assert x.returncode==0,x.stderr
hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.rglob('*')if p.is_file()and'.wex'not in p.relative_to(P).parts}

with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),os.environ.get('WEX_SNAPSHOT_FLAG','--history-smoke'),str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE),'project':str(P)})+'\n');code=proc.wait(timeout=360)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-9000:]
p=json.loads((W/'history-window.json').read_text());assert p['result']=='GPU_PERSISTENT_HISTORY_OK'and p['native_k2']and p['immediate_f12_with_new_unsaved_input']and p['dirty_parked_source_restored']and p['source_lines']>100000 and p['source_bytes']>3500000
assert p['model_status']['phase']=='Applied'and p['diagnostic_snapshot']==[p['current_path'],p['current_version']]and p['current_path']==str(large)and not p['deployment_invoked']
assert p['foreground_capture_us']
m=p['snapshot_metrics'];assert m['materialized_bytes']>=p['source_bytes']and m['materialized']>0 and m['published']>0 and m['active']==0 and m['materializer_thread']!=p['foreground_thread']
d=p['retirement'];assert d['language_reclaimed']>0 and d['synchronous_fallbacks']==d['pending']==d['logical_bytes_pending']==0 and d['worker_thread']!=p['foreground_thread']
assert sum(a['action']=='Paste'for a in p['actions'])==(6 if os.environ.get('WEX_SNAPSHOT_FLAG')=='--input-order-smoke'else 5) and sum(a['action']=='Undo'for a in p['actions'])==(7 if os.environ.get('WEX_SNAPSHOT_FLAG')=='--input-order-smoke'else 6) and sum(a['action']=='Redo'for a in p['actions'])==1
assert len(p['screenshots'])==11
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()and not(P/'.wex/native').exists()
(O/'snapshot-e2e.json').write_text(json.dumps({'result':'NATIVE_K2_ROPE_SNAPSHOT_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_apk_or_deployment':True},indent=2)+'\n')
print('NATIVE_K2_ROPE_SNAPSHOT_OK: actual100k-line active/10k-line dirty parked source; off-foreground exact snapshot materialization; native typing/history/close/reopen; new unsaved Paste + immediate F12 in one input frame resolves real K2 target in parked dirty buffer; both Undo restore originals/current diagnostics;actual GPU captures verified;disk unchanged;no Gradle/APK/deploy',flush=True)
