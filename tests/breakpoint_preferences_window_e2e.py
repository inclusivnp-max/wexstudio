#!/usr/bin/env python3
"""Native M3 preference conflict/recovery, genuine K2, actual external writer process."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,struct,subprocess,sys,time,tempfile,fcntl
from native_window_process import run_window,focus_callback

def closing_locks(out,project):
    """Independent fd locks: no sleeps pretending that the saver/journal is busy."""
    source=project/'app/src/main/kotlin/dev/wex/android/BranchFeature.kt'
    journal=Path(os.environ['WEX_TOOLS_HOME'])/'editor-recovery/breakpoints'/hashlib.sha256(os.fsencode(project)).hexdigest()
    for kind,path in [('save',project/'.wex/editor-saves/locks'/(hashlib.sha256(os.fsencode(source)).hexdigest()+'.lock')),('journal',journal/'.journal.lock')]:
        until=time.monotonic()+240
        while not(out/(kind+'-lock.ready')).exists():
            assert time.monotonic()<until,kind+' readiness deadline';time.sleep(.03)
        path.parent.mkdir(parents=True,exist_ok=True)
        with path.open('a+') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            (out/(kind+'-lock.ack')).write_text('INDEPENDENT_FD_LOCK_HELD\n')
            print('OWNED_CLOSE_FLOCK_HELD kind='+kind+' pid='+str(os.getpid()),flush=True)
            while not(out/(kind+'-release.ready')).exists():
                assert time.monotonic()<until,kind+' release deadline';time.sleep(.03)
        (out/(kind+'-release.ack')).write_text('INDEPENDENT_FD_LOCK_RELEASED\n')
    print('ACTUAL_CLOSE_FLOCKS_OK count=2',flush=True)

def writer(out,project,closing=False):
    path=project/'app/src/main/kotlin/dev/wex/android/MainActivity.kt'
    text=path.read_text();line=next(i+1 for i,l in enumerate(text.splitlines()) if 'val answer = debugNested(doubled)' in l)
    prefs=project/'.wex/breakpoints.json'
    for stage in [1,2]:
        until=time.monotonic()+240
        while not (out/f'writer-{stage}.ready').exists():
            assert time.monotonic()<until,'real native writer checkpoint deadline';time.sleep(.03)
        before=prefs.read_bytes();p=json.loads(before)
        if stage==1:
            markers=p['files'][str(path)];assert len(markers)==1 and markers[0]['id']==1
            def relocated(marker,number,identity):
                m=dict(marker);m.update(id=identity,line=number)
                start=sum(len(l)for l in text.splitlines(True)[:number-1]);row=text.splitlines(True)[number-1];a=len(row)-len(row.lstrip());b=len(row.rstrip());m['span']=[start+a,start+b];return m
            base=markers[0];p['files'][str(path)]=[relocated(base,line-2,1),relocated(base,line+6,90)]
        else:p['files']={}
        after=json.dumps(p,indent=2).encode()
        with tempfile.NamedTemporaryFile(dir=prefs.parent,delete=False)as f:f.write(after);f.flush();os.fsync(f.fileno());name=f.name
        os.replace(name,prefs)
        (out/f'writer-{stage}.json').write_text(json.dumps({'independent_pid':os.getpid(),'before_sha256':hashlib.sha256(before).hexdigest(),'after_sha256':hashlib.sha256(after).hexdigest(),'after':p},indent=2)+'\n')
        (out/f'writer-{stage}.ack').write_text('EXTERNAL_WRITER_OK\n')
    print('EXTERNAL_PREFERENCE_WRITES_OK count=2',flush=True)
    if closing:closing_locks(out,project)

def late_writer(out,project):
    window=out/'recovery/window';meta=json.loads((out/'recovery/held-draft-meta.json').read_text());data=(out/'recovery/held-draft.json').read_bytes();assert hashlib.sha256(data).hexdigest()==meta['sha256']
    journal=Path(os.environ['WEX_TOOLS_HOME'])/'editor-recovery/breakpoints'/hashlib.sha256(os.fsencode(project)).hexdigest()
    until=time.monotonic()+240
    while not(window/'refresh.ready').exists():
        assert time.monotonic()<until,'real live recovery refresh checkpoint';time.sleep(.03)
    with (journal/'.journal.lock').open('r+')as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        with tempfile.NamedTemporaryFile(dir=journal,prefix='.pending-late-',suffix='.tmp',delete=False)as f:f.write(data);f.flush();os.fsync(f.fileno());stage=Path(f.name)
        try:os.link(stage,journal/meta['name'])
        finally:stage.unlink()
        fd=os.open(journal,os.O_RDONLY|os.O_DIRECTORY);os.fsync(fd);os.close(fd)
    assert (journal/meta['name']).read_bytes()==data
    (window/'refresh.ack').write_text('LATE_RECOVERY_PUBLISHED\n');print('LATE_RECOVERY_PUBLISHED sha256='+meta['sha256'],flush=True)

def main():
    p=argparse.ArgumentParser();p.add_argument('output',type=Path);p.add_argument('--phase',choices=['conflict','recovery','writer','recover-writer'],required=True);p.add_argument('--reduced',action='store_true');p.add_argument('--close-review',action='store_true');a=p.parse_args();out=a.output.resolve();root=Path(__file__).resolve().parents[1];project=out/'project'
    if a.phase in ['writer','recover-writer']:
        window=out/('conflict/window'if a.phase=='writer'else'recovery/window')
        try:
            if a.phase=='writer':writer(window,project,a.close_review)
            else:late_writer(out,project)
        except BaseException:
            import traceback
            (window/'writer.failure').write_text(traceback.format_exc());raise
        return
    if a.phase=='conflict':
        out.mkdir(parents=True,exist_ok=False);shutil.copytree(root/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex'))
        (project/'app/src/main/kotlin/dev/wex/android/BranchFeature.kt').write_text('package dev.wex.android\nclass BranchFeature { val message = "parked" }\n')
        original={str(f.relative_to(project)):hashlib.sha256(f.read_bytes()).hexdigest()for f in project.rglob('*')if f.is_file()};(out/'original.json').write_text(json.dumps(original,indent=2)+'\n');traps=out/'traps';traps.mkdir()
        for name in ['gradle','gradlew','mvn','adb','emulator']:
            f=traps/name;f.write_text('#!/bin/sh\nprintf '+name+' > "'+str(out/'FORBIDDEN_EXECUTED')+'"\nexit 99\n');f.chmod(0o755)
    else:assert(out/'conflict/proof.json').is_file()
    original=json.loads((out/'original.json').read_text());phase=out/a.phase;phase.mkdir();window=phase/'window';window.mkdir()
    binary=Path(os.environ.get('WEX_BIN',root/'target/debug/wex')).resolve();ide=Path(os.environ.get('WEX_IDE',root/'target/debug/wex-ide')).resolve()
    env={**os.environ,'WEX_TOOLS_HOME':str(root/'.wex'),'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','IJ_JAVA_OPTIONS':'-XX:ActiveProcessorCount=2 -Xmx1024m','WEX_PROJECT_HISTORY':str(out/'history.json'),'WEX_K2_SYSTEM_DIR':str(out/'k2-system'),'WEX_K2_SHUTDOWN_REPORT':str(phase/'shutdown.json'),'WEX_LSP_TRACE':str(phase/'lsp.jsonl'),'PATH':str(out/'traps')+os.pathsep+os.environ['PATH']}
    for k in ['WAYLAND_DISPLAY','WEX_BP_RECOVERY','WEX_REDUCED_MOTION']:env.pop(k,None)
    if a.reduced:env['WEX_REDUCED_MOTION']='1'
    if a.phase=='recovery':env['WEX_BP_RECOVERY']='1'
    journal=root/'.wex/editor-recovery/breakpoints'/hashlib.sha256(os.fsencode(project)).hexdigest()
    if a.phase=='conflict':
        assert not journal.exists(),'fresh isolated recovery namespace required'
        c=subprocess.run([binary,'resolve',project,'--offline'],env=env,capture_output=True,text=True,timeout=180);(out/'resolve.log').write_text(c.stdout+c.stderr);assert c.returncode==0,c.stderr
    if a.phase=='recovery':
        with (journal/'.journal.lock').open('r+')as lock:
            fcntl.flock(lock,fcntl.LOCK_EX);drafts=list(journal.glob('draft-*.json'));assert len(drafts)==1;draft=drafts[0];data=draft.read_bytes();(phase/'held-draft-meta.json').write_text(json.dumps({'name':draft.name,'sha256':hashlib.sha256(data).hexdigest()},indent=2)+'\n');draft.rename(phase/'held-draft.json')
    cmd=[ide,project,'--window-close-smoke'if a.close_review and a.phase=='conflict'else'--breakpoint-preferences-smoke',window];child=None
    try:
        child=subprocess.Popen([sys.executable,__file__,str(out),'--phase','writer'if a.phase=='conflict'else'recover-writer',*(['--close-review']if a.close_review else[])],stdout=(phase/'writer.log').open('w'),stderr=subprocess.STDOUT,env=env)
        code=run_window(cmd,env,phase/'window.log',phase/'process.json',300,on_start=focus_callback(cmd,phase,300));(phase/'window.exit').write_text(str(code)+'\n')
        assert code==(1 if a.phase=='conflict'else 0),(code,(phase/'window.log').read_text()[-13000:])
        if child:assert child.wait(timeout=5)==0
    finally:
        if child and child.poll()is None:child.terminate();child.wait(timeout=5)
    proof=json.loads((window/'preferences-window.json').read_text());assert proof['result']=='NATIVE_PREFERENCE_REVIEW_OK'and proof['recovery']==(a.phase=='recovery')and proof['reduced_motion']==a.reduced and proof['source_original_restored'];assert proof['result_counts_checked'];assert proof['late_recovery_refreshed']==(a.phase=='recovery');assert len(proof['screenshots'])==(4 if a.phase=='conflict'else 2);assert len(set(proof['minimum_frames']))>=4
    if a.close_review and a.phase=='conflict':
        close=json.loads((window/'closing-window.json').read_text());assert close['result']=='NATIVE_WINDOW_CLOSE_REVIEW_OK'and close['reduced_motion']==a.reduced
        for name in ['source_undo_and_original_bytes','cancel_preserved_text_ids_and_selection','actual_save_lock_cancel_receipts','actual_journal_lock_error_retry','discard_confirmation_cancel_kept_markers','modal_paste_shortcuts_ime_blocked']:assert close[name],name
        assert len(set(close['minimum_frames']))>=4 and len(set(close['failure_minimum_frames']))>=4 and len(close['screenshots'])==5
        for shot in close['screenshots']:
            b=Path(shot['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(760,560)
        log=(phase/'window.log').read_text();assert 'WINDOW_CLOSE_COMMITTED source_save=true discarded_markers=false backup='in log
        assert 'ACTUAL_CLOSE_FLOCKS_OK count=2'in(phase/'writer.log').read_text()
    for shot in proof['screenshots']:
        b=Path(shot['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(shot['width'],shot['height'])
    assert all(hashlib.sha256((project/p).read_bytes()).hexdigest()==h for p,h in original.items());assert not(out/'FORBIDDEN_EXECUTED').exists()and not(project/'.wex/native').exists();shutdown=json.loads((phase/'shutdown.json').read_text());assert shutdown=={'pending':0,'forced':0,'spawn_failed':0}
    messages=[json.loads(l.split(' ',1)[1])for l in(phase/'lsp.jsonl').read_text().splitlines()];assert any(m.get('method')=='textDocument/diagnostic'for m in messages)
    prefs=json.loads((project/'.wex/breakpoints.json').read_text())
    if a.phase=='conflict':
        assert 'preserved for explicit recovery'in(phase/'window.log').read_text();drafts=list(journal.glob('draft-*.json'));assert len(drafts)==1;draft=json.loads(drafts[0].read_text());assert draft['schema']==1 and draft['root']==str(project);assert sum(map(len,draft['local']['files'].values()))==3;assert prefs==json.loads((window/'writer-2.json').read_text())['after'];base={m['id']:m for ms in draft['base']['files'].values()for m in ms};external=json.loads((window/'writer-1.json').read_text())['after']['files'][str(project/'app/src/main/kotlin/dev/wex/android/MainActivity.kt')];assert base[1]['line']==external[0]['line']+1 and base[90]==external[1], 'actual merge must keep chosen local edit AND independent disk addition'
    else:assert [f.name for f in journal.iterdir()]==['.journal.lock'];assert sum(map(len,prefs['files'].values()))==3
    (phase/'proof.json').write_text(json.dumps({'result':'ACTUAL_NATIVE_PREFERENCE_RECOVERY_OK','phase':a.phase,'close_review':a.close_review,'expected_process_exit':code,'binary_sha256':hashlib.sha256(ide.read_bytes()).hexdigest(),'native':proof,'shutdown':shutdown,'no_APK_or_device':True,'original_bytes_restored':True,'journal':str(journal)},indent=2)+'\n');print('ACTUAL_NATIVE_PREFERENCE_RECOVERY_OK phase='+a.phase+' process_exit='+str(code)+' source_restored=true close_review='+str(a.close_review),flush=True)
if __name__=='__main__':main()
