#!/usr/bin/env python3
"""Real CLI metadata/process-lock tests + native folder/Recent windows and real K2 children."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,time,struct,fcntl
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_PROJECT_HISTORY':str(O/'ui-state/history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system')};env.pop('WAYLAND_DISPLAY',None);env.pop('WEX_REDUCED_MOTION',None);(O/'execution-env.json').write_text(json.dumps({k:env[k] for k in ['PATH','WEX_GRADLE_BRIDGE','WEX_PROJECT_HISTORY','WEX_K2_SYSTEM_DIR']},indent=2)+'\n')
def invoke(label,args,expected=0):
 x=subprocess.run([str(B),*map(str,args)],env=env,text=True,capture_output=True,timeout=90);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert x.returncode==expected,(label,x.stderr[-5000:]);return json.loads(x.stdout) if expected==0 else x
# Eight independent actual Wex processes concurrently merge one private metadata store.
roots=O/'parallel-projects';roots.mkdir();history=O/'parallel-state/history.json';procs=[]
created_roots=[]
for n in range(8):
 c=invoke(f'parallel-create-{n}',['new',roots,'--name',f'Parallel {n}','--directory',f'App{n}','--package',f'dev.wex.parallel{n}','--template','kotlin']);created_roots.append(c['root'])
history.parent.mkdir();gate=history.with_name(history.name+'.lock').open('w');fcntl.flock(gate,fcntl.LOCK_EX)
for root in created_roots:procs.append(subprocess.Popen([str(B),'recent','--history',str(history),'--add',root],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE))
time.sleep(.1);assert all(p.poll() is None for p in procs);(O/'parallel-gate.json').write_text(json.dumps({'all_eight_live_before_release':True,'pids':[p.pid for p in procs],'real_parent_flock_held':True},indent=2)+'\n');fcntl.flock(gate,fcntl.LOCK_UN);gate.close()
for n,p in enumerate(procs):
 out,err=p.communicate(timeout=20);(O/f'parallel-record-{n}.json').write_text(out);(O/f'parallel-record-{n}.log').write_text(err);(O/f'parallel-record-{n}.exit').write_text(str(p.returncode)+'\n');assert p.returncode==0,err
persisted=invoke('parallel-reload',['recent','--history',history]);assert len(persisted)==8 and len({p['root'] for p in persisted})==8
saved=history.read_bytes();history.write_text('broken');invoke('corrupt-load',['recent','--history',history],1);invoke('corrupt-record',['recent','--history',history,'--add',roots/'App0'],1);assert history.read_text()=='broken';history.write_bytes(saved);assert len(invoke('repaired-load',['recent','--history',history]))==8
# Folder cap is an explicit partial list, never a complete-list claim.
big=O/'large-folder';big.mkdir()
for n in range(4100):(big/f'd{n:04}').mkdir()
listing=invoke('large-listing',['browse',big]);assert listing['truncated'] and len(listing['folders'])==4096
files_only=O/'large-files';files_only.mkdir()
for n in range(20010):(files_only/f'f{n:05}').touch()
scan=invoke('large-file-scan',['browse',files_only]);assert scan['truncated'] and scan['scanned']==20000 and not scan['folders']
workspace=O/'workspace';workspace.mkdir();(workspace/'Проекты').mkdir();(workspace/'Alpha').mkdir();(workspace/'.hidden').mkdir();(workspace/'Very long folder name with spaces for native clipping').mkdir()
package='dev.wex.navigator.case'+str(time.time_ns());created=invoke('existing-create',['new',workspace/'Проекты','--name','Browse App','--directory','Existing App','--package',package,'--template','kotlin']);project=Path(created['root']);(workspace/'Alias').symlink_to(project,target_is_directory=True)
missing_created=invoke('missing-create',['new',workspace,'--name','Missing Project','--directory','Missing App','--package','dev.wex.missing','--template','kotlin']);missing=Path(missing_created['root']);invoke('seed-missing',['recent','--add',missing]);backup=O/'retained-missing';missing.rename(backup)
sources={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for root in [project,backup] for p in root.rglob('*') if p.is_file()};fixture={'workspace':str(workspace),'project':str(project),'missing':str(missing),'history':env['WEX_PROJECT_HISTORY']}
phases=[]
for name,reopen in [('first',False),('reopen',True)]:
 host=O/(name+'-host');host.mkdir();(host/'Host.kt').write_text('// Original navigator host\nval original = 7\n');out=O/name;out.mkdir();(out/'fixture.json').write_text(json.dumps(fixture,indent=2)+'\n');cmd=[str(IDE),str(host),'--project-nav-smoke',str(out)]+(['--reopen'] if reopen else []);
 with (O/(name+'-window.log')).open('w') as log:x=subprocess.run(cmd,env=env,text=True,stdout=log,stderr=subprocess.STDOUT,timeout=330)
 (O/(name+'-window.exit')).write_text(str(x.returncode)+'\n');assert x.returncode==0,(name,(O/(name+'-window.log')).read_text()[-6000:]);proof=json.loads((out/'navigation-window.json').read_text());assert proof['result']=='GUI_PROJECT_NAVIGATION_OK' and proof['reopened_process']==reopen and proof['dirty_buffer_preserved'] and proof['undo_restored'] and proof['files_not_deleted']
 assert (host/'Host.kt').read_text()=='// Original navigator host\nval original = 7\n'
 for shot in proof['screenshots']:
  data=Path(shot['path']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',data[16:24])==(shot['width'],shot['height'])
 for n in range(1,proof['launches']+1):
  p=json.loads((out/f'opened-{n}/proof.json').read_text());assert p['result']=='GENERATED_PROJECT_K2_READY' and p['source_unchanged'] and p['model']['phase']=='Applied' and not any(d['severity']==1 for d in p['diagnostics'])
 phases.append(proof);assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in sources.items());assert not marker.exists();print('PROJECT_NAVIGATION_PHASE_OK',name,'actual native pointers, persisted metadata, K2 children, source/Undo preserved',flush=True)
assert len(phases[0]['history'])==1 and phases[1]['history']==[];assert invoke('final-ui-history',['recent'])==[];assert project.joinpath('wex.json').is_file();assert not (project/'.wex/native').exists();assert not (project/'gradlew').exists();assert not marker.exists()
(O/'project-navigation-e2e.json').write_text(json.dumps({'result':'NATIVE_PROJECT_NAVIGATION_OK','created':created,'project':str(project),'package':package,'sources':sources,'parallel_process_records':persisted,'parallel_merge_count':8,'malformed_history_retained_and_repaired':True,'large_listing_count':len(listing['folders']),'large_listing_partial':listing['truncated'],'large_scan_count':scan['scanned'],'large_scan_partial':scan['truncated'],'phases':phases,'no_gradle_or_apk_or_device':True,'fixture':fixture},indent=2)+'\n');print('NATIVE_PROJECT_NAVIGATION_OK: 8 real concurrent Wex processes merged without loss; corrupt bytes retained/repaired; explicit 4096-folder/20000-entry partial caps; native Go/hidden/Unicode filter/Up/Use/Cancel; Recent reload across two IDE processes/missing recovery/removal without deleting files; three real K2 child windows; dirty buffers/Undo preserved; no Gradle/APK/device')
