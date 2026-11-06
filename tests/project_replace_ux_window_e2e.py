#!/usr/bin/env python3
"""Real production replacement UX at exact minimum760x560, including keyboard/IME/errors."""
from pathlib import Path
from native_window_process import run_window
import argparse,hashlib,json,os,shutil,struct,subprocess
A=argparse.ArgumentParser();A.add_argument('output');mode=A.add_mutually_exclusive_group(required=True);mode.add_argument('--serial',choices=['emulator-5580']);mode.add_argument('--desktop-only',action='store_true',help='Native UI only; no Android runtime claim');a=A.parse_args();R=Path(__file__).resolve().parents[1];O=Path(a.output).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir();shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
base=P/'app/src/main/kotlin/dev/wex/android';MARK='WEX_REPLACE_検索';(base/'ReplacePark.kt').write_text('package dev.wex.android\nfun replacePark(): Int = 7\n');(base/'ReplaceDisk.txt').write_text('😀 '+MARK+' CLOSED\n');(base/'ReplaceStale.txt').write_text(MARK+' STALE\n')
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/n;f.write_text('#!/bin/sh\nprintf '+n+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
if a.serial:
 devices=json.loads(subprocess.check_output([str(CLI),'devices'],env=env));assert any(d['serial']==a.serial and d['state']=='device'for d in devices);(O/'devices-before.json').write_text(json.dumps(devices,indent=2)+'\n')
x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,capture_output=True,text=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);(O/'resolve.exit').write_text(str(x.returncode)+'\n');assert x.returncode==0,x.stderr
hashes={str(f):hashlib.sha256(f.read_bytes()).hexdigest()for f in P.rglob('*')if f.is_file()and'.wex'not in f.relative_to(P).parts}
code=run_window([IDE,P,'--project-replace-ux-smoke',W],env,O/'window.log',O/'process.json',240)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-10000:]
p=json.loads((W/'project-replace-ux-window.json').read_text());assert p['result']=='GPU_PROJECT_REPLACE_UX_OK'and p['renderer']=='wgpu'and len(p['screenshots'])==11
for k in ['editable_find_and_options','labelled_fields_default_focus','actual_shift_tab_find','enter_preview_command_enter_apply','stale_review_retained_not_applied','real_regex_error_recovery','footer_always_visible','expanded_error_details_bounded','ime_candidate_enter_did_not_submit','no_op_feedback','cancel_focus_button_keyboard_reopen','source_buffers_restored']:assert p[k],k
assert(p['applied'],p['undone'],p['cancelled'])==(1,1,2) and p['minimum_size']==[760,560]
assert p['actual_pointer_clicks']>=10 and not p['save_worker_started']and not p['deployment_invoked']
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()and not(P/'.wex/native').exists()
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
 if s['name'].startswith(('05-','06-','07-','10-','11-')):
  assert(s['width'],s['height'])==(760,560)
  for k in ['pr-preview','pr-apply','pr-cancel']:
   x1,y1,x2,y2=s['footer'][k];assert 0<=x1<x2<=760 and 0<=y1<y2<=560,(s['name'],k)
(O/'project-replace-ux-e2e.json').write_text(json.dumps({'result':'NATIVE_PROJECT_REPLACE_UX_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_apk_or_phone':True,'mtvm_serial':a.serial,'verification_mode':'desktop-only' if a.desktop_only else 'mtvm-present','android_runtime_exercised':False},indent=2)+'\n')
assert json.loads((O/'process.json').read_text())['remaining']==[]
print('NATIVE_PROJECT_REPLACE_UX_OK: labelled editable Find/options/default focus/real ShiftTab/Enter preview/CtrlEnter reviewed apply/retained stale results/real long regex error+recovery/minimum760x560 footer+expanded details/IME candidate Enter blocked/no-op feedback/Cancel focus+keyboard reopen/source+local history restoration;11 GPU frames;no Save/Gradle/APK/phone;'+('mtvm='+a.serial if a.serial else 'desktop-only;Android runtime not tested'),flush=True)
