#!/usr/bin/env python3
"""Actual native production Git UI against an isolated local repository, never the user's repo."""
from pathlib import Path
from native_window_process import run_window
import argparse,hashlib,json,os,shutil,struct,subprocess
A=argparse.ArgumentParser();A.add_argument('output');mode=A.add_mutually_exclusive_group(required=True);mode.add_argument('--serial',choices=['emulator-5580']);mode.add_argument('--desktop-only',action='store_true',help='Native UI only; no Android runtime claim');a=A.parse_args();R=Path(__file__).resolve().parents[1];O=Path(a.output).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir();shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/n;f.write_text('#!/bin/sh\necho '+n+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
for k in list(env):
 if k.startswith('GIT_'):env.pop(k)
def git(*args):return subprocess.check_output(['git','-C',str(P),*args],env=env)
git('init','-q');git('config','user.name','Wex UI Fixture');git('config','user.email','fixture@localhost');git('config','commit.gpgsign','false');git('config','core.hooksPath',str(O/'empty-hooks'))
(P/'.gitignore').write_text('.wex/\n');(P/'Review.txt').write_text('reviewed before\n');git('add','-A');git('commit','-qm','UI baseline');base_commit=git('rev-parse','HEAD').decode().strip();main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';main.write_text('// DISK_GIT_CHANGE\n'+main.read_text());(P/'Review.txt').write_text('reviewed after\n');(P/'New 猫.txt').write_text('new unicode\n');disk=hashlib.sha256(main.read_bytes()).hexdigest()
if a.serial:
 devices=json.loads(subprocess.check_output([str(CLI),'devices'],env=env));assert any(d['serial']==a.serial and d['state']=='device'for d in devices);(O/'devices-before.json').write_text(json.dumps(devices,indent=2)+'\n')
x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,capture_output=True,text=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
code=run_window([IDE,P,'--git-smoke',W],env,O/'window.log',O/'process.json',220)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-9000:]
p=json.loads((W/'git-window.json').read_text());assert p['result']=='GPU_GIT_UX_OK'and p['renderer']=='wgpu'and len(p['screenshots'])==9 and p['mutations']==4 and p['actual_pointer_clicks']>=15 and p['dirty_buffer_preserved_and_undone']and p['no_implicit_save']
assert git('log','-1','--format=%s').decode().strip()=='Reviewed UI fixture commit';assert git('rev-parse','HEAD^').decode().strip()==base_commit;assert git('diff-tree','--no-commit-id','--name-only','-r','HEAD')==b'Review.txt\n';assert git('diff','--cached')==b'';assert hashlib.sha256(main.read_bytes()).hexdigest()==disk;assert(P/'Review.txt').read_text()=='reviewed after\n';assert(P/'New 猫.txt').read_text()=='new unicode\n';assert not(P/'.git/index.lock').exists()and not marker.exists()and not(P/'.wex/native').exists()
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
 if s['name'].startswith('07-'):
  assert(s['width'],s['height'])==(760,560)
  for k,r in s['footer'].items():assert 0<=r[0]<r[2]<=760 and 0<=r[1]<r[3]<=560,(k,r)
(O/'git-e2e.json').write_text(json.dumps({'result':'NATIVE_GIT_UX_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'proof':p,'commit':git('rev-parse','HEAD').decode().strip(),'changed_commit_paths':['Review.txt'],'main_disk_sha256':disk,'mtvm_serial':a.serial,'verification_mode':'desktop-only' if a.desktop_only else 'mtvm-present','android_runtime_exercised':False,'phone_untouched':True},indent=2)+'\n')
assert json.loads((O/'process.json').read_text())['remaining']==[]
print('NATIVE_GIT_UX_OK: isolated real Git status/diff/stage/unstage/local commit;dirty source excluded and Undo kept;foreign lock preserved/error/Refresh;minimum760x560 footer;keyboard reopen;9 GPU frames;no Save/Gradle/APK/phone;'+('mtvm='+a.serial if a.serial else 'desktop-only;Android runtime not tested'),flush=True)
