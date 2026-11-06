#!/usr/bin/env python3
"""Real D8 caller/project PATH regression. Wrappers only record and exec actual SDK/Java."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,time,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();mode=sys.argv[2] if len(sys.argv)>2 else 'modified';assert mode in ['baseline','modified'];O.mkdir(parents=True,exist_ok=True);P=O/'project';CWD=O/'caller';CWD.mkdir();B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();S=Path.home()/'Android/Sdk';real=Path('/usr/lib/jvm/java-21-openjdk');SDK=O/'sdk';bt=SDK/'build-tools/36.0.0';bt.mkdir(parents=True);(SDK/'platforms').symlink_to(S/'platforms',target_is_directory=True)
for n in ['lib','lib64']:(bt/n).symlink_to(S/'build-tools/36.0.0'/n,target_is_directory=True)
original={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in [S/'build-tools/36.0.0/d8',S/'build-tools/36.0.0/lib/d8.jar',real/'bin/java']};(O/'original-tool-hashes.json').write_text(json.dumps(original,indent=2)+'\n');calls=O/'sdk-calls.jsonl';calls.touch();java_calls=O/'java-calls.jsonl';java_calls.touch()
for n in ['aapt2','d8','apksigner','zipalign']:shutil.copy2(S/'build-tools/36.0.0'/n,bt/n)
for n in ['aapt2','d8']:
 dst=bt/n;back=bt/(n+'.real');dst.rename(back);dst.write_text('#!/usr/bin/python3\nimport os,sys,json\nwith open('+repr(str(calls))+',"a") as f:f.write(json.dumps({"tool":'+repr(n)+',"cwd":os.getcwd(),"args":sys.argv[1:]})+"\\n")\nos.execv('+repr(str(back))+',['+repr(str(back))+']+sys.argv[1:])\n');dst.chmod(0o755)
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True);cfg=json.loads((P/'wex.json').read_text());cfg['android']['application_id']='dev.wex.dexpath.e2e.case'+str(time.time_ns());(P/'wex.json').write_text(json.dumps(cfg,indent=2)+'\n');main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';s=main.read_text();assert '        debugProbe(7)' in s;s=s.replace('        debugProbe(7)','        Log.i("WexRun", "WEX_PATH_DEX_OK runId=${intent.getStringExtra("wex.run_id")} java=$nativeProof resource=$sharedProof asset=$assetProof")\n        debugProbe(7)');main.write_text(s)
def jdk(name):
 j=O/name;(j/'bin').mkdir(parents=True)
 for n in ['lib','conf','jmods','include','legal']:
  if (real/n).exists():(j/n).symlink_to(real/n,target_is_directory=True)
 shutil.copy2(real/'release',j/'release')
 for n in ['java','javac','jar','jmod','jlink','keytool','javap']:shutil.copy2(real/'bin'/n,j/'bin'/n)
 return j
actual=jdk('project-runtime');decoy=jdk('caller-runtime');(P/'runtime-relative').symlink_to(actual,target_is_directory=True);(CWD/'runtime-relative').symlink_to(decoy,target_is_directory=True)
def launcher(tag,release=False):
 return '#!/usr/bin/python3\nimport os,sys,json\na=sys.argv[1:]\nis_d8="com.android.tools.r8.D8" in a\n'+('if is_d8:a=["--release" if x=="--debug" else x for x in a]\n' if release else '')+'with open('+repr(str(java_calls))+',"a") as f:f.write(json.dumps({"tag":'+repr(tag)+',"cwd":os.getcwd(),"d8":is_d8,"args":a})+"\\n")\nos.execv('+repr(str(real/'bin/java'))+',['+repr(str(real/'bin/java'))+']+a)\n'
(actual/'bin/java').write_text(launcher('project'));(actual/'bin/java').chmod(0o755);(decoy/'bin/java').write_text(launcher('caller-decoy'));(decoy/'bin/java').chmod(0o755);debug_bytes=(actual/'bin/java').read_bytes();release_bytes=launcher('project',True).encode()
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/n;p.write_text('#!/bin/sh\necho '+n+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
base_path=str(traps)+os.pathsep+os.environ['PATH'];env={**os.environ,'ANDROID_HOME':str(SDK),'JAVA_HOME':str(real),'PATH':'runtime-relative/bin'+os.pathsep+base_path,'WEX_GRADLE_BRIDGE':str(traps/'bridge')};(O/'execution-env.json').write_text(json.dumps({k:env[k] for k in ['ANDROID_HOME','JAVA_HOME','PATH','WEX_GRADLE_BRIDGE']},indent=2)+'\n');rows={}
def read(path):return [json.loads(s) for s in path.read_text().splitlines()]
def invoke(label,args):
 x=subprocess.run([str(B),*map(str,args)],cwd=CWD,env=env,text=True,capture_output=True,timeout=360);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert x.returncode==0,(label,x.stderr[-6000:]);assert not marker.exists();return json.loads(x.stdout)
def build(label,state,release=None):
 start=len(read(calls));jstart=len(read(java_calls));v=invoke(label,['build',P,'--offline']);sdk=read(calls)[start:];java=read(java_calls)[jstart:];d8=[c for c in sdk if c['tool']=='d8'];jvm=[c for c in java if c['d8']];r=v['dex_cache'];assert len(r)==1 and r[0]['state']==state,(label,r)
 assert len(d8)==len(jvm)==(state!='hit'),(label,d8,jvm);assert not any(c['tag']=='caller-decoy' for c in jvm),(label,jvm)
 assert all(c['cwd']==str(P) for c in sdk+java)
 if release is not None and jvm:assert ('--release' in jvm[0]['args'])==release and ('--debug' in jvm[0]['args'])!=release
 names=[s['name'] for s in v['steps']];assert ('D8 · JVM bytecode → DEX' in names)==(state!='hit')
 for n in ['AAPT2 link + generated R','APK sign · debug key','Verify APK signature','Verify ZIP alignment']:assert n in names
 assert any(c['tool']=='aapt2' and c['args'][0]=='link' for c in sdk)
 with zipfile.ZipFile(v['apk']['path']) as z:
  dex=b''.join(z.read(n) for n in sorted(z.namelist()) if n.endswith('.dex'));assert b'WEX_PATH_DEX_OK' in dex
 rows[label]={'report':v,'actual_sdk_calls':sdk,'actual_java_calls':java,'actual_d8_calls':len(d8),'dex_sha256':hashlib.sha256(dex).hexdigest()};print('DEX_PATH_CASE_OK',label,state,'actual D8',len(d8),'actual project Java',len(jvm),flush=True);return v
invoke('resolve',['resolve',P,'--offline']);cold=build('relative-cold','miss',False);build('relative-repeat','hit');assert rows['relative-cold']['dex_sha256']==rows['relative-repeat']['dex_sha256']
(actual/'bin/java').write_bytes(release_bytes)
if mode=='baseline':
 stale=build('actual-launcher-changed-stale-hit','hit');assert stale['dex_cache'][0]['key']==cold['dex_cache'][0]['key'];assert rows['actual-launcher-changed-stale-hit']['dex_sha256']==rows['relative-cold']['dex_sha256']
 entry=P/'.wex/dex-cache/d8-dex-v1'/cold['dex_cache'][0]['key'];assert entry.is_dir();quarantine=O/'quarantined-stale-entry';entry.rename(quarantine)
 forced=build('forced-real-release','miss',True);assert rows['forced-real-release']['dex_sha256']!=rows['relative-cold']['dex_sha256']
 (actual/'bin/java').write_bytes(debug_bytes);build('restored-launcher-stale-release-hit','hit');assert rows['restored-launcher-stale-release-hit']['dex_sha256']==rows['forced-real-release']['dex_sha256']
 result='BASELINE_RELATIVE_PATH_BUG_REPRODUCED'
else:
 changed=build('actual-launcher-release','miss',True);assert changed['dex_cache'][0]['key']!=cold['dex_cache'][0]['key'];assert rows['actual-launcher-release']['dex_sha256']!=rows['relative-cold']['dex_sha256'];build('actual-launcher-repeat','hit')
 (actual/'bin/java').write_bytes(debug_bytes);build('actual-launcher-restored','hit');assert rows['actual-launcher-restored']['dex_sha256']==rows['relative-cold']['dex_sha256']
 saved=(decoy/'release').read_bytes();(decoy/'release').write_bytes(saved+b'\n# changed irrelevant caller runtime\n');build('caller-decoy-changed','hit');(decoy/'release').write_bytes(saved)
 # Leading empty PATH entry means child's cwd, not parent's cwd. Both have java aliases.
 (P/'java').symlink_to(actual/'bin/java');(CWD/'java').symlink_to(decoy/'bin/java');env['PATH']=os.pathsep+base_path
 build('empty-cold','miss',False);build('empty-repeat','hit');(actual/'bin/java').write_bytes(release_bytes);build('empty-launcher-release','miss',True);(actual/'bin/java').write_bytes(debug_bytes);build('empty-launcher-restored','hit')
 blocked=P/'blocked/bin/java';blocked.parent.mkdir(parents=True);blocked.write_text('non-executable candidate');blocked.chmod(0o644);blocked_decoy=CWD/'blocked/bin/java';blocked_decoy.parent.mkdir(parents=True);blocked_decoy.symlink_to(decoy/'bin/java');env['PATH']='blocked/bin'+os.pathsep+'runtime-relative/bin'+os.pathsep+base_path
 build('nonexecuting-first-candidate','miss',False);build('nonexecuting-first-repeat','hit')
 # A relative JAVA_HOME binds caller's configured tool root once, while D8 PATH
 # still binds project's actual launcher. Absolute effective tool argv proves it.
 env['PATH']='runtime-relative/bin'+os.pathsep+base_path;env['JAVA_HOME']='runtime-relative'
 v=build('relative-java-home','miss',False)
 assert any(c['tag']=='caller-decoy' and not c['d8'] for c in rows['relative-java-home']['actual_java_calls'])
 assert all(Path(step['program']).is_absolute() for step in v['steps'])
 generated=next(step for step in v['steps'] if step['name']=='Javac generated R/BuildConfig');assert generated['program']==str(decoy/'bin/javac')
 build('relative-java-home-repeat','hit')
 env['WEX_K2_SYSTEM_DIR']=str(O/'k2-relative-system')
 k2=invoke('relative-java-home-k2',['k2-check',P]);assert k2['result']=='K2_ANDROID_OK' and k2['disk_source_unchanged']
 model=json.loads((P/'.wex/kotlin-workspace/workspace.json').read_text());assert model['sdks'][0]['homePath']==str(decoy.resolve())
 env.pop('WEX_K2_SYSTEM_DIR')
 env['JAVA_HOME']=str(real);build('absolute-java-home-restored','hit')
 # Unsupported runtime layout still executes real Java/D8 and never publishes a key.
 unsupported=P/'custom-bin/java';unsupported.parent.mkdir();unsupported.write_text(launcher('project-layout-unknown'));unsupported.chmod(0o755);env['PATH']='custom-bin'+os.pathsep+base_path
 cache=P/'.wex/dex-cache/d8-dex-v2';before={p.name for p in cache.iterdir() if p.is_dir()};build('unverifiable-runtime','disabled',False);build('unverifiable-runtime-repeat','disabled',False);assert {p.name for p in cache.iterdir() if p.is_dir()}==before
 env['PATH']='runtime-relative/bin'+os.pathsep+base_path;final=build('relative-final','hit');assert rows['relative-final']['dex_sha256']==rows['relative-cold']['dex_sha256'];result='DEX_PROJECT_PATH_E2E_OK'
assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in original.items())
(O/'dex-path-e2e.json').write_text(json.dumps({'result':result,'mode':mode,'project':str(P),'caller_cwd':str(CWD),'sdk':str(SDK),'actual_jdk':str(actual),'decoy_jdk':str(decoy),'rows':rows,'sdk_calls':str(calls),'java_calls':str(java_calls),'original_tools':original,'original_tools_unchanged':True,'no_caller_decoy_d8_invocations':True,'relative_java_home_k2':str(O/'relative-java-home-k2.json') if mode=='modified' else None,'final_apk':list(rows.values())[-1]['report']['apk']},indent=2)+'\n')
print(result+': actual SDK/Java argv + DEX-byte delta; caller/project cwd separated; actual launcher changes '+('expose old false hits' if mode=='baseline' else 'invalidate correctly; caller decoy ignored; empty/relative PATH and nonexecutable lookup; relative JAVA_HOME canonical tool root; unsupported runtime real uncached D8')+'; signed/aligned APK every time; no Gradle')
