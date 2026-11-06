#!/usr/bin/env python3
"""Real native Find in Files widgets, source edits, worker snapshots and external writer."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,struct,subprocess
A=argparse.ArgumentParser();A.add_argument('output');A.add_argument('--serial');a=A.parse_args();R=Path(__file__).resolve().parents[1];O=Path(a.output).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir();shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
base=P/'app/src/main/kotlin/dev/wex/android';MARK='WEX_PROJECT_検索';(base/'ProjectPark.kt').write_text('package dev.wex.android\nfun projectPark(): Int = 7\n');(base/'ProjectDisk.txt').write_text('😀 '+MARK+' CLOSED\n');(base/'ProjectStale.txt').write_text(MARK+' STALE\n');(P/'app/src/main/res/values/project_search.xml').write_text('<resources><string name="project_search_probe">'+MARK+' XML</string></resources>\n');(P/'.search-note').write_text(MARK+' HIDDEN\n');(P/'binary.bin').write_bytes((MARK+'\0 BINARY').encode());(P/'build').mkdir(exist_ok=True);(P/'build/excluded.txt').write_text(MARK+' EXCLUDED\n')
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/n;f.write_text('#!/bin/sh\nprintf '+n+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
if a.serial:
 assert a.serial=='emulator-5580';devices=json.loads(subprocess.check_output([str(CLI),'devices'],env=env));assert any(d['serial']==a.serial and d['state']=='device'for d in devices);(O/'devices-before.json').write_text(json.dumps(devices,indent=2)+'\n')
x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,capture_output=True,text=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);(O/'resolve.exit').write_text(str(x.returncode)+'\n');assert x.returncode==0,x.stderr
hashes={str(f):hashlib.sha256(f.read_bytes()).hexdigest()for f in P.rglob('*')if f.is_file()and'.wex'not in f.relative_to(P).parts}
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),'--project-search-smoke',str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE),'project':str(P)})+'\n')
 try:code=proc.wait(timeout=300)
 finally:
  if proc.poll()is None:assert str(P).encode()in Path('/proc',str(proc.pid),'cmdline').read_bytes();proc.terminate();proc.wait(timeout=15)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-10000:]
p=json.loads((W/'project-search-window.json').read_text());assert p['result']=='GPU_PROJECT_SEARCH_OK'and p['renderer']=='wgpu'and len(p['screenshots'])==12
for k in ['source_buffers_restored','dirty_active_and_parked_overlaid','actual_widget_result_navigation','actual_keyboard_result_navigation','actual_pointer_query_reentry','actual_query_shortcut_same_frame','unicode_character_selection','undo_redo_preserved','invalid_regex_last_good','refresh_recovered','stop_debounced_search','zero_results']:assert p[k],k
assert p['disk_stale_navigation_rejected']==1 and p['worker_active']==0 and p['actual_pointer_clicks']>=10 and not p['save_worker_started']and not p['deployment_invoked']
first=p['snapshots'][0]['result'];assert first['stats']['matches']==6 and len(first['files'])==6 and sum(f['unsaved']for f in first['files'])==2 and first['worker_thread']!=p['foreground_thread']
assert len(p['external_writes'])==2 and all(w['result']=='SEARCH_EXTERNAL_WRITE_OK'and w['exit']==0 for w in p['external_writes'])
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()and not(P/'.wex/native').exists()
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
(O/'project-search-e2e.json').write_text(json.dumps({'result':'NATIVE_PROJECT_SEARCH_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_apk_or_phone':True,'mtvm_serial':a.serial},indent=2)+'\n')
print('NATIVE_PROJECT_SEARCH_OK: genuine native M3 results/disk+dirty active+parked/shortcut+query one frame/actual result widgets/Unicode navigation/Undo+Redo/regex error+recovery/external disk stale rejection/Refresh/Stop/zero results/source restoration;12 GPU frames;no Gradle/APK/phone;mtvm='+str(a.serial),flush=True)
