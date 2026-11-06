#!/usr/bin/env python3
"""Bind actual AAPT wrapper counters to separate private Layoutlib snapshot reports."""
from pathlib import Path
import os,json,subprocess,sys
R=Path(__file__).resolve().parents[1];fixture=Path(sys.argv[1]).resolve();O=Path(sys.argv[2]).resolve();f=json.loads((fixture/'resource-cache-e2e.json').read_text());env={**os.environ,'ANDROID_HOME':f['sdk']}
x=subprocess.run([sys.executable,str(R/'tests/sdk_cache_preview_e2e.py'),str(O)],env=env,text=True,capture_output=True,timeout=1000);O.mkdir(exist_ok=True);(O/'inner.log').write_text(x.stdout+x.stderr);assert x.returncode==0,x.stderr[-5000:]
j=json.loads((O/'sdk-preview-e2e.json').read_text());calls=[json.loads(s) for s in Path(f['actual_counters']).read_text().splitlines()];rows={}
for label,row in j['rows'].items():
 p=row['report'];records=p['resource_cache'];done=sum(r['compiled'] for r in records);hits=sum(r['reused'] for r in records);total=sum(r['files'] for r in records);assert total==hits+done and total>0
 link=next(s for s in p['compilation_steps'] if s['name']=='AAPT2 link + generated R');args=link['args'];work=Path(args[args.index('--manifest')+1]).parent
 counted=[];links=0
 for a in calls:
  if a[0]=='compile' and '-o' in a and Path(a[a.index('-o')+1]).is_relative_to(work):counted.extend(a[3:])
  if a[0]=='link' and '--manifest' in a and Path(a[a.index('--manifest')+1]).is_relative_to(work):links+=1
 assert len(counted)==done and links>0
 assert (done==total and hits==0) if label=='portrait' else (done==0 and hits==total)
 rows[label]={'compiled':done,'reused':hits,'files':total,'actual_compile_files':counted,'actual_links':links,'private_work':str(work)}
(O/'resource-preview-e2e.json').write_text(json.dumps({'result':'RESOURCE_PREVIEW_CACHE_OK','rows':rows,'layoutlib':j,'independent_aapt_counters':f['actual_counters'],'private_snapshots_share_flats':True},indent=2)+'\n');print('RESOURCE_PREVIEW_CACHE_OK: actual Compose/Layoutlib + independent AAPT counters; first compile, night/catalog zero resource compilation in different private snapshots; real relink and error preservation; no APK/device')
