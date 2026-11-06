#!/usr/bin/env python3
"""Native Save while an independent process holds actual cooperative OS locks."""
from pathlib import Path
import hashlib,json,os,struct,subprocess,sys
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();P=O/'project';P.mkdir();W=O/'window';W.mkdir()
with(P/'Large.kt').open('w')as f:
 f.write('// Real Kotlin document; no synthetic parser delay\n')
 for n in range(100000):f.write(f'private val property{n} = {n}\n')
(P/'Tiny.kt').write_text('val tiny=7\n');hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.iterdir()};traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
# A SHA256 path key is separate from exact BLAKE3 document content hashes.
lock_paths={name:str(P/'.wex/editor-saves/locks'/(hashlib.sha256(os.fsencode(P/name)).hexdigest()+'.lock'))for name in ['Large.kt','Tiny.kt']}
lock_code=r'''
from pathlib import Path
import fcntl,json,os,sys,time
w=Path(sys.argv[1]);paths=json.loads(sys.argv[2]);deadline=time.monotonic()+260

def wait(name):
 while not(w/name).exists():
  assert time.monotonic()<deadline,name
  time.sleep(.02)

def acquire(name):
 p=Path(paths[name]);p.parent.mkdir(parents=True,exist_ok=True);f=p.open('a+');fcntl.flock(f,fcntl.LOCK_EX);return f
large=acquire('Large.kt');tiny=acquire('Tiny.kt');(w/'locks-ready').write_text('real flock held\n');wait('release-large-first');large.close();wait('hold-large-second');large=acquire('Large.kt');(w/'large-second-held').write_text('real flock held again\n');wait('write-external');root=Path(paths['Large.kt']).parents[3];source=root/'Large.kt';tmp=root/'external.tmp';tmp.write_text('// external disk revision\nval external=7\n');os.replace(tmp,source);(w/'external-ready').write_text('actual independent file replacement\n');wait('release-large-second');large.close();wait('release-tiny');tiny.close();(w/'tiny-released').write_text('real flock released\n');print('SAVE_LOCK_PROCESS_OK: two real flock cycles + independent atomic external source replacement',flush=True)
'''
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_TOOLS_HOME':str(R/'.wex')};env.pop('WAYLAND_DISPLAY',None)
with(O/'lock-holder.log').open('w')as log:
 locker=subprocess.Popen([sys.executable,'-c',lock_code,str(W),json.dumps(lock_paths)],stdout=log,stderr=subprocess.STDOUT)
 import time
 end=time.monotonic()+10
 while not(W/'locks-ready').exists():assert locker.poll()is None and time.monotonic()<end;time.sleep(.02)
 with(O/'window.log').open('w')as out:
  proc=subprocess.Popen([str(IDE),str(P),'--save-smoke',str(W)],env=env,stdout=out,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'lock_holder_pid':locker.pid,'binary':str(IDE)})+'\n');code=proc.wait(timeout=260)
 if code!=0 and locker.poll()is None:locker.terminate()
 lock_code_exit=locker.wait(timeout=20)
(O/'window.exit').write_text(str(code)+'\n');(O/'lock-holder.exit').write_text(str(lock_code_exit)+'\n');assert code==lock_code_exit==0,(O/'window.log').read_text()[-8000:]+(O/'lock-holder.log').read_text()[-1000:]
p=json.loads((W/'save-window.json').read_text());assert p['result']=='GPU_ASYNC_SAVE_OK'and p['source_lines']>100000 and not p['build_task_started']and not p['run_task_started']
for k in ['typed_while_real_os_lock_waiting','saved_snapshot_kept_newer_edits','undo_redo_baseline_preserved','parked_baselines_merged','external_write_rejected','suppressed_watcher_event_replayed','explicit_keep_mine_retry','partial_stop_acknowledged_committed_files','original_bytes_restored']:assert p[k],k
assert len(p['screenshots'])==10
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()
(O/'save-e2e.json').write_text(json.dumps({'result':'NATIVE_ASYNC_SAVE_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'lock_paths':lock_paths,'proof':p,'no_gradle_or_device_deployment':True},indent=2)+'\n');print('NATIVE_ASYNC_SAVE_OK: real100k source/independent OS flock holder; editable pending snapshot/newer edits/Undo/Redo/parked baselines; actual external write rejected+watcher replay+Keep mine; partial Stop acknowledges committed file and suppresses Build; original bytes restored; 10 genuine GPU frames; no Gradle/deployment',flush=True)
