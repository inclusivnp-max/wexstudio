#!/usr/bin/env python3
"""Actual AAPT/Javac native analysis before any APK, with an execution-disabled cache hit."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,zipfile
R=Path(__file__).resolve().parents[1]
O=Path(sys.argv[1] if len(sys.argv)>1 else R/'.verification/analysis-e2e').resolve();O.mkdir(parents=True,exist_ok=True)
P=O/'project';shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
(P/'wex.lock.json').unlink(missing_ok=True)
B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve()
# Private minimal SDK/JDK profile: deliberately contains no D8/signers/keytool.
SDK=O/'sdk';(SDK/'build-tools/36.0.0').mkdir(parents=True)
real_sdk=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'))
(SDK/'platforms').symlink_to(real_sdk/'platforms',target_is_directory=True)
shutil.copy2(real_sdk/'build-tools/36.0.0/aapt2',SDK/'build-tools/36.0.0/aapt2')
JDK=O/'jdk';(JDK/'bin').mkdir(parents=True)
real_jdk=Path('/usr/lib/jvm/java-21-openjdk')
for name in ['lib','conf','jmods','include','legal']:
 if (real_jdk/name).exists():(JDK/name).symlink_to(real_jdk/name,target_is_directory=True)
shutil.copy2(real_jdk/'release',JDK/'release')
for name in ['java','javac','jar','jmod','jlink']:shutil.copy2(real_jdk/'bin'/name,JDK/'bin'/name)
programs=[SDK/'build-tools/36.0.0/aapt2',*list((JDK/'bin').iterdir())]
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/name;f.write_text('#!/bin/sh\necho '+name+' > '+str(marker)+'\nexit 99\n');f.chmod(0o755)
env={**os.environ,'ANDROID_HOME':str(SDK),'JAVA_HOME':str(JDK),'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
results={}
def call(label,variant='debug',force=False,expected=0,action='analyze'):
 args=[str(B),action,str(P),'--variant',variant,'--offline']+(['--force'] if force else [])
 x=subprocess.run(args,env=env,text=True,capture_output=True,timeout=180)
 (O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n')
 assert x.returncode==expected,(label,x.returncode,x.stderr[-4000:])
 if expected:results[label]={'exit':x.returncode};return None
 value=json.loads(x.stdout);assert value['result']=='NATIVE_ANALYSIS_OK'
 results[label]={'exit':0,'cache_hit':value['cache_hit'],'generation':value['model']['generation'],'executed_generation_steps':value['executed_generation_steps'],'elapsed_ms':value['elapsed_ms']};return value
def no_apk():
 assert not (P/'.wex/native').exists()
 assert not list(P.rglob('*.apk')) and not list(P.rglob('*.keystore'))
 assert not marker.exists()
def build_config(x):
 paths=list(Path(x['model']['generation']).glob('module-generated/app/**/BuildConfig.java'));assert len(paths)==1;return paths[0]
def r_source(x):
 paths=list(Path(x['model']['generation']).glob('module-generated/app/**/R.java'));assert len(paths)==1;return paths[0]
first=call('initial');assert not first['cache_hit'] and first['executed_generation_steps']>0
assert first['graph']['model_api']=='wex-native-analysis-3' and first['graph']['selected_variant']=='debug'
assert any(m['name']==':app' and m['module_dependencies']==[':shared'] for m in first['graph']['modules'])
names=[s['name'] for s in first['model']['generation_steps']]
assert 'Javac generated R/BuildConfig' in names and any(n.startswith('Package scoped R/BuildConfig ·') for n in names)
assert not any('Kotlin K2' in n or n.startswith('Javac ·') or 'DEX' in n or 'sign' in n.lower() for n in names)
assert 'DEBUG = true' in build_config(first).read_text()
with zipfile.ZipFile(Path(first['model']['generation'])/'symbols/app.jar') as z:
 assert 'dev/wex/android/BuildConfig.class' in z.namelist() and 'dev/wex/android/R$string.class' in z.namelist()
 assert not any(n.endswith(('MainActivity.class','Greeter.class','JavaGreeting.class')) for n in z.namelist())
no_apk()
# Removing every execute bit changes metadata but NOT input bytes/fingerprint.
# Any attempted SDK/JDK process would fail; cached preparation must still succeed.
for p in programs:p.chmod(0o644)
try:
 hit=call('no-exec-hit');assert hit['cache_hit'] and hit['executed_generation_steps']==0
 assert hit['model']['generation']==first['model']['generation']
 kotlin=next(P.rglob('MainActivity.kt'));java=next(P.rglob('JavaGreeting.java'));k_before=kotlin.read_bytes();j_before=java.read_bytes()
 kotlin.write_text('this is deliberately invalid Kotlin {{{');java.write_text('deliberately invalid Java {{{')
 broken=call('source-errors-hit');assert broken['cache_hit'] and broken['model']['generation']==first['model']['generation']
 kotlin.write_bytes(k_before);java.write_bytes(j_before)
finally:
 for p in programs:p.chmod(0o755)
resource=P/'app/src/main/res/values/strings.xml';res_before=resource.read_text();valid=res_before.replace('</resources>','    <string name="analysis_added">Analysis before APK</string>\n</resources>');assert valid!=res_before
resource.write_text(valid);new=call('resource-change');assert not new['cache_hit'] and new['model']['generation']!=first['model']['generation'];assert 'analysis_added' in r_source(new).read_text()
# Returning to a previously analyzed tree must reuse its fully verified generation,
# not only the most recent pointer. Private tools have no execute permission here.
receipt=P/'.wex/analysis/debug/current.json'
for p in programs:p.chmod(0o644)
try:
 resource.write_text(res_before);returned=call('return-original-no-exec')
 assert returned['cache_hit'] and returned['executed_generation_steps']==0
 assert returned['model']['generation']==first['model']['generation']
 assert json.loads(receipt.read_text())==returned['model']
 assert 'analysis_added' not in r_source(returned).read_text()
 resource.write_text(valid);returned_new=call('return-changed-no-exec')
 assert returned_new['cache_hit'] and returned_new['executed_generation_steps']==0
 assert returned_new['model']['generation']==new['model']['generation']
 # A missing or malformed latest pointer is recoverable only through a valid,
 # content-addressed receipt, not a directory scan or a fabricated graph.
 receipt.unlink();missing_pointer=call('missing-current-no-exec')
 assert missing_pointer['cache_hit'] and missing_pointer['model']==new['model']
 receipt.write_text('{malformed');bad_pointer=call('malformed-current-no-exec')
 assert bad_pointer['cache_hit'] and bad_pointer['model']==new['model']
 assert json.loads(receipt.read_text())==new['model']
finally:
 for p in programs:p.chmod(0o755)
# An older indexed receipt must pass the SAME output inventory/hash validation.
build_config(first).write_text(build_config(first).read_text()+'\n// damaged historical generation\n')
resource.write_text(res_before);damaged_history=call('damaged-historical-generation')
assert not damaged_history['cache_hit'] and damaged_history['model']['generation']!=first['model']['generation']
resource.write_text(valid);history_recovered=call('history-after-damage')
assert history_recovered['cache_hit'] and history_recovered['model']['generation']==new['model']['generation']
receipt=P/'.wex/analysis/debug/current.json';prior=receipt.read_bytes()
resource.write_text('<resources><broken></resources>');call('broken-resource',expected=1);assert receipt.read_bytes()==prior
resource.write_text(valid);recovered=call('resource-recovered');assert recovered['cache_hit'] and recovered['model']['generation']==new['model']['generation']
build_config(recovered).write_text(build_config(recovered).read_text()+'\n// tampered generated source\n')
tamper=call('generated-tamper');assert not tamper['cache_hit']
extra=build_config(tamper).parent/'Extra.java';extra.write_text('class Extra {}')
extra_result=call('unrecorded-output');assert not extra_result['cache_hit']
(P/'.wex/analysis/models.lmdb').rename(P/'.wex/analysis/models.lmdb-previous')
missing=call('missing-lmdb');assert not missing['cache_hit']
debug_pointer=receipt.read_bytes();release=call('release','release');assert not release['cache_hit'];assert 'DEBUG = false' in build_config(release).read_text();assert receipt.read_bytes()==debug_pointer
release_hit=call('release-hit','release');assert release_hit['cache_hit'] and release_hit['executed_generation_steps']==0
forced=call('release-force','release',force=True);assert not forced['cache_hit'] and forced['model']['generation']!=release['model']['generation'];assert receipt.read_bytes()==debug_pointer
(P/'.wex/analysis/release/current.json').unlink()
for p in programs:p.chmod(0o644)
try:
 forced_index=call('release-forced-index-no-exec','release')
 assert forced_index['cache_hit'] and forced_index['executed_generation_steps']==0
 assert forced_index['model']['generation']==forced['model']['generation']
 assert receipt.read_bytes()==debug_pointer
finally:
 for p in programs:p.chmod(0o755)
sync=call('native-sync',action='sync');assert sync['cache_hit'] and sync['executed_generation_steps']==0
call('unsigned-release-apk','release',action='build',expected=1)
assert 'release requires build.release_signing' in (O/'unsigned-release-apk.log').read_text()
# Actual K2 checks can consume this fresh R field without changing the model.
kotlin.write_bytes(k_before.replace(b'val debug = BuildConfig.DEBUG',b'val freshResource = R.string.analysis_added\n        val debug = BuildConfig.DEBUG'))
no_apk()
(O/'analysis-e2e.json').write_text(json.dumps({'result':'ANALYSIS_E2E_OK','project':str(P),'cases':results,'source_errors_do_not_require_user_compilation':True,'cache_hit_with_sdk_jdk_execution_disabled':True,'apk_built':False,'signing_tools_present':False,'gradle_invoked':False},indent=2)+'\n')
print('ANALYSIS_E2E_OK: first-project R/BuildConfig; debug/release without signing tools; returning trees/missing-current/forced-generation reuse with SDK/JDK execution disabled; broken Kotlin/Java allowed; resource invalidation/error rollback; current+historical tamper/inventory/LMDB rebuild; no user compile/DEX/APK/Gradle')
