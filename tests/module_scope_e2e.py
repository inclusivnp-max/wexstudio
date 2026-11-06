#!/usr/bin/env python3
"""Real local Maven artifacts + SDK/compiler/K2 scope tests. No Gradle/network."""
from pathlib import Path
import json,os,shutil,subprocess,sys,zipfile,uuid
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True)
B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();P=O/'project'
if len(sys.argv)>2 and sys.argv[2] in ['siblings','exclusion','exclusion-positive']:
 mode=sys.argv[2]
 Q=O/('sibling-project' if mode=='siblings' else 'exclusion-project')
 if mode!='exclusion-positive':shutil.copytree(P,Q,ignore=shutil.ignore_patterns('.wex'))
 c=json.loads((Q/'wex.json').read_text());app=next(m for m in c['modules'] if m['name']=='app');shared=next(m for m in c['modules'] if m['name']=='shared')
 probe=Q/'shared/src/main/kotlin/dev/wex/shared/ScopeProbe.kt'
 if mode=='siblings':
  peer=Q/'peer/src/main/kotlin';peer.mkdir(parents=True);(peer/'PeerOnly.kt').write_text('package scope.peer\nclass PeerOnly { fun label(): String = "peer" }\n')
  c['modules'].append({'name':'peer','namespace':'scope.peer','sources':['peer/src/main/kotlin'],'dependencies':[]})
  app['projects'].insert(0,'peer');probe.write_text(probe.read_text()+'\nfun siblingLeak(): String = scope.peer.PeerOnly().label()\n')
 elif mode=='exclusion':
  shared['dependencies']=['scope.fixture:bridge:1'];app['dependencies'].append('scope.fixture:transitive:1')
  next(m for m in c['modules'] if m['name']=='unused')['dependencies'].append('scope.fixture:shared:1')
  probe.write_text('package dev.wex.shared\nimport scope.SharedApi\nimport scope.TransitiveApi\nfun scopePositive(): String = SharedApi.label()\nfun scopeForbidden(): String = TransitiveApi.label()\n')
 else:shared['dependencies'].append('scope.fixture:transitive:1')
 def build(label,expected):
  (Q/'wex.json').write_text(json.dumps(c,indent=2)+'\n')
  x=subprocess.run([str(B),'resolve',str(Q),'--offline'],capture_output=True,text=True,check=True);(O/(label+'-resolve.json')).write_text(x.stdout)
  x=subprocess.run([str(B),'build',str(Q),'--offline'],capture_output=True,text=True,timeout=180)
  (O/(label+'-build.log')).write_text(x.stderr);(O/(label+'-build.json')).write_text(x.stdout);(O/(label+'-build.exit')).write_text(str(x.returncode)+'\n')
  assert x.returncode==expected,(label,x.stderr[-4000:]);return x
 if mode=='siblings':
  x=build('sibling-negative',1);assert 'PeerOnly' in x.stderr
  shared['projects']=['peer'];build('sibling-positive',0)
  print('SIBLING_SCOPE_OK: earlier compiled sibling rejected until explicit project dependency is declared')
 elif mode=='exclusion':
  x=build('exclusion-negative',1);assert 'TransitiveApi' in x.stderr
  lock=json.loads((Q/'wex.lock.json').read_text())
  assert 'scope.fixture:transitive' not in lock['module_scopes']['shared']
  assert 'scope.fixture:transitive' in lock['module_scopes']['unused']
  assert any('scope.fixture:transitive:1' in a['dependencies'] for a in lock['artifacts'] if a['coordinate']['artifact']=='shared')
  print('EXCLUSION_SCOPE_OK: path-specific exclusion survives another module requiring the same artifact')
 else:build('exclusion-positive',0);print('EXCLUSION_EXPLICIT_OK: direct declaration restores transitive symbol despite another excluded path')
 sys.exit()
if len(sys.argv)>2 and sys.argv[2]=='positive':
 c=json.loads((P/'wex.json').read_text());next(m for m in c['modules'] if m['name']=='shared')['dependencies'].append('scope.fixture:app-only:1');(P/'wex.json').write_text(json.dumps(c,indent=2)+'\n')
 # Deliberately re-resolve through the own resolver before the locked offline build.
 x=subprocess.run([str(B),'resolve',str(P),'--offline'],capture_output=True,text=True,check=True);(O/'positive-resolve.json').write_text(x.stdout)
 x=subprocess.run([str(B),'build',str(P),'--offline'],capture_output=True,text=True,timeout=200);(O/'positive-build.json').write_text(x.stdout);(O/'positive-build.log').write_text(x.stderr);(O/'positive-build.exit').write_text(str(x.returncode)+'\n');assert x.returncode==0,x.stderr
 report=json.loads(x.stdout);lock=json.loads((P/'wex.lock.json').read_text());assert report['dependency_artifacts']<len(lock['artifacts'])
 with zipfile.ZipFile(report['apk']['path']) as z:
  assert 'assets/unused-scope.txt' not in z.namelist()
  dex=b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'))
  for name in [b'Lscope/AppOnly;',b'Lscope/TransitiveApi;']:assert name in dex,name
  assert b'Lscope/UnusedOnly;' not in dex
 sdk=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'))
 resources=subprocess.check_output([str(sdk/'build-tools/36.0.0/aapt2'),'dump','resources',report['apk']['path']],text=True)
 assert 'scope_unused_marker' not in resources
 (O/'packaged-resources.txt').write_text(resources)
 print('MODULE_BUILD_OK: explicit shared dependency compiles; AppOnly/TransitiveApi in DEX; unused AAR class/asset/resource excluded')
 sys.exit()
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True)
repo=O/'repo';repo.mkdir();java=Path('/usr/lib/jvm/java-21-openjdk/bin')
def artifact(name,clazz,deps=(),aar=False):
 work=O/name;work.mkdir();src=work/(clazz+'.java');src.write_text('package scope; public final class '+clazz+' { public static String label() { return "'+clazz+'"; } }')
 classes=work/'classes';classes.mkdir();subprocess.run([str(java/'javac'),'-g','--release','17','-d',str(classes),str(src)],check=True)
 jar=work/'classes.jar';subprocess.run([str(java/'jar'),'--create','--file',str(jar),'-C',str(classes),'.'],check=True)
 d=repo/'scope/fixture'/name/'1';d.mkdir(parents=True);typ='aar' if aar else 'jar'
 if aar:
  with zipfile.ZipFile(d/(name+'-1.aar'),'w') as z:
   z.write(jar,'classes.jar');z.writestr('AndroidManifest.xml','<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="scope.unused"><application><service android:name="scope.UnusedOnly" /></application></manifest>')
   z.writestr('res/values/strings.xml','<resources><string name="scope_unused_marker">unused</string></resources>');z.writestr('assets/unused-scope.txt','must not be packaged')
 else:shutil.copy2(jar,d/(name+'-1.jar'))
 dependencies=''.join('<dependency><groupId>scope.fixture</groupId><artifactId>'+dep+'</artifactId><version>1</version></dependency>' for dep in deps)
 (d/(name+'-1.pom')).write_text('<project><modelVersion>4.0.0</modelVersion><groupId>scope.fixture</groupId><artifactId>'+name+'</artifactId><version>1</version><packaging>'+typ+'</packaging><dependencies>'+dependencies+'</dependencies></project>')
artifact('app-only','AppOnly');artifact('shared','SharedApi',['transitive']);artifact('transitive','TransitiveApi');artifact('unused','UnusedOnly',aar=True)
artifact('bridge','BridgeApi',['shared'])
f=repo/'scope/fixture/bridge/1/bridge-1.pom';f.write_text(f.read_text().replace('</dependency>','<exclusions><exclusion><groupId>scope.fixture</groupId><artifactId>transitive</artifactId></exclusion></exclusions></dependency>'))

c=json.loads((P/'wex.json').read_text());c['repositories'].insert(0,repo.as_uri());c['android']['application_id']='dev.wex.scope.e2e.case'+uuid.uuid4().hex[:10]
app=next(m for m in c['modules'] if m['name']=='app');shared=next(m for m in c['modules'] if m['name']=='shared');app['dependencies'].append('scope.fixture:app-only:1');shared['dependencies'].append('scope.fixture:shared:1')
unused=P/'unused/src/main/kotlin';unused.mkdir(parents=True);(unused/'Unused.kt').write_text('package scope.unused\nclass Unused\n')
c['modules'].append({'name':'unused','namespace':'scope.unused.project','sources':['unused/src/main/kotlin'],'dependencies':['scope.fixture:unused:1']});(P/'wex.json').write_text(json.dumps(c,indent=2)+'\n')
probe=P/'shared/src/main/kotlin/dev/wex/shared/ScopeProbe.kt';probe.write_text('package dev.wex.shared\nimport scope.TransitiveApi\nimport scope.AppOnly\nfun scopePositive(): String = TransitiveApi.label()\nfun scopeForbidden(): String = AppOnly.label()\n')
main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';text=main.read_text();text=text.replace('val debug = BuildConfig.DEBUG','val scopeProof = dev.wex.shared.scopeForbidden()\n        Log.i("WexScope", "WEX_SCOPE_OK value=$scopeProof runId=${intent.getStringExtra("wex.run_id")}")\n        val debug = BuildConfig.DEBUG');main.write_text(text)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','curl']:
 f=traps/name;f.write_text('#!/bin/sh\necho '+name+' > '+str(marker)+'\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
x=subprocess.run([str(B),'resolve',str(P),'--offline'],capture_output=True,text=True,env=env,timeout=120);(O/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
(O/'resolve.json').write_text(x.stdout)
x=subprocess.run([str(B),'analyze',str(P),'--offline'],capture_output=True,text=True,env=env,timeout=160);(O/'negative-analysis.json').write_text(x.stdout);(O/'negative-analysis.log').write_text(x.stderr);assert x.returncode==0,x.stderr
model=json.loads(x.stdout);modules={m['name']:m for m in model['graph']['modules']};assert set(modules)=={':app',':shared',':unused'}
assert any('app-only-1.jar' in p for p in modules[':app']['classpath']);assert not any('app-only-1.jar' in p for p in modules[':shared']['classpath'])
assert any('transitive-1.jar' in p for p in modules[':shared']['classpath']);assert not any('/unused/' in p for key,m in modules.items() if key in [':app',':shared'] for p in m['classpath'])
headers=next(Path(p) for p in modules[':shared']['classpath'] if p.endswith('/symbols/shared.jar'))
with zipfile.ZipFile(headers) as z:
 assert 'dev/wex/shared/BuildConfig.class' in z.namelist();assert not any(p.startswith('dev/wex/android/') or p.startswith('scope/unused/') for p in z.namelist())
assert not (P/'.wex/native/debug/current.json').exists()
x=subprocess.run([str(B),'build',str(P),'--offline'],capture_output=True,text=True,env=env,timeout=180);(O/'negative-build.log').write_text(x.stderr);(O/'negative-build.exit').write_text(str(x.returncode)+'\n');assert x.returncode!=0 and 'AppOnly' in x.stderr,x.stderr
assert not (P/'.wex/native/debug/current.json').exists() and not marker.exists()
print('MODULE_SCOPE_NEGATIVE_OK: selected transitive classpath + namespace-scoped headers; parent-only dependency rejected by real Kotlin compiler; unused AAR excluded; no Gradle/network')
