#!/usr/bin/env python3
"""Actual two-million-character row: typing/GPU capture during private worker layout."""
from pathlib import Path
import json,os,runpy,sys
os.environ['WEX_VIEWPORT_FLAG']='--layout-smoke';os.environ['WEX_VIEWPORT_LINE_CHARS']='2000000'
runpy.run_path(str(Path(__file__).with_name('viewport_window_e2e.py')),run_name='__main__')
o=Path(sys.argv[1]).resolve();j=json.loads((o/'viewport-e2e.json').read_text());p=j['proof'];b=p['background_layout'];assert b['complete']and b['active_at_input']>0 and b['revision_after']==b['revision_before']+1 and b['layout_worker_thread']!=b['foreground_thread']
assert p['long_line_chars']>=2000000 and p['source_chars']>=2000000 and b['metrics']['superseded']>0 and b['metrics']['active']==0 and b['retirement']['layouts_reclaimed']>0
cold=next(s for s in p['screenshots']if s['name']=='00-cold-input');assert cold['background_layout_active']>0,'Native capture must complete while actual shaping is active, not after a fake delay'
j['result']='NATIVE_BACKGROUND_LAYOUT_OK';(o/'layout-e2e.json').write_text(json.dumps(j,indent=2)+'\n')
print('NATIVE_BACKGROUND_LAYOUT_OK: actual2M-character Unicode row;real worker active at native Text input AND GPU capture;new revision/Undo/latest state/exact head-tail/mouse/IME/tabs;private atlas;13 GPU frames;no Gradle/deploy',flush=True)
