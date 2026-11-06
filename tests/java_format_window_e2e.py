#!/usr/bin/env python3
"""Actual winit/wgpu Java formatting with genuine K2 model; fresh owned project, no APK/device mutation."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,struct,subprocess,sys
from native_window_process import run_window,focus_callback
p=argparse.ArgumentParser();p.add_argument('output',type=Path);p.add_argument('--reduced',action='store_true');a=p.parse_args()
r=Path(__file__).resolve().parents[1];out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
project=out/'project';shutil.copytree(r/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex'))
traps=out/'traps';traps.mkdir();marker=out/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/name;f.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
binary=Path(os.environ.get('WEX_BIN',r/'target/debug/wex')).resolve();ide=Path(os.environ.get('WEX_IDE',r/'target/debug/wex-ide')).resolve()
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(r/'.wex'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','IJ_JAVA_OPTIONS':'-XX:ActiveProcessorCount=2 -Xmx1024m','WEX_K2_SYSTEM_DIR':str(out/'k2-system'),'WEX_PROJECT_HISTORY':str(out/'history.json'),'WEX_LSP_TRACE':str(out/'lsp.jsonl'),'WEX_K2_SHUTDOWN_REPORT':str(out/'shutdown.json'),'WEX_FORMATTER_OFFLINE':'1','WEX_JAVA_FORMAT_TRACE':str(out/'java-commands.jsonl')}
env.pop('WAYLAND_DISPLAY',None);env.pop('WEX_REDUCED_MOTION',None)
if a.reduced:env['WEX_REDUCED_MOTION']='1'
x=subprocess.run([binary,'resolve',project,'--offline'],env=env,text=True,capture_output=True,timeout=180)
(out/'resolve.json').write_text(x.stdout);(out/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
original={str(f.relative_to(project)):hashlib.sha256(f.read_bytes()).hexdigest()for f in project.rglob('*')if f.is_file()and'.wex'not in f.parts}
window=out/'window';window.mkdir();cmd=[ide,project,'--java-format-smoke',window]
code=run_window(cmd,env,out/'window.log',out/'process.json',300,on_start=focus_callback(cmd,out,300))
(out/'window.exit').write_text(str(code)+'\n');assert code==0,(out/'window.log').read_text()[-10000:]
q=json.loads((window/'format-window.json').read_text());assert q['result']=='NATIVE_JAVA_FORMATTING_OK' and len(q['screenshots'])==8
layout=json.loads((window/'minimum-layout.json').read_text());assert layout['reduced_motion']==a.reduced
frames=layout['observed_native_frames'];assert len(frames)>=4 and len({x['frame']for x in frames})==len(frames) and all(x['width']==760 and x['height']==560 for x in frames)
assert layout['viewport']=={'min':[0.0,0.0],'max':[760.0,560.0]}
for rect in layout['footer']:assert all(0<=rect['min'][i]<=rect['max'][i]<=limit for i,limit in enumerate([760,560]))

for k in ['same_frame_paste_then_format','cancel_preserved_history','selection_formatting','single_undo_redo','minimum_footer','already_formatted','same_frame_cancel_and_late_result_guard','source_disk_unchanged','original_buffer_restored','genuine_K2_model_and_restored_Kotlin_diagnostics','dirty_parked_Kotlin_restored','no_APK_or_device_mutation']:assert q[k],k
for shot in q['screenshots']:
 data=Path(shot['path']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',data[16:24])==(shot['width'],shot['height'])
messages=[json.loads(x.split(' ',1)[1])for x in (out/'lsp.jsonl').read_text().splitlines()if x.startswith('send ')]
assert any(x.get('method')=='textDocument/didChange' and x.get('params',{}).get('textDocument',{}).get('uri','').endswith('MainActivity.kt') for x in messages)
assert not any(x.get('method')in['textDocument/formatting','textDocument/rangeFormatting']and x.get('params',{}).get('textDocument',{}).get('uri','').endswith('.java') for x in messages)
commands=[json.loads(l)for l in(out/'java-commands.jsonl').read_text().splitlines()]
assert len([x for x in commands if x['stage']=='spawn'and x['program'].endswith('/bin/java')])>=4
assert q['Java_formatter']=='google-java-format 1.37.0/AOSP'
assert all(hashlib.sha256((project/f).read_bytes()).hexdigest()==h for f,h in original.items())
assert not marker.exists() and not(project/'.wex/native').exists()
shutdown=json.loads((out/'shutdown.json').read_text());assert shutdown=={'pending':0,'forced':0,'spawn_failed':0},shutdown
(out/'format-window-e2e.json').write_text(json.dumps({'result':'ACTUAL_NATIVE_JAVA_FORMATTING_OK','mode':'reduced'if a.reduced else'normal','binary':str(ide),'binary_sha256':hashlib.sha256(ide.read_bytes()).hexdigest(),'proof':q,'original_sources':original,'normal_shutdown':shutdown,'no_APK_VM_or_device_mutation':True},indent=2)+'\n')
print('ACTUAL_NATIVE_JAVA_FORMATTING_OK: actual Google Java file+selection, genuine K2 model and parked Kotlin Undo; same-frame input; preview/Cancel/Apply/Undo/Redo;760x560 footer/no-op/late-result cancellation;8 native GPU captures; original source restored; no Gradle/APK/VM/device mutation')
