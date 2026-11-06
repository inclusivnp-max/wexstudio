#!/usr/bin/env python3
"""Actual native source-marker UI and independent-process persistence reopen."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,struct,subprocess
from native_window_process import run_window,focus_callback
p=argparse.ArgumentParser();p.add_argument('output',type=Path);p.add_argument('--phase',choices=['initial','reopen'],required=True);p.add_argument('--reduced',action='store_true');a=p.parse_args();r=Path(__file__).resolve().parents[1];out=a.output.resolve()
if a.phase=='initial':
 out.mkdir(parents=True,exist_ok=False);shutil.copytree(r/'tests/fixtures/native-android',out/'project',ignore=shutil.ignore_patterns('.wex'))
 traps=out/'traps';traps.mkdir()
 for name in ['gradle','gradlew','mvn','adb','emulator']:
  f=traps/name;f.write_text('#!/bin/sh\nprintf '+name+' > "'+str(out/'FORBIDDEN_EXECUTED')+'"\nexit 99\n');f.chmod(0o755)
else:assert (out/'initial/proof.json').is_file()
project=out/'project';phase=out/a.phase;phase.mkdir();binary=Path(os.environ.get('WEX_BIN',r/'target/debug/wex')).resolve();ide=Path(os.environ.get('WEX_IDE',r/'target/debug/wex-ide')).resolve()
env={**os.environ,'PATH':str(out/'traps')+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(r/'.wex'),'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','IJ_JAVA_OPTIONS':'-XX:ActiveProcessorCount=2 -Xmx1024m','WEX_K2_SYSTEM_DIR':str(out/'k2-system'),'WEX_PROJECT_HISTORY':str(out/'history.json'),'WEX_LSP_TRACE':str(phase/'lsp.jsonl'),'WEX_K2_SHUTDOWN_REPORT':str(phase/'shutdown.json')}
env.pop('WAYLAND_DISPLAY',None);env.pop('WEX_BP_REOPEN',None);env.pop('WEX_REDUCED_MOTION',None)
if a.phase=='reopen':env['WEX_BP_REOPEN']='1'
if a.reduced:env['WEX_REDUCED_MOTION']='1'
if a.phase=='initial':
 c=subprocess.run([binary,'resolve',project,'--offline'],env=env,capture_output=True,text=True,timeout=180);(out/'resolve.log').write_text(c.stdout+c.stderr);assert c.returncode==0,c.stderr
 sources={str(f.relative_to(project)):hashlib.sha256(f.read_bytes()).hexdigest()for f in project.rglob('*')if f.is_file()and'.wex'not in f.parts};(out/'original.json').write_text(json.dumps(sources,indent=2)+'\n')
else:sources=json.loads((out/'original.json').read_text())
window=phase/'window';window.mkdir();cmd=[ide,project,'--breakpoints-smoke',window];code=run_window(cmd,env,phase/'window.log',phase/'process.json',300,on_start=focus_callback(cmd,phase,300));(phase/'window.exit').write_text(str(code)+'\n');assert code==0,(phase/'window.log').read_text()[-10000:]
q=json.loads((window/'breakpoints-window.json').read_text());assert q['result']=='NATIVE_BREAKPOINT_ANCHORS_OK'and q['reopen']==(a.phase=='reopen')and q['reduced_motion']==a.reduced
assert len(q['screenshots'])==(2 if a.phase=='reopen'else 4)
for shot in q['screenshots']:
 b=Path(shot['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(shot['width'],shot['height'])
assert all(hashlib.sha256((project/f).read_bytes()).hexdigest()==h for f,h in sources.items());assert not(out/'FORBIDDEN_EXECUTED').exists()and not(project/'.wex/native').exists()
prefs=json.loads((project/'.wex/breakpoints.json').read_text());assert prefs['schema']==2
if a.phase=='reopen':assert prefs['files']=={}
else:assert len(q['minimum_native_frames'])>=4 and len(set(q['minimum_native_frames']))==len(q['minimum_native_frames'])
if a.phase=='initial':
 markers=prefs['files'][str(project/'app/src/main/kotlin/dev/wex/android/MainActivity.kt')];assert len(markers)==1 and markers[0]['line']==q['line']and markers[0]['span']and len(markers[0]['source_hash'])==64
shutdown=json.loads((phase/'shutdown.json').read_text());assert shutdown=={'pending':0,'forced':0,'spawn_failed':0}
messages=[json.loads(l.split(' ',1)[1])for l in(phase/'lsp.jsonl').read_text().splitlines()];assert any(m.get('method')=='textDocument/diagnostic'for m in messages)
(phase/'proof.json').write_text(json.dumps({'result':'ACTUAL_NATIVE_BREAKPOINTS_OK','phase':a.phase,'binary':str(ide),'binary_sha256':hashlib.sha256(ide.read_bytes()).hexdigest(),'proof':q,'shutdown':shutdown,'source_original_restored':True,'no_APK_device_mutation':True},indent=2)+'\n')
print('ACTUAL_NATIVE_BREAKPOINTS_OK phase='+a.phase+' screenshots='+str(len(q['screenshots']))+' source_restored=true shutdown=clean')
