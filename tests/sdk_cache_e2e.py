#!/usr/bin/env python3
"""Real SDK transform counters, shared analysis/APK image, damage/busy/input fences."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,time,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);P=O/'project';B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();real_sdk=Path.home()/'Android/Sdk';real_jdk=Path('/usr/lib/jvm/java-21-openjdk');calls=O/'tool-calls.jsonl';records={};calls.touch()
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True);f=P/'wex.json';cfg=json.loads(f.read_text());cfg['android']['application_id']='dev.wex.sdkcache.e2e.case'+str(time.time_ns());f.write_text(json.dumps(cfg,indent=2)+'\n')
SDK=O/'sdk';platform=SDK/'platforms/android-36';platform.mkdir(parents=True);shutil.copy2(real_sdk/'platforms/android-36/android.jar',platform/'android.jar');core=platform/'core-for-system-modules.jar';original_core=real_sdk/'platforms/android-36/core-for-system-modules.jar';original_hash=hashlib.sha256(original_core.read_bytes()).hexdigest();shutil.copy2(original_core,core)
bt=SDK/'build-tools/36.0.0';bt.mkdir(parents=True);(bt/'lib').symlink_to(real_sdk/'build-tools/36.0.0/lib',target_is_directory=True);(bt/'lib64').symlink_to(real_sdk/'build-tools/36.0.0/lib64',target_is_directory=True)
for n in ['aapt2','d8','apksigner','zipalign']:shutil.copy2(real_sdk/'build-tools/36.0.0'/n,bt/n)
J=O/'jdk';(J/'bin').mkdir(parents=True)
for n in ['lib','conf','jmods','include','legal']:
 if (real_jdk/n).exists():(J/n).symlink_to(real_jdk/n,target_is_directory=True)
shutil.copy2(real_jdk/'release',J/'release')
for n in ['java','javac','jar','jmod','jlink','keytool','javap']:
 dst=J/'bin'/n;shutil.copy2(real_jdk/'bin'/n,dst)
 if n!='keytool' and n!='javap':
  backing=dst.with_suffix('.real');dst.rename(backing);dst.write_text('#!/usr/bin/python3\nimport os,sys,json\nwith open('+repr(str(calls))+',"a") as f:f.write(json.dumps({"tool":'+repr(n)+',"args":sys.argv[1:]})+"\\n")\nos.execv('+repr(str(backing))+',['+repr(str(backing))+']+sys.argv[1:])\n');dst.chmod(0o755)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/n;p.write_text('#!/bin/sh\necho '+n+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
env={**os.environ,'ANDROID_HOME':str(SDK),'JAVA_HOME':str(J),'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
(O/'fixture.json').write_text(json.dumps({'project':str(P),'sdk':str(SDK),'jdk':str(J),'traps':str(traps),'calls':str(calls),'binary':str(B)},indent=2)+'\n')
def count():return len(calls.read_text().splitlines()) if calls.exists() else 0
def transforms(start):
 out=[]
 for line in calls.read_text().splitlines()[start:]:
  x=json.loads(line);t=x['tool'];a=x['args']
  if (t=='javac' and any(s.endswith('module-info.java') for s in a)) or (t=='jar' and '--update' in a and any(s.endswith('java-base.jar') for s in a)) or t in ['jmod','jlink']:out.append(t)
 return out

def invoke(label,args,expected=None,ok=True):
 start=count();x=subprocess.run([str(B),*map(str,args)],env=env,text=True,capture_output=True,timeout=300);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert (x.returncode==0)==ok,(label,x.stderr[-5000:]);seen=transforms(start)
 if not ok:assert not seen;return x
 value=json.loads(x.stdout)
 if expected is not None:
  sdk=value['sdk_cache'] if 'sdk_cache' in value else value['model']['generation_sdk_cache'];assert len(sdk)==1 and sdk[0]['state']==expected,(label,sdk)
  assert seen==([] if expected=='hit' else ['javac','jar','jmod','jlink']),(label,seen)
  steps=value.get('steps',value.get('model',{}).get('generation_steps',[]));assert any(s['name']=='Android Java system image' for s in steps)==(expected!='hit')
  if 'apk' in value:assert any(s['name']=='D8 · JVM bytecode → DEX' for s in steps)==(value['dex_cache'][0]['state']!='hit') and any(s['name']=='Verify APK signature' for s in steps)
  records[label]={'sdk':sdk,'transform_calls':seen,'value':value}
  print('SDK_CACHE_CASE_OK',label,expected,'actual transform calls',seen,flush=True)
 assert not marker.exists();return value
invoke('resolve',['resolve',P,'--offline'])
a=invoke('analysis-cold',['analyze',P,'--offline'],'miss');assert not (P/'.wex/native').exists()
b=invoke('analysis-release',['analyze',P,'--offline','--variant','release'],'hit');assert 'DEBUG = false' in next(Path(b['model']['generation']).glob('module-generated/app/**/BuildConfig.java')).read_text()
a=invoke('apk-after-analysis',['build',P,'--offline'],'hit');b=invoke('apk-repeat',['build',P,'--offline'],'hit');assert a['blake3']==b['blake3'] and all(s['state']=='hit' for s in b['compilation_cache'])
key=b['sdk_cache'][0]['key'];cache=P/'.wex/sdk-cache/android-java-image-v1'/key;target=cache/'image/lib/modules';target.write_bytes(target.read_bytes()+b'PRIVATE_IMAGE_TAMPER')
tamper=invoke('image-tamper',['build',P,'--offline'],'miss');assert 'Invalid SDK image' in tamper['sdk_cache'][0]['detail'] and all(s['state']=='hit' for s in tamper['compilation_cache'])
(cache/'image/lib/unrecorded.bin').write_bytes(b'PRIVATE_EXTRA');invoke('image-unrecorded',['analyze',P,'--offline','--force'],'miss')
release=J/'release';saved=release.read_bytes();release.write_bytes(saved+b'\n');invoke('jdk-changed',['build',P,'--offline'],'miss');release.write_bytes(saved);invoke('jdk-restored',['build',P,'--offline'],'hit')
old_options=env.get('JAVA_TOOL_OPTIONS');env['JAVA_TOOL_OPTIONS']='-Dwex.sdk.cache.fixture=changed';flags=invoke('options-model-change',['analyze',P,'--offline'],'miss');assert not flags['cache_hit'] and flags['model']['fingerprint']!=records['analysis-cold']['value']['model']['fingerprint']
if old_options is None:env.pop('JAVA_TOOL_OPTIONS')
else:env['JAVA_TOOL_OPTIONS']=old_options
invoke('options-restored',['build',P,'--offline'],'hit')
lock=cache.parent/(key+'.lock');receipt=(cache/'receipt.json').read_bytes();locker=subprocess.Popen(['/usr/bin/python3','-c',"import fcntl,sys; f=open(sys.argv[1],'r+'); fcntl.flock(f,fcntl.LOCK_EX); print('LOCKED',flush=True); sys.stdin.read()",str(lock)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True);assert locker.stdout.readline().strip()=='LOCKED'
try:invoke('busy-independent',['analyze',P,'--offline','--force'],'disabled');assert (cache/'receipt.json').read_bytes()==receipt
finally:locker.communicate('',timeout=10)
assert locker.returncode==0;invoke('busy-released',['analyze',P,'--offline','--force'],'hit')
# Actual SDK bytes change only in the private core copy; ZIP comment changes key,
# while leaving platform classes and original SDK intact.
core_before=core.read_bytes()
with zipfile.ZipFile(core,'a') as z:z.comment=b'WEX_PRIVATE_CORE_CACHE_INPUT'
changed=invoke('sdk-core-changed',['analyze',P,'--offline'],'miss');assert not changed['cache_hit'];core.write_bytes(core_before);invoke('sdk-core-restored',['build',P,'--offline'],'hit')
pointer=P/'.wex/analysis/debug/current.json';prior=pointer.read_bytes();apk_pointer=(P/'.wex/native/debug/current.json').read_bytes();resource=P/'app/src/main/res/values/strings.xml';saved=resource.read_bytes();resource.write_text('<resources><broken>')
try:invoke('invalid-resource',['analyze',P,'--offline'],ok=False);assert pointer.read_bytes()==prior and (P/'.wex/native/debug/current.json').read_bytes()==apk_pointer
finally:resource.write_bytes(saved)
invoke('resource-repaired',['analyze',P,'--offline','--force'],'hit')
assert hashlib.sha256(original_core.read_bytes()).hexdigest()==original_hash and not marker.exists()
(O/'sdk-cache-e2e.json').write_text(json.dumps({'result':'SDK_IMAGE_CACHE_E2E_OK','records':records,'fixture':str(O/'fixture.json'),'original_sdk_unchanged':True,'real_cross_process_busy_fallback':True,'failed_resource_preserves_model_apk':True,'sdk_jdk_env_invalidation':True,'analysis_release_apk_share_image':True,'no_gradle_or_device':True},indent=2)+'\n');print('SDK_IMAGE_CACHE_E2E_OK: actual descriptor/jar/jmod/jlink counters; analysis/debug/release/APK image reuse; tamper/extra/JDK/env/core invalidation; real OS-lock busy independent generation; errors keep prior model/APK; original SDK unchanged')
