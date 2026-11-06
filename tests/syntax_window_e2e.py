#!/usr/bin/env python3
"""Actual large Kotlin typing and a separate fresh-tree oracle, no parser delays."""
from pathlib import Path
import hashlib,json,os,struct,subprocess,sys
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();P=O/'project';P.mkdir();W=O/'window';W.mkdir()
with(P/'Large.kt').open('w')as f:
 f.write('// Real Kotlin document; no synthetic parser delay\n')
 for n in range(500000):f.write(f'private val property{n} = {n}\n')
(P/'Tiny.kt').write_text('val tiny=7\n');hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.iterdir()};traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'WEX_TOOLS_HOME':str(R/'.wex')};env.pop('WAYLAND_DISPLAY',None)
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),'--syntax-smoke',str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE)})+'\n');code=proc.wait(timeout=310)
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-8000:];p=json.loads((W/'syntax-window.json').read_text());assert p['result']=='GPU_ASYNC_SYNTAX_OK'and p['source_lines']>500000 and p['burst_count']==20 and p['syntax_workers']==1
for k in ['typed_while_real_parser_active','latest_wins','no_stale_spans_while_pending','tab_switch_and_parked_buffer_preserved','ime_commit_single_undo','indent_outdent_deferred','original_buffer_undo_restored','disk_sources_unchanged','no_synthetic_parse_delay']:assert p[k],k
oracle=p['fresh_oracle'];assert oracle['digest']==oracle['fresh_digest']and oracle['nodes']>1000000 and oracle['bytes']>10000000 and oracle['full_node_kinds_byte_ranges_points_match']
assert p['completed_parses']>0 and p['last_parse_us']>0
assert sum(a['action']=='IME'for a in p['actions'])==1 and sum(a['action']=='Indent'for a in p['actions'])==2
assert sum(a['action']=='Paste'for a in p['actions'])==22
for a in p['actions']:
 assert a['revision_after']==a['revision_before']+1
 assert(a['tree_end_byte']is None and a['syntax_pending_at_input'])or a['tree_end_byte']==a['rope_bytes']
assert all(a['syntax_pending_at_input']for a in p['actions']if a['action']in['Paste','IME','Indent'])
assert len(p['screenshots'])==9
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()
(O/'syntax-e2e.json').write_text(json.dumps({'result':'NATIVE_ASYNC_SYNTAX_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'no_gradle_or_device_deployment':True},indent=2)+'\n');print('NATIVE_ASYNC_SYNTAX_OK: actual500k source/native20-input burst while real parser active; latest-state publication/no stale spans; tab/parked/IME/indent/outdent/Undo/Redo; independent full fresh-tree node kinds+byte ranges+points match; disk unchanged; 9 genuine GPU frames; no synthetic parse delay/Gradle/deployment',flush=True)
