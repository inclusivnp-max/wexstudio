#!/usr/bin/env python3
"""Native first-frame profiling and real asynchronous large-file/Cancel/repair/Retry windows."""
from pathlib import Path
import hashlib,json,os,shutil,statistics,struct,subprocess,sys,time
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/n;p.write_text('#!/bin/sh\nprintf '+n+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system')};env.pop('WAYLAND_DISPLAY',None);env.pop('WGPU_BACKEND',None)
def png(s):
 data=Path(s['path']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',data[16:24])==(s['width'],s['height'])
def run(label,args,reduced=False):
 with (O/(label+'.log')).open('w')as log:x=subprocess.run([str(IDE),*map(str,args)],env={**env,**({"WEX_REDUCED_MOTION":"1"}if reduced else{})},stdout=log,stderr=subprocess.STDOUT,timeout=190)
 (O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert x.returncode==0,(label,(O/(label+'.log')).read_text()[-6000:])
results=[]
for n in range(3):
 out=O/f'welcome-{n}';run(f'welcome-{n}',['--startup-smoke',out]);s=json.loads((out/'startup.json').read_text());assert s['result']=='NATIVE_WELCOME_STARTUP_OK' and s['source_discovery_calls']==0 and s['editor_workers']==0 and s['document']is None and not s['k2_started'] and s['devices_loaded']==0 and s['startup_trace']['gpu_attempts']==1 and 'Vulkan' in s['startup_trace']['adapter'];assert s['font_fallback_ready'] and s['font_fallback_installed'];png({'path':out/'welcome-first.png','width':s['width'],'height':s['height']});png({'path':out/'welcome.png','width':s['width'],'height':s['height']});results.append(s)
retry=O/'gpu-retry';run('gpu-retry',['--startup-retry-smoke',retry]);retry=json.loads((retry/'startup.json').read_text());assert retry['startup_trace']['gpu_attempts']==2 and 'GPU_INITIALIZATION_RETRY:'in(O/'gpu-retry.log').read_text();assert len([m for m in retry['startup_trace']['marks_us']if m[0]=='gpu_ready'])==1;assert retry['source_discovery_calls']==0
cases=[]
for label,lines in [('open',60000),('cancel',500000)]:
 project=O/(label+'-project');project.mkdir();source=project/'Large.kt'
 with source.open('w')as f:
  f.write('// Real large Kotlin document; no synthetic parser delay\n')
  for n in range(lines):f.write(f'private val property{n} = {n}\n')
 original=hashlib.sha256(source.read_bytes()).hexdigest();out=O/(label+'-window');out.mkdir();run(label,[project,'--boot-'+label+'-smoke',out],reduced=label=='cancel');p=json.loads((out/'boot-window.json').read_text());assert p['result']==('GPU_ASYNC_PROJECT_OPEN_OK'if label=='open'else'GPU_ASYNC_PROJECT_CANCEL_OK')and p['worker_live_at_first_frame']and p['worker_acknowledged_end'];assert hashlib.sha256(source.read_bytes()).hexdigest()==original
 if label=='cancel':assert p['document']is None and p['welcome'] and p['k2']is None and p['reduced_motion']
 else:assert p['document_lines']>=lines and p['editor_rendered_rows']<200 and not p['welcome']
 for s in p['screenshots']:png(s)
 cases.append(p);assert not marker.exists();print(p['result'],p['startup_trace'],'document_ready_ms',p['document_ready_ms'],flush=True)
# A real malformed native config: error is visible before external repair; retry must import genuine K2.
project=O/'repair-project';shutil.copytree(R/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex'));(project/'wex.lock.json').unlink(missing_ok=True)
x=subprocess.run([str(B),'resolve',str(project),'--offline'],env=env,text=True,capture_output=True,timeout=180);assert x.returncode==0,x.stderr
config=project/'wex.json';valid=config.read_bytes();config.write_bytes(b'malformed JSON\n');source_hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in project.rglob('*')if p.is_file()and p!=config and ".wex"not in p.parts};out=O/'repair-window';out.mkdir()
with(O/'repair.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(project),'--boot-repair-smoke',str(out)],env=env,stdout=log,stderr=subprocess.STDOUT);deadline=time.monotonic()+160
 while not(out/'error-visible').exists():
  assert proc.poll()is None,'repair window exited before error frame';assert time.monotonic()<deadline,'error frame timeout';time.sleep(.03)
 config.write_bytes(valid);(out/'repair-ready').write_text('external config bytes restored\n');code=proc.wait(timeout=180)
(O/'repair.exit').write_text(str(code)+'\n');assert code==0,(O/'repair.log').read_text()[-6000:];p=json.loads((out/'boot-window.json').read_text());assert p['result']=='GPU_ASYNC_PROJECT_REPAIR_OK'and p['k2']['phase']=='Applied'and not any(d['severity']==1 for d in p['diagnostics']);assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in source_hashes.items())and config.read_bytes()==valid
for s in p['screenshots']:png(s)
cases.append(p);assert not marker.exists();evidence={'result':'NATIVE_STARTUP_ASYNC_OK','binaries':{str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in [B,IDE]},'welcomes':results,'gpu_retry':retry,'cases':cases,'median_first_update_ms':statistics.median(s['first_update_ms']for s in results),'median_first_gpu_capture_ms':statistics.median(s['first_gpu_screenshot_ms']for s in results),'real_source_sizes':{str(O/(label+'-project/Large.kt')):(O/(label+'-project/Large.kt')).stat().st_size for label in ['open','cancel']},'no_gradle_or_device_deployment':True}
(O/'startup-e2e.json').write_text(json.dumps(evidence,indent=2)+'\n');print('NATIVE_STARTUP_ASYNC_OK: 3 measured native welcome processes/zero workers-discovery-devices-K2; real failed GL initialization recovered before sole app creation; 60k-line source opens asynchronously/virtualized Ctrl+End; 500k-line actual parse Cancel acknowledged with no late commit; malformed native config error/external repair/Retry/actual K2; system CJK fallback preserved; no Gradle/device deployment',flush=True)
