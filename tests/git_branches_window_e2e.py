#!/usr/bin/env python3
"""Native desktop local branch UX. No Gradle/K2/Android fixture build or emulator required."""
from pathlib import Path
from native_window_process import run_window
import argparse,hashlib,json,os,struct,subprocess
parser=argparse.ArgumentParser();parser.add_argument('output',type=Path);args=parser.parse_args();root=Path(__file__).resolve().parents[1];out=args.output.resolve();out.mkdir(parents=True,exist_ok=False);project=out/'project';project.mkdir();window=out/'window';window.mkdir()
ide=Path(os.environ.get('WEX_IDE',root/'target/debug/wex-ide')).resolve();env={**os.environ,'WEX_PROJECT_HISTORY':str(out/'history.json')};env.pop('WAYLAND_DISPLAY',None)
for k in list(env):
 if k.startswith('GIT_'):env.pop(k)
def git(*a):return subprocess.check_output(['git','-C',str(project),*a],env=env)
git('init','-q','--initial-branch=main');git('config','user.name','Wex Branch Fixture');git('config','user.email','fixture@localhost');git('config','commit.gpgsign','false');git('config','core.hooksPath',str(out/'empty-hooks'))
source=project/'Main.kt';source.write_text('fun main() = println("branch fixture")\n');before=source.read_bytes();git('add','.');git('commit','-qm','branch baseline');oid=git('rev-parse','HEAD')
code=run_window([ide,project,'--git-branches-smoke',window],env,out/'window.log',out/'process.json',150);(out/'window.exit').write_text(str(code)+'\n');assert code==0,(out/'window.log').read_text()[-6000:]
proof=json.loads((window/'git-branches-window.json').read_text());assert proof['result']=='GPU_GIT_BRANCHES_OK'and len(proof['screenshots'])==7 and proof['mutations']==3
for flag in ['dirty_switch_blocked','create_preserves_unsaved','selection_and_enter_never_switch','source_undo_preserved','explicit_review_then_switch','duplicate_error_refresh_recovery','no_save_build_deployment']:assert proof[flag],flag
assert source.read_bytes()==before and git('rev-parse','HEAD')==oid and git('symbolic-ref','--short','HEAD')==b'feature/live\n'and git('status','--porcelain')==b''
assert git('rev-parse','refs/heads/feature/recovered')==oid and not(project/'.git/index.lock').exists()and not(project/'.wex/native').exists()
assert json.loads((out/'process.json').read_text())['remaining']==[]
for shot in proof['screenshots']:
 data=Path(shot['path']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',data[16:24])==(shot['width'],shot['height'])
 for key,rect in shot['footer'].items():assert 0<=rect[0]<rect[2]<=shot['width']and 0<=rect[1]<rect[3]<=shot['height'],(key,rect,shot)
(out/'git-branches-e2e.json').write_text(json.dumps({'result':'NATIVE_GIT_BRANCHES_OK','binary':str(ide),'binary_sha256':hashlib.sha256(ide.read_bytes()).hexdigest(),'proof':proof,'source_sha256':hashlib.sha256(before).hexdigest(),'oid':oid.decode().strip(),'current':'feature/live','no_android_runtime_claim':True,'phone_untouched':True},indent=2)+'\n')
print('NATIVE_GIT_BRANCHES_OK: installed Git refs verified; create while dirty/no Save, source Undo, list keyboard review/no implicit switch, explicit reviewed switch, duplicate+minimum error footer+Refresh recovery;7 GPU captures; no Android runtime/phone')
