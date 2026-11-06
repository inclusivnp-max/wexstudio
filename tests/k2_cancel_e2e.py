#!/usr/bin/env python3
"""Actual pinned K2 cancellation + typed follow-up/restore/F12, no APK or device."""
from pathlib import Path
import argparse,hashlib,json,os,re,shutil,subprocess,sys
from native_window_process import run_window
p=argparse.ArgumentParser();p.add_argument('output',type=Path);p.add_argument('--control',action='store_true');p.add_argument('--declarations',action='store_true');a=p.parse_args();r=Path(__file__).resolve().parents[1];out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);project=out/'project';shutil.copytree(r/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex'));(project/'CANCEL_FIXTURE').write_text('disposable actual K2 cancellation fixture\n');traps=out/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/name;f.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
trace=out/'lsp.jsonl';proof=out/'genuine-k2.json';binary=Path(os.environ.get('WEX_BIN',r/'target/debug/wex')).resolve();server=r/'.wex/tools/kotlin-lsp/kotlin-server-263.6379.0/bin/intellij-server'
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(r/'.wex'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','IJ_JAVA_OPTIONS':'-XX:ActiveProcessorCount=2 -Xmx1024m','WEX_K2_CANCEL_PROJECT':str(project),'WEX_K2_CANCEL_OUTPUT':str(proof),'WEX_KOTLIN_LSP':str(server),'WEX_K2_SYSTEM_DIR':str(out/'k2-system'),'WEX_LSP_TRACE':str(trace),'CARGO_BUILD_JOBS':'1','RUST_TEST_THREADS':'1'}
env.pop('WEX_K2_CANCEL_CONTROL',None);env.pop('WEX_K2_CANCEL_DECLARATIONS',None)
if a.control:env['WEX_K2_CANCEL_CONTROL']='1'
if a.declarations:env['WEX_K2_CANCEL_DECLARATIONS']='1'
resolver=subprocess.run([binary,'resolve',project,'--offline'],env=env,text=True,capture_output=True,timeout=180);(out/'resolve.json').write_text(resolver.stdout);(out/'resolve.log').write_text(resolver.stderr);assert resolver.returncode==0,resolver.stderr
sources={str(f.relative_to(project)):hashlib.sha256(f.read_bytes()).hexdigest()for f in project.rglob('*')if f.is_file()and'.wex'not in f.parts}
code=run_window(['cargo','test','--lib','--features','desktop','--locked','genuine_k2_staged_cancel_and_followup_survive','--','--ignored','--nocapture'],env,out/'probe.log',out/'process.json',600);(out/'probe.exit').write_text(str(code)+'\n');assert code==0,(out/'probe.log').read_text()[-10000:];assert re.search(r'test result: ok\. 1 passed; 0 failed;', (out/'probe.log').read_text())
q=json.loads(proof.read_text());assert q['result']==('GENUINE_K2_CANCEL_CONTROL_OK'if a.control else'GENUINE_K2_CANCEL_OK')and len(q['rounds'])==3 and q['source_disk_unchanged']and q['no_apk_or_device'];assert all(hashlib.sha256((project/f).read_bytes()).hexdigest()==s for f,s in sources.items())and not marker.exists()and not(project/'.wex/native').exists()
messages=[]
for line in trace.read_text().splitlines():
 direction,body=line.split(' ',1);messages.append((direction,json.loads(body)))
requests={m['id']:m for direction,m in messages if direction=='send'and'id'in m and'method'in m};cancel=[m for direction,m in messages if direction=='send'and m.get('method')=='$/cancelRequest'];assert len(cancel)==(0 if a.control else 3) and len({m['params']['id']for m in cancel})==len(cancel),cancel
for m in cancel:assert'id'not in m and requests[m['params']['id']]['method']=='textDocument/diagnostic'
ids={m['params']['id']for m in cancel};replies=[m for direction,m in messages if direction=='recv'and m.get('id')in ids and'method'not in m];ack=[m for m in replies if m.get('error',{}).get('code')==-32800]
result={'result':'ACTUAL_K2_CANCEL_CONTROL_OK'if a.control else'ACTUAL_K2_CANCEL_PROTOCOL_OK','probe':q,'cancellations':cancel,'late_responses':replies,'backend_request_cancelled_errors':len(ack),'backend_cpu_interruption_guaranteed':False,'ordinary_followups_healthy':True,'no_gradle_apk_device':True,'remaining':json.loads((out/'process.json').read_text())['remaining']};assert result['remaining']==[];(out/'k2-cancel-e2e.json').write_text(json.dumps(result,indent=2)+'\n')
print(('ACTUAL_K2_CANCEL_CONTROL_OK:0 cancellation notifications;'if a.control else'ACTUAL_K2_CANCEL_PROTOCOL_OK:3 exact diagnostic-id notifications;')+str(len(ack))+' backend RequestCancelled replies; genuine dirty error/restore/shared F12 survive; disk unchanged; no Gradle/APK/device; no universal CPU-stop guarantee',flush=True)
