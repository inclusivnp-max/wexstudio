#!/usr/bin/env python3
"""Real M3 project replacement/preview/Cancel/shared Undo/Redo on the native GPU window."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,struct,subprocess
A=argparse.ArgumentParser();A.add_argument('output');A.add_argument('--serial',required=True,choices=['emulator-5580']);a=A.parse_args();R=Path(__file__).resolve().parents[1];O=Path(a.output).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir();shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
base=P/'app/src/main/kotlin/dev/wex/android';MARK='WEX_REPLACE_検索';(base/'ReplacePark.kt').write_text('package dev.wex.android\nfun replacePark(): Int = 7\n');(base/'ReplaceDisk.txt').write_text('😀 '+MARK+' CLOSED\n');(base/'ReplaceStale.txt').write_text(MARK+' STALE\n')
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/n;f.write_text('#!/bin/sh\nprintf '+n+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
devices=json.loads(subprocess.check_output([str(CLI),'devices'],env=env));assert any(d['serial']==a.serial and d['state']=='device'for d in devices);(O/'devices-before.json').write_text(json.dumps(devices,indent=2)+'\n')
x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,capture_output=True,text=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);(O/'resolve.exit').write_text(str(x.returncode)+'\n');assert x.returncode==0,x.stderr
hashes={str(f):hashlib.sha256(f.read_bytes()).hexdigest()for f in P.rglob('*')if f.is_file()and'.wex'not in f.relative_to(P).parts}
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),'--project-replace-smoke',str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE),'project':str(P)})+'\n')
 try:code=proc.wait(timeout=240)
 finally:
  if proc.poll()is None:assert str(P).encode()in Path('/proc',str(proc.pid),'cmdline').read_bytes();proc.terminate();proc.wait(timeout=15)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-10000:]
p=json.loads((W/'project-replace-window.json').read_text());assert p['result']=='GPU_PROJECT_REPLACE_OK'and p['renderer']=='wgpu'and len(p['screenshots'])==9
for k in ['source_buffers_restored','dirty_active_and_parked_reviewed','closed_files_unsaved_not_written','shared_keyboard_undo_redo','local_history_preserved','compact_modal']:assert p[k],k
assert(p['applied'],p['undone'],p['redone'],p['rejected'],p['cancelled'])==(1,2,1,1,3)
assert p['actual_pointer_clicks']>=15 and not p['save_worker_started']and not p['deployment_invoked']
assert len(p['reviews'])==2 and all(r['edits']==4 and len(r['files'])==4 and sum(f['dirty']for f in r['files'])==2 and r['worker']!=p['foreground_thread']for r in p['reviews'])
assert len(p['external_writes'])==2 and all(w['result']=='REPLACE_EXTERNAL_WRITE_OK'and w['exit']==0 for w in p['external_writes'])
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()and not(P/'.wex/native').exists()
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
(O/'project-replace-e2e.json').write_text(json.dumps({'result':'NATIVE_PROJECT_REPLACE_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_apk_or_phone':True,'mtvm_serial':a.serial},indent=2)+'\n')
print('NATIVE_PROJECT_REPLACE_OK: actual M3 replacement/preview/Cancel/external conflict/atomic dirty active+parked+closed buffers/shared keyboard Undo+Redo/local history+disk restoration/compact modal;9 GPU frames;no Gradle/APK/phone;mtvm='+a.serial,flush=True)
