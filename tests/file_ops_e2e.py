#!/usr/bin/env python3
"""Actual native file lifecycle + K2 F12 + Kotlin/Java/APK/direct ADB Run on explicit mtvm."""
from pathlib import Path
from native_window_process import run_window
import argparse,hashlib,json,os,shutil,struct,subprocess,sys,time,zipfile
R=Path(__file__).resolve().parents[1]
a=argparse.ArgumentParser();a.add_argument('output',type=Path);a.add_argument('--serial',required=True,choices=['emulator-5580']);args=a.parse_args();O=args.output.resolve();O.mkdir(parents=True,exist_ok=True)
B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve()
sys.path.insert(0,str(R/'tests'));from adb_direct import Adb
adb=Adb(args.serial);assert adb.shell('getprop','ro.kernel.qemu')['stdout'].strip()=='1'
settings=lambda: {k:[l for l in adb.shell('settings','list','global')['stdout'].splitlines() if l.startswith(k+'=')] for k in ['debug_app','wait_for_debugger']}
before=settings();P=O/'project';shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True)
f=P/'wex.json';c=json.loads(f.read_text());package='dev.wex.fileops.case'+str(time.time_ns());c['android']['application_id']=package;f.write_text(json.dumps(c,indent=2)+'\n');assert not adb.shell('pm','list','packages',package)['stdout'].strip()
sources={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in P.rglob('*') if p.is_file()}
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/n;p.write_text('#!/bin/sh\nprintf '+n+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system')};env.pop('WAYLAND_DISPLAY',None);(O/'execution-env.json').write_text(json.dumps({k:env[k] for k in ['JAVA_HOME','ANDROID_HOME','PATH','WEX_GRADLE_BRIDGE','WEX_PROJECT_HISTORY','WEX_K2_SYSTEM_DIR']},indent=2)+'\n')
adb.shell('input','keyevent','224');adb.shell('wm','dismiss-keyguard');(O/'window').mkdir()
try:
 x=subprocess.run([str(B),'resolve',str(P),'--offline'],env=env,text=True,capture_output=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
 code=run_window([IDE,P,'--files-smoke',O/'window','--serial',args.serial],env,O/'window.log',O/'process.json',630)
 (O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-6000:]
 proof=json.loads((O/'window/files-window.json').read_text());assert proof['result']=='GUI_FILE_LIFECYCLE_MTVM_OK' and proof['serial']==args.serial and proof['path_only_rename'] and proof['undo_preserved_through_move_trash_restore'] and proof['saved_affected_only'] and proof['dirty_unrelated_preserved_until_run']
 run=proof['deployment'];assert run['serial']==args.serial and run['device']['emulator'] and run['apk']['application_id']==package and run['build_backend']=='wex-native-v1' and run['install']['output']['exit_code']==0
 assert proof['k2']['model']['phase']=='Applied' and not any(d['severity']==1 for d in proof['k2']['diagnostics'])
 assert any(e['pid']==run['pid'] and 'WEX_FILE_OPS_OK value=Hello from WorkbenchFeature' in e['message'] and run['run_id'] in e['message'] for e in proof['logcat'])
 assert any(s['name']=='Verify APK signature' for s in run['native_build']['steps'])
 with zipfile.ZipFile(run['apk']['path']) as z:
  dex=b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'));assert b'Ldev/wex/android/WorkbenchFeature;' in dex and b'Hello from WorkbenchFeature' in dex and b'WEX_FILE_OPS_OK' in dex
 main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';assert main.read_text().count('val fileProof = WorkbenchFeature().message()')==1
 assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in sources.items() if Path(p)!=main)
 assert not list(P.glob('**/FeatureRenamed.kt')) and len(proof['screenshots'])==8
 for shot in proof['screenshots']:
  data=Path(shot['path']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',data[16:24])==(shot['width'],shot['height'])
 assert not marker.exists();(O/'file-ops-e2e.json').write_text(json.dumps({'result':'NATIVE_FILE_OPS_MTVM_OK','serial':args.serial,'package':package,'binaries':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in [B,IDE]},'proof':proof,'baseline_sources':sources,'no_gradle_or_adb_executable':True,'phone_untouched':True},indent=2)+'\n')
 print('NATIVE_FILE_OPS_MTVM_OK: actual wgpu RawInput create folder/Kotlin class; Save affected; path-only rename/move; persistent trash/restore; dirty unrelated Main + Undo preserved; genuine K2 F12 to moved class; real Kotlin/Java/resource/DEX/signature/install/launch/binary Logcat exact emulator-5580; 8 GPU frames; no Gradle; Pixel untouched',flush=True)
finally:
 adb.shell('am','force-stop','--user','current',package,check=False);gone=not adb.shell('pidof',package,check=False)['stdout'].strip();installed=adb.shell('pm','list','packages',package)['stdout'].strip();removed=adb.shell('pm','uninstall',package) if installed else None
 after=settings();assert before==after and gone
 (O/'cleanup.json').write_text(json.dumps({'target_pid_gone':gone,'owned_package_removed':not adb.shell('pm','list','packages',package)['stdout'].strip(),'uninstall':removed,'debug_settings_before':before,'debug_settings_after':after,'phone_untouched':True},indent=2)+'\n')
