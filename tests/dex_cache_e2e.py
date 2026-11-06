#!/usr/bin/env python3
"""Actual D8 launch counters, APK bytes and failure isolation; no fake tool output."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,time,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);P=O/'project';B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();S=Path.home()/'Android/Sdk';SDK=O/'sdk';bt=SDK/'build-tools/36.0.0';bt.mkdir(parents=True);(SDK/'platforms').symlink_to(S/'platforms',target_is_directory=True)
shutil.copytree(S/'build-tools/36.0.0/lib',bt/'lib');(bt/'lib64').symlink_to(S/'build-tools/36.0.0/lib64',target_is_directory=True)
for n in ['aapt2','d8','apksigner','zipalign']:shutil.copy2(S/'build-tools/36.0.0'/n,bt/n)
original={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in [S/'build-tools/36.0.0/d8',S/'build-tools/36.0.0/lib/d8.jar']}
calls=O/'calls.jsonl';calls.touch()
for n in ['d8','aapt2']:
 wrapper=bt/n;backing=bt/(n+'.real');wrapper.rename(backing);wrapper.write_text('#!/usr/bin/python3\nimport os,sys,json\nwith open('+repr(str(calls))+',"a") as f:f.write(json.dumps({"tool":'+repr(n)+',"args":sys.argv[1:]})+"\\n")\nos.execv('+repr(str(backing))+',['+repr(str(backing))+']+sys.argv[1:])\n');wrapper.chmod(0o755)
J=O/'jdk';(J/'bin').mkdir(parents=True);real=Path('/usr/lib/jvm/java-21-openjdk')
for n in ['lib','conf','jmods','include','legal']:
 if (real/n).exists():(J/n).symlink_to(real/n,target_is_directory=True)
shutil.copy2(real/'release',J/'release')
for n in ['java','javac','jar','jmod','jlink','keytool','javap']:shutil.copy2(real/'bin'/n,J/'bin'/n)
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True);f=P/'wex.json';cfg=json.loads(f.read_text());cfg['android']['application_id']='dev.wex.dexcache.e2e.case'+str(time.time_ns());f.write_text(json.dumps(cfg,indent=2)+'\n')
main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';s=main.read_text();s=s.replace('        debugProbe(7)','        Log.i("WexRun", "WEX_DEX_CACHE_OK runId=${intent.getStringExtra("wex.run_id")} java=$nativeProof resource=$sharedProof asset=$assetProof")\n        debugProbe(7)');main.write_text(s)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/n;p.write_text('#!/bin/sh\necho '+n+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
env={**os.environ,'ANDROID_HOME':str(SDK),'JAVA_HOME':str(J),'PATH':str(traps)+os.pathsep+str(J/'bin')+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')};cases={}
def count():return len(calls.read_text().splitlines())
def invoke(label,args,ok=True):
 start=count();x=subprocess.run([str(B),*map(str,args)],env=env,text=True,capture_output=True,timeout=360);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert (x.returncode==0)==ok,(label,x.stderr[-6000:]);launched=[json.loads(s) for s in calls.read_text().splitlines()[start:]];assert not marker.exists();return x,launched
def build(label,want):
 x,launched=invoke(label,['build',P,'--offline']);v=json.loads(x.stdout);d=v['dex_cache'];assert len(d)==1 and d[0]['state']==want,(label,d)
 d8=[c for c in launched if c['tool']=='d8'];assert len(d8)==(want!='hit'),(label,d8)
 names=[s['name'] for s in v['steps']];assert ('D8 · JVM bytecode → DEX' in names)==(want!='hit')
 assert any(c['tool']=='aapt2' and c['args'][0]=='link' for c in launched)
 for name in ['AAPT2 link + generated R','APK sign · debug key','Verify APK signature','Verify ZIP alignment']:assert name in names
 assert all(s['exit_code']==0 for s in v['steps'])
 with zipfile.ZipFile(v['apk']['path']) as z:
  dex=b''.join(z.read(n) for n in sorted(z.namelist()) if n.endswith('.dex'));assert b'WEX_DEX_CACHE_OK' in dex
 cases[label]={'state':want,'actual_d8_calls':d8,'actual_aapt_links':sum(c['tool']=='aapt2' and c['args'][0]=='link' for c in launched),'report':v,'dex_sha256':hashlib.sha256(dex).hexdigest()};print('DEX_CACHE_CASE_OK',label,want,'real D8',len(d8),flush=True);return v
invoke('resolve',['resolve',P,'--offline']);cold=build('cold','miss');repeat=build('repeat','hit');assert cold['blake3']==repeat['blake3']
resource=P/'shared/src/main/res/values/strings.xml';resource.write_text(resource.read_text().replace('</string>',' WEX_DEX_RESOURCE_V2</string>',1));value=build('resource-only','hit');assert value['blake3']!=cold['blake3'] and cases['resource-only']['dex_sha256']==cases['cold']['dex_sha256']
asset=P/'app/src/main/assets/wex-native-proof.txt';asset.write_text('WEX_DEX_ASSET_V2\n');assets=build('asset-only','hit');assert assets['blake3']!=value['blake3']
java=P/'app/src/main/java/dev/wex/android/JavaGreeting.java';java.write_text(java.read_text().replace('Java + Kotlin: ','WEX_DEX_APP_V2: '));build('app-bytecode','miss')
shared=P/'shared/src/main/kotlin/dev/wex/shared/Greeter.kt';shared.write_text(shared.read_text().replace('Hello,','WEX_DEX_DEP_V2,'));build('dependency-bytecode','miss')
ghost=shared.with_name('Ghost.kt');ghost.write_text('package dev.wex.shared\nclass Ghost { fun unused(): Int = 99 }\n');build('new-class','miss');ghost.unlink();removed=build('removed-class','hit')
with zipfile.ZipFile(removed['apk']['path']) as z:assert b'Ldev/wex/shared/Ghost;' not in z.read('classes.dex')
cache=P/'.wex/dex-cache/d8-dex-v2';key=removed['dex_cache'][0]['key'];entry=cache/key;target=entry/'dex/classes.dex';target.write_bytes(target.read_bytes()+b'PRIVATE_CORRUPTION');build('tampered-dex','miss')
(entry/'dex/classes2.dex').write_bytes(target.read_bytes());build('unrecorded-dex','miss');(entry/'receipt.json').write_text('{}');repaired=build('malformed-receipt','miss')
receipt=(entry/'receipt.json').read_bytes();before={str(p.relative_to(entry)):hashlib.sha256(p.read_bytes()).hexdigest() for p in entry.rglob('*') if p.is_file()}
locker=subprocess.Popen(['/usr/bin/python3','-c',"import sys,fcntl; f=open(sys.argv[1],'r+'); fcntl.flock(f,fcntl.LOCK_EX); print('LOCKED',flush=True); sys.stdin.read()",str(cache/(key+'.lock'))],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True);assert locker.stdout.readline().strip()=='LOCKED'
try:
 build('busy-key','disabled');assert {str(p.relative_to(entry)):hashlib.sha256(p.read_bytes()).hexdigest() for p in entry.rglob('*') if p.is_file()}==before
finally:locker.communicate('',timeout=10)
build('lock-released','hit')
for label,path in [('d8-wrapper',bt/'d8'),('d8-backing',bt/'d8.real'),('d8-jar',bt/'lib/d8.jar'),('jdk-release',J/'release')]:
 saved=path.read_bytes();path.write_bytes(saved+b'\n# private verified cache input change\n');build(label+'-changed','miss');path.write_bytes(saved);build(label+'-restored','hit')
shell=O/'shell-env.sh';shell.write_text('export WEX_DEX_SHELL_PROBE=1\n');env['BASH_ENV']=str(shell);build('dynamic-shell-uncached','disabled');env.pop('BASH_ENV');build('dynamic-shell-restored','hit')
env['JAVA_TOOL_OPTIONS']='-Dwex.dex.cache.fixture=changed';build('jvm-option-changed','miss');env.pop('JAVA_TOOL_OPTIONS');build('jvm-option-restored','hit')
cfg['android']['min_sdk']=27;f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('min-api-resolve',['resolve',P,'--offline']);build('min-api-changed','miss');cfg['android']['min_sdk']=26;f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('min-api-restore-resolve',['resolve',P,'--offline']);last=build('min-api-restored','hit')
# Real D8 duplicate-type rejection, not a wrapper-generated error.
pointer=P/'.wex/native/debug/current.json';prior=pointer.read_bytes();existing={p.name for p in cache.iterdir() if p.is_dir()};repo=O/'repo';artifact=repo/'dev/wex/dexduplicate/1';artifact.mkdir(parents=True)
with zipfile.ZipFile(Path(last['build_dir'])/'module-1.jar') as z:
 name='dev/wex/shared/Greeter.class';body=z.read(name)
with zipfile.ZipFile(artifact/'dexduplicate-1.jar','w') as z:z.writestr(name,body)
(artifact/'dexduplicate-1.pom').write_text('<project><modelVersion>4.0.0</modelVersion><groupId>dev.wex</groupId><artifactId>dexduplicate</artifactId><version>1</version></project>')
cfg['repositories'].insert(0,repo.as_uri());cfg['modules'][0]['dependencies'].append('dev.wex:dexduplicate:1');f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('duplicate-resolve',['resolve',P,'--offline'])
try:
 x,launched=invoke('duplicate-d8-failure',['build',P,'--offline'],False);assert 'D8 · JVM bytecode' in x.stderr and ('defined multiple times' in x.stderr or 'Duplicate' in x.stderr);assert sum(c['tool']=='d8' for c in launched)==1;assert pointer.read_bytes()==prior and {p.name for p in cache.iterdir() if p.is_dir()}==existing
finally:
 cfg['repositories'].pop(0);cfg['modules'][0]['dependencies'].pop();f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('duplicate-restore-resolve',['resolve',P,'--offline'])
final=build('failure-recovered','hit')
prior=pointer.read_bytes() # Recovery publishes a fresh build directory/pointer.
# No signing credentials read during Debug/analysis, but packaged key metadata fails
# before SDK execution and creates no resource/DEX snapshots.
cfg['build']['release_signing']=dict(keystore='app/src/main/res/raw/private.jks',key_alias='release',store_password_env='WEX_MISSING_STORE',key_password_env='WEX_MISSING_KEY',certificate_sha256='a'*64);f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('key-resolve',['resolve',P,'--offline'])
for command in ['build','analyze']:
 x,launched=invoke('packaged-key-'+command,[command,P,'--offline'],False);assert 'would be packaged' in x.stderr and not launched and pointer.read_bytes()==prior
cfg['build']['release_signing']['keystore']='outside/missing.jks';f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('missing-key-resolve',['resolve',P,'--offline']);build('external-missing-key-debug','hit');x,launched=invoke('external-missing-key-analysis',['analyze',P,'--offline']);assert json.loads(x.stdout)['model']
cfg['build'].pop('release_signing');f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('final-resolve',['resolve',P,'--offline']);final=build('final','hit')
with zipfile.ZipFile(final['apk']['path']) as z:
 dex=z.read('classes.dex');assert all(x in dex for x in [b'WEX_DEX_CACHE_OK',b'WEX_DEX_APP_V2',b'WEX_DEX_DEP_V2']);assert z.read('assets/wex-native-proof.txt')==b'WEX_DEX_ASSET_V2\n'
dump=subprocess.check_output([str(bt/'aapt2'),'dump','resources',final['apk']['path']],env=env,text=True);assert 'WEX_DEX_RESOURCE_V2' in dump;(O/'final-resources.txt').write_text(dump)
assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in original.items()) and not marker.exists()
(O/'dex-cache-e2e.json').write_text(json.dumps({'result':'DEX_CACHE_E2E_OK','project':str(P),'sdk':str(SDK),'jdk':str(J),'traps':str(traps),'cases':cases,'actual_counters':str(calls),'real_duplicate_d8_failure_keeps_apk_and_cache':True,'resource_and_asset_only_reuse_dex':True,'original_sdk_hashes':original,'original_sdk_unchanged':True,'final_apk':final['apk']},indent=2)+'\n');print('DEX_CACHE_E2E_OK: actual D8 counters; warm/resource/asset zero; source/dependency/newclass rebuild; deletion removes class; corrupt/extra/receipt repair; OS-lock independent D8; tool/backing/JAR/JDK/options/minAPI; real duplicate-type failure preserves APK/cache; key packaging guard; no Gradle')
