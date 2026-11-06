#!/usr/bin/env python3
"""Real native surface-bound IME tape and PTYs; not an OS IME-engine test."""
from pathlib import Path
import hashlib,json,os,shutil,struct,subprocess,sys,signal
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir();shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
(P/'app/src/main/kotlin/dev/wex/android/ZImeOther.kt').write_text('package dev.wex.android\nfun imeOther(): Int = 17\n')
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/n;f.write_text('#!/bin/sh\nprintf '+n+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,capture_output=True,text=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);(O/'resolve.exit').write_text(str(x.returncode)+'\n');assert x.returncode==0,x.stderr
hashes={str(f):hashlib.sha256(f.read_bytes()).hexdigest()for f in P.rglob('*')if f.is_file()and'.wex'not in f.relative_to(P).parts}
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),'--ime-scope-smoke',str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE),'project':str(P)})+'\n')
 try:code=proc.wait(timeout=360)
 finally:
  if proc.poll()is None:assert str(P).encode()in Path('/proc',str(proc.pid),'cmdline').read_bytes();proc.terminate();proc.wait(timeout=15)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-9000:]
p=json.loads((W/'ime-window.json').read_text());assert p['result']=='GPU_IME_SCOPE_OK'and p['renderer']=='wgpu'and len(p['screenshots'])==11
for k in ['source_and_undo_restored','source_preedit_ephemeral','terminal_preedit_no_write','source_terminal_late_commits_rejected','close_then_source_text_same_frame','fresh_source_commit_and_disabled_text','find_source_handoff','document_epoch_fenced','session_id_fenced','fresh_terminal_commit_and_disabled_enter','window_focus_fenced','all_sessions_reaped']:assert p[k],k
assert not p['save_worker_started']and not p['deployment_invoked']and p['metrics']['rejected']>=6 and p['metrics']['cancelled']>=6 and p['metrics']['resets']>0
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()and not(P/'.wex/native').exists()
for pid in p['terminal_pids']:assert not(Path('/proc')/str(pid)).exists()
assert len(p['terminal_pids'])==2 and p['terminal_pids'][0]!=p['terminal_pids'][1]
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
(O/'ime-e2e.json').write_text(json.dumps({'result':'NATIVE_IME_SCOPE_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_apk_or_phone':True},indent=2)+'\n')
print('NATIVE_IME_SCOPE_OK: actual native editor/Find/PTY surface and document/session/window ownership;late commits rejected/new text kept/fresh IME resumes/post-Disabled input/AltF12 same-frame route/Undo/source restoration;11 genuine GPU frames;not a system IME-engine automation;no Gradle/APK/phone',flush=True)
