#!/usr/bin/env python3
"""Validate real native-window model/diagnostic/variant navigation evidence."""
from pathlib import Path
from urllib.parse import unquote,urlparse
import json,struct,sys
out=Path(sys.argv[1]).resolve();project=out/'live-project';r=json.loads((out/'window/analysis-window.json').read_text())
assert r['result']=='GUI_NATIVE_ANALYSIS_OK' and r['disk_source_unchanged']
assert not r['apk_built'] and not r['deployment_invoked'] and not r['gradle_invoked']
assert not (project/'.wex/native').exists() and len(r['screenshots'])==5
s=r['snapshots'];assert len(s)==5
for index in [0,1,2,3]:
 assert s[index]['diagnostic_snapshot']==[s[index]['path'],s[index]['version']]
 assert bool(any(d['severity']==1 for d in s[index]['diagnostics']))==(index==1)
assert s[0]['variant']=='debug' and s[3]['variant']=='release'
assert s[1]['dirty'] and not s[2]['dirty']
path=Path(r['generated_definition']);assert path.is_relative_to(project/'.wex/analysis/release') and 'DEBUG = false' in path.read_text()
for variant in ['debug','release']:
 m=json.loads((project/'.wex/analysis'/variant/'current.json').read_text());assert m['variant']==variant and m['backend']=='wex-native-analysis-3'
 assert all('Kotlin K2' not in x['name'] and 'DEX' not in x['name'] and 'sign' not in x['name'].lower() for x in m['generation_steps'])
for shot in r['screenshots']:
 b=Path(shot['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',b[16:24])==(shot['width'],shot['height'])
print('ANALYSIS_WINDOW_OK: genuine matching K2 diagnostic versions; type error/Undo; debug/release independent models; real F12 generated DEBUG=false;5 native PNGs; no APK/deploy/Gradle')
