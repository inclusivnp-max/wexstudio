#!/usr/bin/env python3
"""Real whole-workspace models without adding detached modules to application APK."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,uuid,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True)
P=O/'project';B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();MODE=sys.argv[2] if len(sys.argv)>2 else 'initial'
J=Path('/usr/lib/jvm/java-21-openjdk');REAL_SDK=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'))
def write(p,s):p.parent.mkdir(parents=True,exist_ok=True);p.write_text(s)
def call(label,*args,expected=0,env=None):
 x=subprocess.run([str(B),*map(str,args)],text=True,capture_output=True,env=env,timeout=300)
 (O/(label+'.log')).write_text(x.stderr);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.exit')).write_text(str(x.returncode)+'\n')
 assert x.returncode==expected,(label,x.returncode,x.stderr[-6000:]);return json.loads(x.stdout) if expected==0 else x
SDK=O/'sdk';JDK=O/'jdk';traps=O/'traps'
env={**os.environ,'ANDROID_HOME':str(SDK),'JAVA_HOME':str(JDK),'PATH':os.environ['PATH'] if os.environ['PATH'].startswith(str(traps)+os.pathsep) else str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
source=P/'feature/src/main/kotlin/dev/wex/feature/Feature.kt'
if MODE=='compose':
 Q=O/'compose-project';shutil.copytree(R/'tests/fixtures/compose-android',Q,ignore=shutil.ignore_patterns('.wex'))
 c=json.loads((Q/'wex.json').read_text());shared=next(m for m in c['modules'] if m['name']=='shared')
 c['modules'].append({'name':'detached','namespace':'dev.wex.detached','sources':['detached/src'],'projects':['shared'],'dependencies':shared['dependencies']})
 (Q/'wex.json').write_text(json.dumps(c,indent=2)+'\n')
 write(Q/'detached/src/Detached.kt','''package dev.wex.detached
import androidx.compose.runtime.Composable
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Text
@Composable
fun DetachedScene() {
    val colors = ButtonDefaults.buttonColors()
    Text("Detached")
}
''')
 call('detached-compose-resolve','resolve',Q,'--offline')
 print('WORKSPACE_COMPOSE_PREPARED: independent third module + actual Material3/Compose dependencies')
 sys.exit()
def text(positive):
 return '''package dev.wex.feature
import scope.feature.FeatureApi
fun workspacePositive(): String = FeatureApi.label()
fun leafResource(): Int = dev.wex.leaf.R.string.leaf_only
fun javaProof(): String = FeatureJava.decorate("source")
fun variantProof(): Boolean {
    val debug = BuildConfig.DEBUG
    return debug
}
fun workspaceForbidden(): Int = '''+('dev.wex.leaf.R.string.leaf_only' if positive else 'dev.wex.android.R.string.app_name')+'\n'
if MODE=='positive':
 source.write_text(text(True));m=call('body-recovery','analyze',P,'--offline',env=env);assert m['cache_hit'] and m['executed_generation_steps']==0
 # Application build must ignore detached source errors/resources/minSdk manifests.
 bad=P/'feature/src/main/res/values/broken.xml';bad.write_text('<resources><broken>')
 original=source.read_bytes();source.write_text('deliberately invalid detached Kotlin {{{')
 try:
  x=call('app-isolated-build','build',P,'--offline')
  with zipfile.ZipFile(x['apk']['path']) as z:
   dex=b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'))
   for marker in [b'Lscope/feature/FeatureApi;',b'Ldev/wex/feature/FeatureJava;']:assert marker not in dex
   assert 'assets/detached-only.txt' not in z.namelist()
  dump=subprocess.check_output([str(REAL_SDK/'build-tools/36.0.0/aapt2'),'dump','resources',x['apk']['path']],text=True)
  assert 'feature_only' not in dump and 'leaf_only' not in dump and 'feature_maven_only' not in dump
  (O/'packaged-resources.txt').write_text(dump)
 finally:source.write_bytes(original);bad.unlink()
 pointer=P/'.wex/native/debug/current.json';last_good=pointer.read_bytes();config=P/'wex.json';original_config=config.read_bytes()
 try:
  linked=json.loads(original_config);next(m for m in linked['modules'] if m['name']=='app')['projects'].append('feature');config.write_text(json.dumps(linked))
  call('linked-feature-resolve','resolve',P,'--offline')
  failed=call('linked-feature-min-sdk','build',P,'--offline',expected=1)
  assert 'requires minSdk' in failed.stderr and pointer.read_bytes()==last_good
 finally:
  config.write_bytes(original_config);call('app-config-restored','resolve',P,'--offline')
 (O/'workspace-application.json').write_text(json.dumps({'result':'WORKSPACE_APP_ISOLATED_OK','apk':x['apk'],'detached_source_errors_and_invalid_xml_do_not_block_app':True,'detached_classes_resources_assets_excluded':True,'linked_feature_app_min_sdk_rejected':True,'last_good_pointer_preserved':True},indent=2)+'\n')
 print('WORKSPACE_APP_ISOLATED_OK: own native signed APK; detached Kotlin/XML errors ignored by app build; detached Maven/classes/resources/assets excluded; linking high-minSdk feature still rejects actual APK; source/config restored')
 sys.exit()
assert MODE=='initial'
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True)
(SDK/'build-tools/36.0.0').mkdir(parents=True);(SDK/'platforms').symlink_to(REAL_SDK/'platforms',target_is_directory=True)
shutil.copy2(REAL_SDK/'build-tools/36.0.0/aapt2',SDK/'build-tools/36.0.0/aapt2')
(JDK/'bin').mkdir(parents=True)
for name in ['lib','conf','jmods','include','legal']:
 if (J/name).exists():(JDK/name).symlink_to(J/name,target_is_directory=True)
shutil.copy2(J/'release',JDK/'release')
for name in ['java','javac','jar','jmod','jlink']:shutil.copy2(J/'bin'/name,JDK/'bin'/name)
traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','curl']:
 f=traps/name;f.write_text('#!/bin/sh\necho '+name+' > '+str(marker)+'\nexit 99\n');f.chmod(0o755)
repo=O/'repo/scope/fixture/detached/1';repo.mkdir(parents=True)
lib=O/'library';write(lib/'FeatureApi.java','package scope.feature; public final class FeatureApi { public static String label() { return "detached"; } }')
(lib/'classes').mkdir();subprocess.run([str(J/'bin/javac'),'--release','17','-d',str(lib/'classes'),str(lib/'FeatureApi.java')],check=True)
with zipfile.ZipFile(repo/'detached-1.aar','w') as z:
 with zipfile.ZipFile(lib/'classes.jar','w') as j:j.write(lib/'classes/scope/feature/FeatureApi.class','scope/feature/FeatureApi.class')
 z.write(lib/'classes.jar','classes.jar');z.writestr('AndroidManifest.xml','<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="scope.feature"><uses-sdk android:minSdkVersion="31"/></manifest>')
 z.writestr('res/values/strings.xml','<resources><string name="feature_maven_only">detached-aar</string></resources>')
 z.writestr('assets/detached-only.txt','must never reach selected application')
(repo/'detached-1.pom').write_text('<project><modelVersion>4.0.0</modelVersion><groupId>scope.fixture</groupId><artifactId>detached</artifactId><version>1</version><packaging>aar</packaging></project>')
c=json.loads((P/'wex.json').read_text());c['repositories'].insert(0,(O/'repo').as_uri());c['android']['application_id']='dev.wex.workspace.e2e.case'+uuid.uuid4().hex[:10]
c['modules'].extend([
 {'name':'leaf','namespace':'dev.wex.leaf','sources':['leaf/src'],'resources':['leaf/res'],'manifest':'leaf/AndroidManifest.xml','dependencies':[]},
 {'name':'feature','namespace':'dev.wex.feature','sources':['feature/src/main/kotlin','feature/src/main/java'],'resources':['feature/src/main/res'],'manifest':'feature/AndroidManifest.xml','projects':['leaf'],'dependencies':['scope.fixture:detached:1']},
 {'name':'pure','sources':['pure/src'],'projects':['leaf'],'dependencies':[]}
]);(P/'wex.json').write_text(json.dumps(c,indent=2)+'\n')
write(P/'leaf/src/Leaf.kt','package dev.wex.leaf\nclass Leaf\n')
write(P/'leaf/res/values/strings.xml','<resources><string name="leaf_only">leaf</string></resources>')
write(P/'leaf/AndroidManifest.xml','<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="dev.wex.leaf"><uses-sdk android:minSdkVersion="28"/></manifest>')
write(P/'feature/AndroidManifest.xml','<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="dev.wex.feature"><uses-sdk android:minSdkVersion="34"/><application android:label="@string/feature_only"/></manifest>')
write(P/'feature/src/main/res/values/strings.xml','<resources><string name="feature_only">feature</string><string name="feature_alias">@string/leaf_only</string></resources>')
write(source,text(False));write(P/'feature/src/main/java/dev/wex/feature/FeatureJava.java','package dev.wex.feature; public class FeatureJava { public static String decorate(String value) { return "feature:"+value; } }')
write(P/'pure/src/Pure.kt','package dev.wex.pure\nfun pureLeaf(): Int = dev.wex.leaf.R.string.leaf_only\n')
call('resolve','resolve',P,'--offline',env=env)
m=call('initial','analyze',P,'--offline',env=env);assert not m['cache_hit'];assert m['graph']['model_api']=='wex-native-analysis-3' and m['model']['schema']==2
mods={x['name']:x for x in m['graph']['modules']};assert set(mods)=={':app',':shared',':leaf',':feature',':pure'}
cs=m['model']['contexts'];assert [x['root_module'] for x in cs]==['app','feature','pure'];assert [x['modules'] for x in cs]==[['shared','app'],['leaf','feature'],['pure']]
for name in [':feature',':leaf',':pure']:assert mods[name]['project_type']=='LIBRARY' and not mods[name]['application_id']
assert mods[':pure']['namespace']==''
assert mods[':feature']['module_dependencies']==[':leaf'] and mods[':pure']['module_dependencies']==[':leaf']
for name in [':app',':shared',':pure',':leaf']:assert not any('classes.jar' in p and '/aar/' in p and '/contexts/feature/' in p for p in mods[name]['classpath'])
assert any('/contexts/feature/aar/' in p and p.endswith('classes.jar') for p in mods[':feature']['classpath'])
def r_source(module,package):return (Path(mods[module]['generated_source_roots'][0])/package.replace('.','/')/'R.java').read_text()
assert 'feature_only' in r_source(':feature','dev.wex.feature') and 'leaf_only' in r_source(':leaf','dev.wex.leaf')
assert 'feature_only' not in r_source(':app','dev.wex.android') and 'app_name' not in r_source(':feature','dev.wex.feature')
for name in [':feature',':pure']:
 with zipfile.ZipFile(mods[name]['classpath'][-1]) as z:assert not any(n.startswith('dev/wex/android/') or n.startswith('dev/wex/analysis/') for n in z.namelist())
assert not list(Path(m['model']['generation']).rglob('*.apk')) and not (P/'.wex/native').exists()
programs=[SDK/'build-tools/36.0.0/aapt2',*list((JDK/'bin').iterdir())]
for p in programs:p.chmod(0o644)
try:
 h=call('execution-disabled-hit','analyze',P,'--offline',env=env);assert h['cache_hit'] and h['executed_generation_steps']==0 and h['model']['generation']==m['model']['generation']
finally:
 for p in programs:p.chmod(0o755)
# Coverage metadata must be validated even when graph/output bytes still match.
pointer=P/'.wex/analysis/debug/current.json';bad=json.loads(pointer.read_text());bad['contexts'][-1]['modules']=[];pointer.write_text(json.dumps(bad))
rebuilt=call('missing-context-rebuild','analyze',P,'--offline',env=env);assert not rebuilt['cache_hit'] and rebuilt['model']['generation']!=m['model']['generation']
prior=pointer.read_bytes();resource=P/'feature/src/main/res/values/strings.xml';before=resource.read_bytes();resource.write_text('<resources><broken>')
try:
 call('detached-resource-failure','analyze',P,'--offline',expected=1,env=env);assert pointer.read_bytes()==prior
finally:resource.write_bytes(before)
h=call('detached-resource-recovered','analyze',P,'--offline',env=env);assert h['cache_hit'] and h['model']['generation']==rebuilt['model']['generation']
release=call('release','analyze',P,'--variant','release','--offline',env=env)
assert [x['root_module'] for x in release['model']['contexts']]==['app','feature','pure']
feature=next(x for x in release['graph']['modules'] if x['name']==':feature');assert 'DEBUG = false' in (Path(feature['generated_source_roots'][0])/'dev/wex/feature/BuildConfig.java').read_text()
assert not (P/'.wex/native').exists() and not list((P/'.wex/analysis').rglob('*.apk')) and not marker.exists()
(O/'workspace-model.json').write_text(json.dumps({'result':'WORKSPACE_ANALYSIS_E2E_OK','modules':list(mods),'contexts':cs,'cache_hit_with_sdk_jdk_execution_disabled':True,'coverage_tamper_rebuild':True,'detached_resource_error_preserves_pointer':True,'debug_release_no_apk':True,'private_sdk':str(SDK),'private_jdk':str(JDK),'gradle_invoked':False},indent=2)+'\n')
print('WORKSPACE_ANALYSIS_E2E_OK: all five modules; separate app/feature/pure resource contexts; own scopes and detached minSdk; no APK/user compile/sign; execution-disabled cache hit; context-tamper rebuild; resource failure preserves pointer; debug/release')
