#!/usr/bin/env python3
"""Genuine K2 and Save, entered from Find: click/Paste/CtrlS/F12 in one native frame."""
from pathlib import Path
import json,os,runpy,sys
os.environ['WEX_SNAPSHOT_FLAG']='--input-order-smoke'
runpy.run_path(str(Path(__file__).with_name('language_snapshot_window_e2e.py')),run_name='__main__')
o=Path(sys.argv[1]).resolve();j=json.loads((o/'snapshot-e2e.json').read_text());p=j['proof'];c=p['ordered_commands']
for k in ['complete','find_query_handoff','incoming_click_paste_save_f12_one_frame','saved_prefix_verified','undo_save_restored_disk']:assert c[k],k
assert c['saved_version']>c['before_version']and Path(c['definition']).name=='Tiny.kt'and len(p['screenshots'])==11
j['result']='NATIVE_ORDERED_COMMANDS_OK';(o/'input-order-e2e.json').write_text(json.dumps(j,indent=2)+'\n')
print('NATIVE_ORDERED_COMMANDS_OK: actual100k source/genuine K2;CtrlF+query handoff;incoming editor click/Paste/CtrlS/F12 in ONE frame;real disk prefix before definition/current clock;Undo+Save restores original;dirty parked snapshots;11 genuine GPU frames;no Gradle/APK/deploy',flush=True)
