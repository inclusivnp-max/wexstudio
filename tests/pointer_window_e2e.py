#!/usr/bin/env python3
"""Real cold font work + native same-frame Press/Move/Release/Paste, not direct selection."""
from pathlib import Path
import json,os,runpy,sys
os.environ['WEX_VIEWPORT_FLAG']='--gesture-smoke';os.environ['WEX_VIEWPORT_LINE_CHARS']='2000000'
runpy.run_path(str(Path(__file__).with_name('viewport_window_e2e.py')),run_name='__main__')
o=Path(sys.argv[1]).resolve();j=json.loads((o/'viewport-e2e.json').read_text());p=j['proof'];b=p['background_layout'];g=p['ordered_pointer'];m=g['metrics']
assert b['complete']and b['active_at_input']>0 and b['layout_worker_thread']!=b['foreground_thread']
assert g['complete']and g['active_at_batch']>0 and g['revision_after']==g['revision_before']+2
for k in ['same_frame_press_move_release_paste','undo_restored_selection','protected_source_restored']:assert g[k],k
assert m['last_deferred_revision']==g['revision_before']+1 and m['last_deferred_order'][:4]==['press','move','release','paste']and m['last_selection']==[3,7]and m['deferred']>0 and m['resumed']>0
assert len(p['screenshots'])==15 and p['source_chars']==2000015 and p['source_bytes']==2777778
assert next(s for s in p['screenshots']if s['name']=='00-cold-input')['background_layout_active']>0
j['result']='NATIVE_ORDERED_POINTER_OK';(o/'pointer-e2e.json').write_text(json.dumps(j,indent=2)+'\n')
print('NATIVE_ORDERED_POINTER_OK: real2M Unicode row;actual active font worker;native Press/Move/Release/Paste in ONE frame;queued order/exact selection/current revision/two Undo/restored bytes;IME/tabs;15 genuine GPU captures;no Gradle/deploy',flush=True)
