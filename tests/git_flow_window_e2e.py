#!/usr/bin/env python3
"""Actual keyboard/buffer-only Save/external refresh/cancellation through native production Git UI."""
from pathlib import Path
from native_window_process import run_window
import argparse,hashlib,json,os,shutil,struct,subprocess
A=argparse.ArgumentParser();A.add_argument('output');mode=A.add_mutually_exclusive_group(required=True);mode.add_argument('--serial',choices=['emulator-5580']);mode.add_argument('--desktop-only',action='store_true',help='Real native Git/editor window only; no Android runtime claim');a=A.parse_args();R=Path(__file__).resolve().parents[1];O=Path(a.output).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir();shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/n;f.write_text('#!/bin/sh\necho '+n+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
for k in list(env):
 if k.startswith('GIT_'):env.pop(k)
def git(*args):return subprocess.check_output(['git','-C',str(P),*args],env=env)
git('init','-q');git('config','user.name','Wex UI Fixture');git('config','user.email','fixture@localhost');git('config','commit.gpgsign','false');git('config','core.hooksPath',str(O/'empty-hooks'))
(P/'.gitignore').write_text('.wex/\n');(P/'Review.txt').write_text('reviewed before\n');(P/'0-new.txt').write_text('initial row\n');main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';park=main.parent/'ZFlowPark.kt';park.write_text('package dev.wex.android\nfun flowPark(): Int = 7\n');main_before=main.read_bytes();park_before=park.read_bytes();git('add','-A');git('commit','-qm','UI baseline');base_commit=git('rev-parse','HEAD').decode().strip();(P/'Review.txt').write_text('reviewed after\n')
for n in range(40):(P/('List%02d.txt'%n)).write_text('list file %d\n'%n)
# A blocking read barrier still delegates all successful operations to the real Git.
real_git=shutil.which('git');wrapper=traps/'git';slow=traps/'slow';started=traps/'slow-started.json'
wrapper.write_text('#!/usr/bin/python3\nimport os,sys,pathlib,time,json\nslow=pathlib.Path('+repr(str(slow))+')\nif "status" in sys.argv and slow.exists():\n pathlib.Path('+repr(str(started))+').write_text(json.dumps({"pid":os.getpid(),"pgid":os.getpgrp(),"argv":sys.argv}))\n while slow.exists():time.sleep(.1)\nos.execv('+repr(real_git)+', ['+repr(real_git)+',*sys.argv[1:]])\n');wrapper.chmod(0o755)

if a.serial:
 devices=json.loads(subprocess.check_output([str(CLI),'devices'],env=env));assert any(d['serial']==a.serial and d['state']=='device'for d in devices);(O/'devices-before.json').write_text(json.dumps(devices,indent=2)+'\n')
x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,capture_output=True,text=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
code=run_window([IDE,P,'--git-flow-smoke',W],env,O/'window.log',O/'process.json',220)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-9000:]
p=json.loads((W/'git-flow-window.json').read_text());assert p['result']=='GPU_GIT_FLOW_OK'and p['renderer']=='wgpu'and len(p['screenshots'])==13 and p['mutations']==2 and p['saved']==1 and p['opened']==1 and p['observed_changes']>=1
for k in ['no_implicit_save_before_action','buffer_only_tabs_and_exact_save','ordered_navigation_and_source_handoff','source_undo_redo_kept','outside_review_path_and_message_focus','save_conflict_no_overwrite','read_cancel_reopen_kept_draft']:assert p[k],k
assert git('log','-1','--format=%s').decode().strip()=='Flow commit 猫_continued';assert git('rev-parse','HEAD^').decode().strip()==base_commit;assert git('diff-tree','--no-commit-id','--name-only','-r','HEAD')==b'app/src/main/kotlin/dev/wex/android/MainActivity.kt\n';assert git('diff','--cached')==b''
assert main.read_bytes()==b'// FLOW_MAIN\n'+main_before;assert park.read_bytes()==park_before+b'// EXTERNAL_PARK\n';assert(P/'Review.txt').read_text()=='outside after\n';assert(P/'0-new.txt').read_text()=='external tracked insertion\n';assert not(P/'.git/index.lock').exists()and not marker.exists()and not(P/'.wex/native').exists()
barrier=json.loads(started.read_text());assert barrier['pid']==barrier['pgid']and not Path('/proc',str(barrier['pid'])).exists()and not slow.exists();assert len(p['external_writes'])==3 and all(x['exit']==0 and x['literal']=='FLOW_EXTERNAL_WRITE_OK\n' for x in p['external_writes'])
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
 if s['name'].startswith(('10-','11-','12-','13-')):
  assert(s['width'],s['height'])==(760,560)
  for k,r in s['footer'].items():assert 0<=r[0]<r[2]<=760 and 0<=r[1]<r[3]<=560,(k,r)
(O/'git-flow-e2e.json').write_text(json.dumps({'result':'NATIVE_GIT_FLOW_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'proof':p,'commit':git('rev-parse','HEAD').decode().strip(),'changed_commit_paths':['app/src/main/kotlin/dev/wex/android/MainActivity.kt'],'saved_main_sha256':hashlib.sha256(main.read_bytes()).hexdigest(),'read_barrier':barrier,'mtvm_serial':a.serial,'verification_mode':'desktop-only' if a.desktop_only else 'mtvm-present','android_runtime_exercised':False,'phone_untouched':True},indent=2)+'\n')
assert json.loads((O/'process.json').read_text())['remaining']==[]
print('NATIVE_GIT_FLOW_OK: real buffer-only tabs/F6/arrows/Home/End/Page/ordered Enter+source text; explicit selected Save and review/no auto-stage; outside writer+path-stable refresh+message focus; Undo/Redo; Main-only local commit; Save conflict/minimum footer; real read process-group cancellation/reopen;13 GPU frames;no Gradle/APK/phone;'+('mtvm='+a.serial if a.serial else 'desktop-only;Android runtime not tested'),flush=True)
