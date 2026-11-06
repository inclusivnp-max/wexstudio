#!/usr/bin/env python3
"""Independent actual SDK counters across private Compose/Layoutlib snapshots/catalog."""
from pathlib import Path
import json,os,shutil,subprocess,sys,struct
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);P=O/'project';B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();calls=O/'calls.jsonl';calls.touch();J=O/'jdk';(J/'bin').mkdir(parents=True);real=Path('/usr/lib/jvm/java-21-openjdk')
shutil.copytree(R/'tests/fixtures/compose-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True)
for n in ['lib','conf','jmods','include','legal']:
 if (real/n).exists():(J/n).symlink_to(real/n,target_is_directory=True)
shutil.copy2(real/'release',J/'release')
for n in ['java','javac','jar','jmod','jlink','keytool']:
 dst=J/'bin'/n;shutil.copy2(real/'bin'/n,dst)
 if n!='keytool':
  backing=dst.with_suffix('.real');dst.rename(backing);dst.write_text('#!/usr/bin/python3\nimport os,sys,json\nwith open('+repr(str(calls))+',"a") as f:f.write(json.dumps({"tool":'+repr(n)+',"args":sys.argv[1:]})+"\\n")\nos.execv('+repr(str(backing))+',['+repr(str(backing))+']+sys.argv[1:])\n');dst.chmod(0o755)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/n;p.write_text('#!/bin/sh\necho '+n+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
env={**os.environ,'JAVA_HOME':str(J),'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')};rows={}
def transforms(start):
 out=[]
 for line in calls.read_text().splitlines()[start:]:
  x=json.loads(line);t=x['tool'];a=x['args']
  if t in ['jmod','jlink'] or (t=='javac' and any(s.endswith('module-info.java') for s in a)) or (t=='jar' and '--update' in a and any(s.endswith('java-base.jar') for s in a)):out.append(t)
 return out

def invoke(label,args,state=None,ok=True):
 start=len(calls.read_text().splitlines());x=subprocess.run([str(B),'--adb','127.0.0.1:1',*map(str,args)],env=env,text=True,capture_output=True,timeout=300);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert (x.returncode==0)==ok,(label,x.stderr[-6000:]);seen=transforms(start)
 if not ok:assert not seen;return x
 if state is None:return x
 value=json.loads(x.stdout);(O/(label+'.json')).write_text(x.stdout);assert value['sdk_cache'][0]['state']==state and seen==([] if state=='hit' else ['javac','jar','jmod','jlink']),(label,value['sdk_cache'],seen)
 assert not (P/'.wex/native').exists() and not list((P/'.wex/preview-jobs').iterdir())
 if value['result']=='PREVIEW_OK':
  render=value['renderer'];assert render['result']=='HEADLESS_COMPOSE_LAYOUTLIB_OK' and render['compose_groups']>0 and render['resource_ids']>0 and not render['emulator_used'];b=Path(value['output']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',b[16:24])==(render['width'],render['height'])
 steps=value['compilation_steps'];assert any(s['name'].startswith('Kotlin K2 ·') for s in steps);assert not any(Path(s['program']).name in ['d8','apksigner','zipalign','adb'] for s in steps)
 rows[label]={'report':value,'actual_transform_calls':seen};print('PREVIEW_SDK_CASE_OK',label,state,seen,flush=True);return value
invoke('resolve',['resolve',P,'--offline']);invoke('prepare',['prepare-preview',P,'--offline'])
a=invoke('portrait',['preview',P,'--output',O/'portrait.png'],'miss')
f=P/'wex.preview.json';cfg=json.loads(f.read_text());cfg['entries'][0].update(dark=True,width_dp=640,height_dp=360);f.write_text(json.dumps(cfg,indent=2)+'\n')
b=invoke('night',['preview',P,'--output',O/'night.png'],'hit');assert a['sdk_cache'][0]['key']==b['sdk_cache'][0]['key'] and a['png_blake3']!=b['png_blake3']
# Actual javac --system locations must be different owned snapshots, not shared cache paths.
def imagepath(value):
 step=next(s for s in value['compilation_steps'] if s['name']=='Javac generated R/BuildConfig');args=step['args'];return args[args.index('--system')+1]
assert imagepath(a)!=imagepath(b) and '/preview-jobs/' in imagepath(a) and '/sdk-cache/' not in imagepath(a)
invoke('catalog',['preview-list',P],'hit');before=(O/'night.png').read_bytes();receipt=(O/'night.json').read_bytes();cfg['entries'][0]['composable']='dev.wex.compose.MainActivityKt.DoesNotExist';f.write_text(json.dumps(cfg,indent=2)+'\n');invoke('missing',['preview',P,'--output',O/'night.png'],ok=False);assert (O/'night.png').read_bytes()==before and (O/'night.json').read_bytes()==receipt
assert not marker.exists() and not list((P/'.wex/preview-jobs').iterdir())
(O/'sdk-preview-e2e.json').write_text(json.dumps({'result':'SDK_PREVIEW_CACHE_OK','rows':rows,'actual_private_image_paths':[imagepath(a),imagepath(b)],'independent_image_copies':True,'missing_preserves_last_good':True,'snapshot_jobs_cleaned':True,'no_apk_dex_sign_device':True,'counters':str(calls)},indent=2)+'\n');print('SDK_PREVIEW_CACHE_OK: actual Compose/Layoutlib portrait+night/catalog; independent tool counters prove one SDK transform then zero; owned image copies in distinct private snapshots; missing render keeps last-good; no APK/DEX/sign/device')
