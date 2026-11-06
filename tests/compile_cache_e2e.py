#!/usr/bin/env python3
"""Real native SDK/Kotlin/Java module cache; independent compiler-call counters."""
from pathlib import Path
import json,os,shutil,subprocess,sys,time,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();P=O/'project'
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True)
f=P/'wex.json';cfg=json.loads(f.read_text());cfg['android']['application_id']='dev.wex.cache.e2e.case'+str(time.time_ns());f.write_text(json.dumps(cfg,indent=2)+'\n')
J=O/'jdk';(J/'bin').mkdir(parents=True);real=Path('/usr/lib/jvm/java-21-openjdk');calls=O/'compiler-calls.jsonl'
for name in ['lib','conf','jmods','include','legal']:
 if (real/name).exists():(J/name).symlink_to(real/name,target_is_directory=True)
shutil.copy2(real/'release',J/'release')
for tool in ['java','javac','jar','jmod','jlink','keytool','javap']:
 dst=J/'bin'/tool;shutil.copy2(real/'bin'/tool,dst)
 if tool in ['java','javac']:
  backing=dst.with_suffix('.real');dst.rename(backing)
  dst.write_text('#!/usr/bin/python3\nimport json,os,sys\nwith open('+repr(str(calls))+',"a") as f:f.write(json.dumps({"tool":'+repr(tool)+',"args":sys.argv[1:]})+"\\n")\nos.execv('+repr(str(backing))+',['+repr(str(backing))+']+sys.argv[1:])\n');dst.chmod(0o755)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\necho '+name+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
env={**os.environ,'JAVA_HOME':str(J),'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
def invoke(label,args,ok=True):
 x=subprocess.run([str(B),*map(str,args)],env=env,text=True,capture_output=True,timeout=300)
 (O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert (x.returncode==0)==ok,(label,x.returncode,x.stderr[-5000:]);return x
invoke('resolve',['resolve',P,'--offline'])
records={};observed={}
def call_count():return len(calls.read_text().splitlines()) if calls.exists() else 0
def compiler_modules(start):
 out=[]
 for line in calls.read_text().splitlines()[start:]:
  c=json.loads(line);a=c['args']
  if 'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler' in a:out.append(('Kotlin',a[a.index('-module-name')+1]))
  if c['tool']=='javac' and '-d' in a and '/classes/' in a[a.index('-d')+1]:out.append(('Java',Path(a[a.index('-d')+1]).name))
 return out

def build(label,expected):
 start=call_count();x=invoke(label,['build',P,'--offline']);v=json.loads(x.stdout);cache={s['module']:s for s in v['compilation_cache']};assert {n:s['state'] for n,s in cache.items()}==expected,(label,cache)
 counted=compiler_modules(start);wanted=[]
 for n,state in expected.items():
  if state=='miss':wanted.append(('Kotlin',n));wanted.extend([('Java',n)] if n=='app' else [])
 assert sorted(counted)==sorted(wanted),(label,counted,wanted)
 names=[s['name'] for s in v['steps']]
 for n,state in expected.items():assert (f'Kotlin K2 · {n}' in names)==(state=='miss')
 assert ('D8 · JVM bytecode → DEX' in names)==(v['dex_cache'][0]['state']!='hit')
 assert 'Verify APK signature' in names and 'Verify ZIP alignment' in names
 assert all(s['exit_code']==0 for s in v['steps']) and v['signing']['kind']=='debug'
 graph=json.loads(Path(v['graph_file']).read_text());assert graph['root']==str(P)
 for m in graph['modules']:assert all(Path(p).is_relative_to(v['build_dir']) for p in m['classes_roots'])
 records[label]=v;observed[label]={'expected':expected,'actual_compiler_calls':counted,'elapsed_ms':v['elapsed_ms'],'blake3':v['blake3']}
 assert not marker.exists();print('COMPILE_CACHE_CASE_OK',label,expected,'real module compiler calls',counted,flush=True);return v
miss={'shared':'miss','app':'miss'};hit={'shared':'hit','app':'hit'}
first=build('cold',miss);repeat=build('repeat',hit);assert first['blake3']==repeat['blake3']
java=P/'app/src/main/java/dev/wex/android/JavaGreeting.java';java.write_text(java.read_text().replace('Java + Kotlin: ','WEX_CACHE_APP_V2: '));build('app-body',{'shared':'hit','app':'miss'})
shared=P/'shared/src/main/kotlin/dev/wex/shared/Greeter.kt';shared.write_text(shared.read_text().replace('Hello,','WEX_CACHE_DEP_V2,'));build('dependency-body',miss)
ghost=shared.with_name('Ghost.kt');ghost.write_text('package dev.wex.shared\nclass Ghost { fun unused(): Int = 99 }\n');build('add-class',miss);ghost.unlink();removed=build('remove-class',hit)
def dex(report):
 with zipfile.ZipFile(report['apk']['path']) as z:return b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'))
assert b'Ldev/wex/shared/Ghost;' not in dex(removed)
assert b'WEX_CACHE_APP_V2' in dex(removed) and b'WEX_CACHE_DEP_V2' in dex(removed)
resource=P/'app/src/main/res/values/strings.xml';base=resource.read_text();resource.write_text(base.replace('</string>',' CACHE_RESOURCE_VALUE</string>',1));value=build('resource-value',hit)
resource.write_text(resource.read_text().replace('</resources>','<string name="zz_cache_new">CACHE_NEW_FIELD</string></resources>'));build('resource-field',{'shared':'hit','app':'miss'})
key=records['resource-field']['compilation_cache'];appkey=next(s['key'] for s in key if s['module']=='app');root=P/'.wex/compile-cache/wex-module-classes-v1/debug'
classes=root/'app'/appkey/'classes';target=next(classes.rglob('MainActivity.class'));target.write_bytes(target.read_bytes()+b'PRIVATE_CORRUPTION');tamper=build('tampered-class',{'shared':'hit','app':'miss'});assert 'Invalid class cache' in next(s['detail'] for s in tamper['compilation_cache'] if s['module']=='app')
sharedkey=next(s['key'] for s in tamper['compilation_cache'] if s['module']=='shared');(root/'shared'/sharedkey/'classes/Injected.class').write_bytes(b'invalid');extra=build('unrecorded-output',{'shared':'miss','app':'hit'});assert b'Injected' not in dex(extra)
pointer=P/'.wex/native/debug/current.json';prior=pointer.read_bytes();saved=java.read_bytes();dirs={str(p) for p in root.glob('*/*') if p.is_dir()};java.write_text('deliberately invalid Java;\n')
start=call_count()
try:
 invoke('invalid-java',['build',P,'--offline'],False)
 assert pointer.read_bytes()==prior and set(str(p) for p in root.glob('*/*') if p.is_dir())==dirs
 assert ('Kotlin','app') in compiler_modules(start) # K2 also consumes Java sources and can reject malformed Java before javac.
finally:java.write_bytes(saved)
recovered=build('source-recovered',hit);assert recovered['blake3']==extra['blake3']
release=J/'release';saved=release.read_bytes();release.write_bytes(saved+b'\n');build('jdk-input-change',miss);release.write_bytes(saved);build('jdk-input-restored',hit)
# Same source/CP can be compiled under explicit JVM options, but must use another key.
env['JAVA_TOOL_OPTIONS']='-Dwex.compile.cache.fixture=changed';build('jvm-options-change',miss);env.pop('JAVA_TOOL_OPTIONS');final=build('jvm-options-restored',hit)
assert first['signing']['keystore']==final['signing']['keystore'];assert not marker.exists()
report={'result':'MODULE_COMPILE_CACHE_E2E_OK','project':str(P),'jdk':str(J),'traps':str(traps),'cases':observed,'actual_compiler_counters':str(calls),'source_removal_drops_class':True,'corrupt_and_unrecorded_outputs_rebuilt':True,'failed_source_keeps_apk_and_cache':True,'jdk_and_jvm_option_invalidation':True,'resource_value_reuses_classes':True,'resource_symbols_invalidate_app':True,'repeat_identical_apk':True,'no_gradle_maven_adb':True,'final_apk':final['apk'],'final_report':str(O/'jvm-options-restored.json')}
(O/'compile-cache-e2e.json').write_text(json.dumps(report,indent=2)+'\n');print('MODULE_COMPILE_CACHE_E2E_OK: real SDK/Kotlin/Java/sign/DEX; compiler counters prove module hits/misses; body/dependency/class removal/resources/JDK/JVM options; damaged cache recovery; failed source preserves APK/cache; no Gradle')
