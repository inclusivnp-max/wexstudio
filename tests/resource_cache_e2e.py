#!/usr/bin/env python3
"""Actual per-file AAPT2 counters and APK contents; no fake flats/compiler responses."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,time,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);P=O/'project';B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();S=Path.home()/'Android/Sdk';SDK=O/'sdk';bt=SDK/'build-tools/36.0.0';bt.mkdir(parents=True);(SDK/'platforms').symlink_to(S/'platforms',target_is_directory=True)
for n in ['lib','lib64']:(bt/n).symlink_to(S/'build-tools/36.0.0'/n,target_is_directory=True)
for n in ['aapt2','d8','apksigner','zipalign']:shutil.copy2(S/'build-tools/36.0.0'/n,bt/n)
original_sdk_hash=hashlib.sha256((S/'build-tools/36.0.0/aapt2').read_bytes()).hexdigest();backing=bt/'aapt2.real';(bt/'aapt2').rename(backing);calls=O/'calls.jsonl';calls.touch();wrapper=bt/'aapt2';wrapper.write_text('#!/usr/bin/python3\nimport os,sys,json\nwith open('+repr(str(calls))+',"a") as f:f.write(json.dumps(sys.argv[1:])+"\\n")\nos.execv('+repr(str(backing))+',['+repr(str(backing))+']+sys.argv[1:])\n');wrapper.chmod(0o755)
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True);f=P/'wex.json';cfg=json.loads(f.read_text());cfg['android']['application_id']='dev.wex.flatcache.e2e.case'+str(time.time_ns());f.write_text(json.dumps(cfg,indent=2)+'\n')
res=P/'app/src/main/res';values=res/'values/cache_values.xml';values.write_text('<resources><string name="cache_saved">WEX_FLAT_V1</string></resources>\n');layout=res/'layout/cache_view.xml';layout.parent.mkdir();layout.write_text('<LinearLayout xmlns:android="http://schemas.android.com/apk/res/android" android:layout_width="match_parent" android:layout_height="wrap_content"><TextView android:id="@+id/cache_label" android:layout_width="wrap_content" android:layout_height="wrap_content" android:text="@string/cache_saved"/></LinearLayout>\n')
raw=res/'raw/cache_blob.bin';raw.parent.mkdir();raw.write_bytes(b'WEX_FLAT_RAW')
icon=res/'drawable/cache_icon.png';icon.parent.mkdir(exist_ok=True)
with zipfile.ZipFile(S/'platforms/android-36/android.jar') as z:
 name=next(n for n in z.namelist() if n.endswith('.png') and not n.endswith('.9.png'));icon.write_bytes(z.read(name))
main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';s=main.read_text();anchor='        debugProbe(7)';assert anchor in s;s=s.replace(anchor,'''        val cachedView = layoutInflater.inflate(R.layout.cache_view, null)
        val bitmap = android.graphics.BitmapFactory.decodeResource(resources, R.drawable.cache_icon)
        val cachedRaw = resources.openRawResource(R.raw.cache_blob).bufferedReader().use { it.readText() }
        Log.i("WexRun", "WEX_RESOURCE_CACHE_OK runId=${intent.getStringExtra("wex.run_id")} value=${getString(R.string.cache_saved)} raw=$cachedRaw label=${cachedView.findViewById<TextView>(R.id.cache_label).text} png=${bitmap.width}x${bitmap.height}")
'''+anchor);main.write_text(s)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/n;p.write_text('#!/bin/sh\necho '+n+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
env={**os.environ,'ANDROID_HOME':str(SDK),'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_GRADLE_BRIDGE':str(traps/'bridge')};cases={}
def count():return len(calls.read_text().splitlines())
def observed(start):
 compiled=[];links=0
 for line in calls.read_text().splitlines()[start:]:
  a=json.loads(line)
  if a[0]=='link':links+=1
  if a[0]=='compile':
   assert '--dir' not in a,('unexpected directory fallback',a)
   assert a[1]=='-o';compiled.extend(a[3:])
 return compiled,links

def invoke(label,args,want=None,ok=True):
 start=count();x=subprocess.run([str(B),*map(str,args)],env=env,text=True,capture_output=True,timeout=300);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');assert (x.returncode==0)==ok,(label,x.stderr[-6000:]);compiled,links=observed(start)
 if not ok:return x,compiled
 value=json.loads(x.stdout)
 if want is not None:
  records=value.get('resource_cache',value.get('model',{}).get('generation_resource_cache',[]));assert records
  total=sum(r['files'] for r in records);done=sum(r['compiled'] for r in records);hits=sum(r['reused'] for r in records);assert done+hits==total and done==len(compiled)
  if want=='cold':assert done==total and hits==0
  else:assert done==want,(label,done,want,records)
  assert links>0 and all('resource-inputs' in p or '.tmp' in p for p in compiled)
  cases[label]={'compiled':done,'reused':hits,'files':total,'actual_compile_files':compiled,'actual_links':links,'records':records,'report':value};print('RESOURCE_CACHE_CASE_OK',label,'compiled',done,'reused',hits,'links',links,flush=True)
 assert not marker.exists();return value
invoke('resolve',['resolve',P,'--offline'])
initial=invoke('analysis-cold',['analyze',P,'--offline'],'cold');assert not (P/'.wex/native').exists()
a=invoke('apk-after-analysis',['build',P,'--offline'],0);b=invoke('apk-repeat',['build',P,'--offline'],0);assert a['blake3']==b['blake3']
values.write_text(values.read_text().replace('WEX_FLAT_V1','WEX_FLAT_V2'));invoke('one-value-change',['build',P,'--offline'],1)
main.write_text(main.read_text()+'\n// source-only body cache check\n');invoke('source-only',['build',P,'--offline'],0)
locale=res/'values-en/cache_values.xml';locale.parent.mkdir();locale.write_text('<resources><string name="cache_saved">WEX_FLAT_V2_EN</string></resources>\n');invoke('qualifier-added',['build',P,'--offline'],1)
renamed=values.with_name('cache_renamed.xml');values.rename(renamed);invoke('file-renamed',['build',P,'--offline'],1);locale.unlink();b=invoke('qualifier-deleted',['build',P,'--offline'],0)
sample=next(s for r in b['resource_cache'] for s in r['samples'] if s['source'].endswith('/values/cache_renamed.xml'));key=sample['key'];cache=P/'.wex/resource-cache/aapt-flat-files-v1';entry=cache/key;flat=next((entry/'flat').glob('*.flat'));flat.write_bytes(flat.read_bytes()+b'PRIVATE_TAMPER');invoke('flat-tampered',['build',P,'--offline'],1)
source=P/'.wex/resource-inputs/aapt-flat-files-v1'/key/'source/values/cache_renamed.xml';source.write_bytes(b'PRIVATE_BAD_SOURCE');invoke('source-snapshot-repaired',['build',P,'--offline'],0);assert source.read_bytes()==renamed.read_bytes()
lock=cache/(key+'.lock');receipt=(entry/'receipt.json').read_bytes();locker=subprocess.Popen(['/usr/bin/python3','-c',"import sys,fcntl; f=open(sys.argv[1],'r+'); fcntl.flock(f,fcntl.LOCK_EX); print('LOCKED',flush=True); sys.stdin.read()",str(lock)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True);assert locker.stdout.readline().strip()=='LOCKED'
try:busy=invoke('busy-one-file',['build',P,'--offline'],1);assert sum(r['uncached'] for r in busy['resource_cache'])==1 and (entry/'receipt.json').read_bytes()==receipt
finally:locker.communicate('',timeout=10)
invoke('busy-released',['build',P,'--offline'],0)
pointer=P/'.wex/native/debug/current.json';prior=pointer.read_bytes();saved=renamed.read_bytes();renamed.write_text('<resources><broken>')
try:
 x,compiled=invoke('invalid-xml',['build',P,'--offline'],ok=False);assert len(compiled)==1 and pointer.read_bytes()==prior and str(renamed) in x.stderr and '.wex/resource-inputs/' not in x.stderr
finally:renamed.write_bytes(saved)
saved=layout.read_bytes();layout.write_text(layout.read_text().replace('@string/cache_saved','@string/this_does_not_exist'))
try:
 x,compiled=invoke('invalid-link',['build',P,'--offline'],ok=False);assert len(compiled)==1 and pointer.read_bytes()==prior and str(layout) in x.stderr and 'this_does_not_exist' in x.stderr
finally:layout.write_bytes(saved)
invoke('repaired',['build',P,'--offline'],0)
old=wrapper.read_bytes();wrapper.write_bytes(old+b'\n# test different verified AAPT tool bytes\n');invoke('tool-input-changed',['build',P,'--offline'],'cold');wrapper.write_bytes(old);final=invoke('tool-input-restored',['build',P,'--offline'],0)
# Real linked APK data, not merely cache flags or compiled filenames.
dump=subprocess.check_output([str(wrapper),'dump','resources',final['apk']['path']],env=env,text=True);assert 'WEX_FLAT_V2' in dump and 'WEX_FLAT_V2_EN' not in dump;(O/'final-resources.txt').write_text(dump)
with zipfile.ZipFile(final['apk']['path']) as z:assert z.read('res/raw/cache_blob.bin')==b'WEX_FLAT_RAW' and 'res/layout/cache_view.xml' in z.namelist() and 'res/drawable/cache_icon.png' in z.namelist()
assert hashlib.sha256((S/'build-tools/36.0.0/aapt2').read_bytes()).hexdigest()==original_sdk_hash and not marker.exists()
(O/'resource-cache-e2e.json').write_text(json.dumps({'result':'RESOURCE_FILE_CACHE_E2E_OK','project':str(P),'sdk':str(SDK),'traps':str(traps),'cases':cases,'actual_counters':str(calls),'single_file_incremental':True,'deletion_rename_qualifier':True,'raw_png_layout_ids':True,'tamper_and_busy_recovery':True,'errors_mapped_to_original_sources':True,'failed_compile_link_keeps_apk':True,'original_sdk_unchanged':True,'final_apk':final['apk']},indent=2)+'\n');print('RESOURCE_FILE_CACHE_E2E_OK: actual AAPT counters; warm zero/single XML one; AAR+project+PNG/raw/layout; add/rename/delete qualifiers; tamper/source-repair/OS-lock fallback; genuine compile/link errors preserve APK and map sources; no Gradle')
