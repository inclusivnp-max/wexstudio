#!/usr/bin/env python3
"""Build/install/launch the actual K2-renamed sources on an explicitly named mtvm."""
from pathlib import Path
import json,os,shutil,subprocess,sys,tempfile
ROOT=Path(__file__).resolve().parents[1]
assert len(sys.argv)==4,'Usage: refactor_native_e2e.py REFACTOR_RECEIPT OUTPUT EXPLICIT_EMULATOR_SERIAL'
receipt=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve();serial=sys.argv[3]
assert serial.startswith('emulator-') and serial[9:].isdigit(), 'explicit emulator serial required'
r=json.loads(receipt.read_text());assert r['result']=='GUI_REFACTOR_OK' and r['disk_source_unchanged']
out.mkdir(parents=True,exist_ok=True);project=out/'renamed-android'
assert not project.exists(),'use a fresh output directory to preserve existing evidence'
shutil.copytree(ROOT/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex','.gradle','build'))
for f in r['renamed_sources']:
 p=Path(f['path']);assert not p.is_absolute() and '..' not in p.parts
 (project/p).write_text(f['after'])
assert not list(project.rglob('*.gradle*')) and not list(project.rglob('gradlew'))
binary=Path(os.environ.get('WEX_BIN',ROOT/'target/debug/wex')).resolve()
info=subprocess.run([str(binary),'device-info','--serial',serial],capture_output=True,text=True,check=True)
assert json.loads(info.stdout)['emulator']
with tempfile.TemporaryDirectory() as tmp:
 traps=Path(tmp);marker=traps/'GRADLE_OR_MAVEN_EXECUTED'
 for n in ['gradle','gradlew','mvn','bridge']:
  p=traps/n;p.write_text('#!/bin/sh\nprintf forbidden > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
 env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ.get('PATH',''),'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
 command=[str(binary),'run',str(project),'--serial',serial,'--offline','--log-seconds','4','--require-marker','WEX_NATIVE_OK']
 result=subprocess.run(command,env=env,capture_output=True,text=True,timeout=300)
 (out/'run-command.json').write_text(json.dumps(command,indent=2)+'\n');(out/'run.log').write_text(result.stderr);(out/'run.stdout').write_text(result.stdout);(out/'run.exit').write_text(str(result.returncode)+'\n')
 assert result.returncode==0,result.stderr
 run=json.loads(result.stdout);assert run['result']=='RUN_OK' and run['deployment']['serial']==serial and run['deployment']['device']['emulator']
 assert run['deployment']['build_backend']=='wex-native-v1' and run['deployment']['install']['output']['exit_code']==0 and run['logcat']['marker_found']
 assert not marker.exists(),'Gradle/Maven invoked'
 (out/'renamed-run.json').write_text(json.dumps(run,indent=2)+'\n')
print('REFACTOR_ANDROID_OK: '+serial+'; actual K2-renamed sources + unsaved extra caller compile; native APK/install/launch/PID Logcat; no Gradle/Maven')
print('RENAMED_PROJECT: '+str(project))
