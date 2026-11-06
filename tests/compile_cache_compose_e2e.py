#!/usr/bin/env python3
"""Actual Compose lowering on cached module outputs and compiler flag invalidation."""
from pathlib import Path
import json,os,subprocess,shutil,sys,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);P=O/'project';B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve()
shutil.copytree(R/'tests/fixtures/compose-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True);f=P/'wex.json';cfg=json.loads(f.read_text());cfg['build']['compose']['reports']=False
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\necho '+name+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')};reports={}
def invoke(label,args):
 x=subprocess.run([str(B),*map(str,args)],env=env,text=True,capture_output=True,timeout=360);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert x.returncode==0,(label,x.stderr[-5000:]);return json.loads(x.stdout)
def resolve(label):f.write_text(json.dumps(cfg,indent=2)+'\n');invoke(label,['resolve',P,'--offline'])
def build(label,state):
 x=invoke(label,['build',P,'--offline']);assert {r['module']:r['state'] for r in x['compilation_cache']}=={'shared':state,'app':state};assert x['compose']['modules']==['shared','app'];assert x['compose']['reports']==cfg['build']['compose']['reports'];names=[s['name'] for s in x['steps']]
 for n in ['app','shared']:assert (f'Kotlin K2 · {n}' in names)==(state!='hit')
 javap=Path(os.environ.get('JAVA_HOME','/usr/lib/jvm/java-21-openjdk'))/'bin/javap';bytecode=subprocess.check_output([str(javap),'-classpath',str(Path(x['build_dir'])/'classes/shared'),'-c','-p','dev.wex.compose.shared.CounterCardKt'],text=True);(O/(label+'-bytecode.txt')).write_text(bytecode)
 assert 'Composer.startRestartGroup' in bytecode and ('ComposerKt.sourceInformation:' in bytecode)==cfg['build']['compose']['source_information']
 if state=='disabled':
  for n in ['app','shared']:assert (Path(x['build_dir'])/'compose'/n/'metrics'/(n+'-module.json')).is_file()
 with zipfile.ZipFile(x['apk']['path']) as z:
  dex=b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'));assert b'Ldev/wex/compose/shared/CounterCardKt;' in dex and b'Landroidx/compose/runtime/Composer;' in dex and b'Lorg/jetbrains/kotlin/cli/jvm/K2JVMCompiler;' not in dex
 assert not marker.exists();reports[label]={'cache':x['compilation_cache'],'compose':x['compose'],'blake3':x['blake3'],'apk':x['apk']};print('COMPOSE_CACHE_CASE_OK',label,state,flush=True);return x
resolve('resolve');a=build('cold','miss');b=build('repeat','hit');assert a['blake3']==b['blake3']
cfg['build']['compose']['source_information']=False;resolve('flags-resolve');a=build('flags-changed','miss');b=build('flags-repeat','hit');assert a['blake3']==b['blake3']
cfg['build']['compose']['reports']=True;resolve('reports-resolve');build('reports-uncached','disabled')
(O/'compose-cache-e2e.json').write_text(json.dumps({'result':'COMPOSE_MODULE_CACHE_OK','reports':reports,'actual_compose_lowering':True,'matching_plugin_and_metadata_retained':True,'flags_invalidate_cache':True,'reports_execute_compiler':True,'no_gradle_or_device':True},indent=2)+'\n');print('COMPOSE_MODULE_CACHE_OK: actual compiler lowering + cached Composer bytecode/metadata/DEX; source-info option invalidation; repeated signed APK identical; metrics explicitly bypass cache; no Gradle/device')
