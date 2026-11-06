#!/usr/bin/env python3
"""Create all native templates, then run real SDK builds and genuine Compose/Layoutlib."""
from pathlib import Path
import hashlib,json,os,subprocess,sys,time,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();projects=O/'projects';projects.mkdir();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')};(O/'execution-env.json').write_text(json.dumps({k:env[k] for k in ['PATH','WEX_GRADLE_BRIDGE']},indent=2)+'\n');rows={}
def invoke(label,args,expected=0):
 x=subprocess.run([str(B),*map(str,args)],env=env,text=True,capture_output=True,timeout=360);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert x.returncode==expected,(label,x.stderr[-6000:]);return json.loads(x.stdout) if expected==0 else x
for template in ['kotlin','java','compose']:
 package='dev.wex.generated.'+template+'.case'+str(time.time_ns());name='Мой & "App" \' %s @ ? \\';created=invoke(template+'-create',['new',projects,'--name',name,'--directory',template+' project','--package',package,'--template',template,'--min-sdk','23']);p=Path(created['root']);assert p.is_dir() and Path(created['entry']).is_file();assert not (p/'.wex').exists() and not (p/'wex.lock.json').exists();assert not list(p.rglob('*gradle*'));sources={str(f):hashlib.sha256(f.read_bytes()).hexdigest() for f in p.rglob('*') if f.is_file()}
 invoke(template+'-collision',['new',projects,'--name','Other','--directory',template+' project','--package','dev.wex.other'],expected=1);assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in sources.items())
 invoke(template+'-resolve',['resolve',p,'--offline']);a=invoke(template+'-cold',['build',p,'--offline']);b=invoke(template+'-warm',['build',p,'--offline']);assert a['backend']==b['backend']=='wex-native-v1';assert b['dex_cache'][0]['state']=='hit' and all(c['state']=='hit' for c in b['compilation_cache']);assert a['blake3']==b['blake3'];assert a['kotlin_files']==(0 if template=='java' else 1) and a['java_files']==(1 if template=='java' else 0)
 assert any(s['name']=='Verify APK signature' for s in a['steps']) and any(s['name']=='AAPT2 link + generated R' for s in b['steps'])
 with zipfile.ZipFile(b['apk']['path']) as z:
  dex=b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'));assert ('L'+package.replace('.','/')+'/MainActivity;').encode() in dex and b'WEX_RUN_OK' in dex
  if template=='compose':assert b'Landroidx/compose/runtime/Composer;' in dex and b'Lorg/jetbrains/kotlin/cli/jvm/K2JVMCompiler;' not in dex
 tools=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'))/'build-tools/36.0.0'
 dump=subprocess.check_output([str(tools/'aapt2'),'dump','resources',b['apk']['path']],text=True);(O/(template+'-resources.txt')).write_text(dump);assert 'Мой & \\"App\\"' in dump or 'Мой & "App"' in dump,dump[-2500:];assert '%s @ ?' in dump
 rows[template]={'created':created,'cold':a,'warm':b,'sources':sources,'actual_resource_dump':str(O/(template+'-resources.txt')),'package':package}
 assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in sources.items());assert not marker.exists();print('NATIVE_TEMPLATE_CASE_OK',template,'actual SDK signed APK; cold/warm DEX; escaped resource name; no Gradle',flush=True)
preview=invoke('compose-preview',['preview',rows['compose']['created']['root'],'--entry','App','--output',O/'compose-preview.png']);assert preview['result']=='PREVIEW_OK';assert not marker.exists();(O/'scaffold-e2e.json').write_text(json.dumps({'result':'NATIVE_PROJECT_TEMPLATES_OK','rows':rows,'preview':preview,'actual_sdk':True,'no_gradle_adb_or_device':True},indent=2)+'\n');print('NATIVE_PROJECT_TEMPLATES_OK: three actual generated Kotlin/Java/Compose apps; offline own resolver; six genuine signed/aligned APKs; warm DEX hits; escaped Unicode/quote/percent label via real AAPT; actual Compose lowering/Layoutlib Preview; source files unchanged; no Gradle/adb/device')
