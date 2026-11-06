#!/usr/bin/env python3
"""Real different-tree local branch checkout, open/parked sources, K2 and optional explicit mtvm Run."""
from pathlib import Path
from native_window_process import run_window
import argparse,hashlib,json,os,shutil,struct,subprocess,sys,time,zipfile
A=argparse.ArgumentParser();A.add_argument('output',type=Path);M=A.add_mutually_exclusive_group(required=True);M.add_argument('--desktop-only',action='store_true');M.add_argument('--serial',choices=['emulator-5580']);a=A.parse_args();R=Path(__file__).resolve().parents[1];O=a.output.resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir();shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True)
B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();f=P/'wex.json';c=json.loads(f.read_text());package='dev.wex.branch.case'+str(time.time_ns());c['android']['application_id']=package;f.write_text(json.dumps(c,indent=2)+'\n')
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 t=traps/name;t.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');t.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','IJ_JAVA_OPTIONS':'-XX:ActiveProcessorCount=2 -Xmx1024m'};env.pop('WAYLAND_DISPLAY',None)
for k in list(env):
 if k.startswith('GIT_'):env.pop(k)
def git(*args):return subprocess.check_output(['git','-C',str(P),*args],env=env)
main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';shared=P/'shared/src/main/kotlin/dev/wex/shared/Greeter.kt';legacy=main.parent/'LegacyOnly.kt';feature=main.parent/'BranchFeature.kt';resource=P/'app/src/main/res/values/strings.xml'
base=main.read_text().replace('        val greeter = Greeter()', '        val legacy = LegacyOnly().value()\n        Log.i("WexBranch", "WEX_BRANCH_OK tree=MAIN runId=${intent.getStringExtra("wex.run_id")}")\n        val greeter = Greeter()');main.write_text(base);legacy.write_text('package dev.wex.android\nclass LegacyOnly { fun value(): String = "LEGACY_TREE" }\n');old_shared=shared.read_text();old_resources=resource.read_text()
x=subprocess.run([str(B),'resolve',str(P),'--offline'],env=env,text=True,capture_output=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
git('init','-q','--initial-branch=main');git('config','user.name','Wex Branch Model Fixture');git('config','user.email','fixture@localhost');git('config','commit.gpgsign','false');git('config','core.hooksPath',str(O/'empty-hooks'));(P/'.gitignore').write_text('.wex/\n');git('add','-A');git('commit','-qm','main tree');base_oid=git('rev-parse','HEAD').decode().strip()
git('switch','-c','feature/model');changed=base.replace('        val legacy = LegacyOnly().value()\n','').replace('        Log.i("WexBranch", "WEX_BRANCH_OK tree=MAIN runId=${intent.getStringExtra("wex.run_id")}")','        val branchProof = BranchFeature().value(getString(R.string.branch_title))\n        Log.i("WexBranch", "WEX_BRANCH_OK tree=FEATURE value=$branchProof runId=${intent.getStringExtra("wex.run_id")}")').replace('greeter.greeting(', 'greeter.branchGreeting(');main.write_text(changed);legacy.unlink();feature.write_text('package dev.wex.android\nimport dev.wex.shared.Greeter\nclass BranchFeature { fun value(title: String): String = Greeter().branchGreeting(title) }\n');changed_shared=old_shared.replace('fun greeting(', 'fun branchGreeting(').replace('"Hello, $name!"','"Feature, $name!"');shared.write_text(changed_shared);resource.write_text(old_resources.replace('</resources>','<string name="branch_title">FEATURE_TREE</string>\n</resources>'));git('add','-A');git('commit','-qm','feature source and resource tree');feature_oid=git('rev-parse','HEAD').decode().strip();assert feature_oid!=base_oid;git('switch','main')
expected={'main':base,'feature':changed,'shared_main':old_shared,'shared_feature':changed_shared,'base_oid':base_oid,'feature_oid':feature_oid};(W/'expectations.json').write_text(json.dumps(expected,ensure_ascii=False,indent=2)+'\n')
initial={str(p.relative_to(P)):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.rglob('*')if p.is_file()and'.git'not in p.parts and'.wex'not in p.parts};(O/'execution-env.json').write_text(json.dumps({k:env[k]for k in ['JAVA_HOME','ANDROID_HOME','PATH','WEX_GRADLE_BRIDGE','WEX_PROJECT_HISTORY','WEX_K2_SYSTEM_DIR']},indent=2)+'\n')
adb=None;before=None
if a.serial:
 from adb_direct import Adb
 adb=Adb(a.serial);assert adb.shell('getprop','ro.kernel.qemu')['stdout'].strip()=='1';assert not adb.shell('pm','list','packages',package)['stdout'].strip()
 def settings():
  lines=adb.shell('settings','list','global')['stdout'].splitlines();return{k:[s for s in lines if s.startswith(k+'=')]for k in ['debug_app','wait_for_debugger']}
 before=settings();adb.shell('input','keyevent','224');adb.shell('wm','dismiss-keyguard')
try:
 command=[IDE,P,'--branch-model-smoke',W]+(['--serial',a.serial]if a.serial else['--desktop-only'])
 code=run_window(command,env,O/'window.log',O/'process.json',650 if a.serial else 330);(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-10000:]
 proof=json.loads((W/'branch-model-window.json').read_text());assert proof['result']=='GPU_GIT_BRANCH_MODEL_OK'and len(proof['screenshots'])==8 and proof['mutations']==3
 for key in ['active_and_parked_reloaded','clean_deleted_buffer_retained_not_published','restored_uri_republished','fresh_generated_resource_model','genuine_definition','ghost_symbol_unresolved','source_undo_and_disk_preserved']:assert proof[key],key
 assert main.read_text()==changed and shared.read_text()==changed_shared and not legacy.exists()and feature.exists();assert git('rev-parse','HEAD').decode().strip()==feature_oid and git('symbolic-ref','--short','HEAD')==b'feature/model\n'and git('status','--porcelain')==b'' and not marker.exists()
 if a.serial:
  run=proof['deployment'];assert run['serial']==a.serial and run['build_backend']=='wex-native-v1'and run['device']['emulator']and run['apk']['application_id']==package and run['install']['output']['exit_code']==0
  assert any(e['pid']==run['pid']and'WEX_BRANCH_OK tree=FEATURE value=Feature, FEATURE_TREE!'in e['message']and run['run_id']in e['message']for e in proof['logcat']);assert any(s['name']=='Verify APK signature'for s in run['native_build']['steps'])
  with zipfile.ZipFile(run['apk']['path'])as z:
   dex=b''.join(z.read(n)for n in z.namelist()if n.endswith('.dex'));assert b'Ldev/wex/android/BranchFeature;'in dex and b'Ldev/wex/android/LegacyOnly;'not in dex and b'WEX_BRANCH_OK tree=FEATURE'in dex
 else:assert proof['deployment']is None and not(P/'.wex/native').exists()
 for shot in proof['screenshots']:
  d=Path(shot['path']).read_bytes();assert d[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',d[16:24])==(shot['width'],shot['height'])
 (O/'branch-model-e2e.json').write_text(json.dumps({'result':'NATIVE_GIT_BRANCH_MODEL_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'proof':proof,'base_oid':base_oid,'feature_oid':feature_oid,'serial':a.serial,'initial_sources':initial,'no_gradle_or_adb_executable':True,'phone_untouched':True},indent=2)+'\n');print('NATIVE_GIT_BRANCH_MODEL_OK: three real different-tree switches; active/parked reload, deleted clean URI closed/retained/restored, genuine K2 generated resource model/F12/new class/removed ghost diagnostic/Undo;8 GPU states;'+('native APK/DEX/signature/install/launch/PID-run-bound binary Logcat exact '+a.serial if a.serial else'desktop only; no APK/VM/phone'),flush=True)
finally:
 if adb:
  adb.shell('am','force-stop','--user','current',package,check=False);gone=not adb.shell('pidof',package,check=False)['stdout'].strip();installed=adb.shell('pm','list','packages',package)['stdout'].strip();uninstall=adb.shell('pm','uninstall',package)if installed else None;assert before==settings()and gone
  (O/'cleanup.json').write_text(json.dumps({'target_pid_gone':gone,'owned_package_removed':not adb.shell('pm','list','packages',package)['stdout'].strip(),'uninstall':uninstall,'settings_before':before,'settings_after':settings(),'phone_untouched':True},indent=2)+'\n')
