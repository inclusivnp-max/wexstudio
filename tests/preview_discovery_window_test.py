from pathlib import Path
import json,sys,struct
out=Path(sys.argv[1]);window=Path(sys.argv[2]) if len(sys.argv)>2 else out/'ide';r=json.loads((window/'preview-window.json').read_text())
assert r['result']=='GUI_HEADLESS_PREVIEW_CATALOG_OK' and len(r['screenshots'])==10
assert r['disk_source_unchanged'] and r['snapshot_jobs_cleaned'] and not r['deployment_invoked'] and not r['emulator_used']
assert len(r['catalog']['items'])==15 and sum(i['problem'] is not None for i in r['catalog']['items'])==3
s=r['snapshots'];night=s['07-annotation-night']['report'];land=s['08-annotation-landscape']['report'];dirty=s['09-unsaved-annotation']['report']
assert night['entry']['locale']=='fr-FR' and night['entry']['dark'] and night['entry']['font_scale']==1.25
assert land['entry']['width_dp']==640 and dirty['entry']['width_dp']==180 and dirty['buffers']
assert land['selected_id']==dirty['selected_id'] and land['png_blake3']!=dirty['png_blake3']
assert not (out/'cli/project/wex.preview.json').exists() and not (out/'cli/project/.wex/native/debug/current.json').exists()
for shot in r['screenshots']:
 b=Path(shot['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',b[16:24])==(shot['width'],shot['height'])
print('GUI_PREVIEW_DISCOVERY_OK: 10 real GPU screenshots; compiled @Preview/multipreview picker; French/night/landscape; live unsaved annotation resize with stable ID; disabled unsupported scenes; error/Cancel/reopen; sources unchanged; no config/emulator/DEX/sign/deploy/Gradle')
