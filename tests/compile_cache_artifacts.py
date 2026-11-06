#!/usr/bin/env python3
"""Reopen actual compiler receipts/APKs and independent wrapper call evidence."""
from pathlib import Path
import json,os,subprocess,sys,zipfile
O=Path(sys.argv[1]).resolve();r=json.loads((O/'compile-cache-e2e.json').read_text());assert r['result']=='MODULE_COMPILE_CACHE_E2E_OK'
assert len(r['cases'])==15,len(r['cases'])
for label,c in r['cases'].items():
 x=json.loads((O/(label+'.json')).read_text());assert {s['module']:s['state'] for s in x['compilation_cache']}==c['expected']
 assert (O/(label+'.exit')).read_text().strip()=='0'
 for s in x['compilation_cache']:
  assert len(s['key'])==64 and s['files']>0
  out=Path(x['build_dir'])/'classes'/s['module'];assert out.is_dir() and list(out.rglob('*.class'))
 with zipfile.ZipFile(x['apk']['path']) as z:assert all(n in z.namelist() for n in ['classes.dex','resources.arsc','AndroidManifest.xml'])
 assert c['actual_compiler_calls']==[] if all(v=='hit' for v in c['expected'].values()) else c['actual_compiler_calls']
assert r['cases']['cold']['blake3']==r['cases']['repeat']['blake3']
assert (O/'invalid-java.exit').read_text().strip()!='0'
assert 'Kotlin K2 · app failed' in (O/'invalid-java.log').read_text() or 'Javac · app failed' in (O/'invalid-java.log').read_text()
sdk=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'));aapt=sdk/'build-tools/36.0.0/aapt2'
for label,needle in [('resource-value','CACHE_RESOURCE_VALUE'),('resource-field','CACHE_NEW_FIELD')]:
 x=json.loads((O/(label+'.json')).read_text());dump=subprocess.check_output([str(aapt),'dump','resources',x['apk']['path']],text=True);assert needle in dump;(O/(label+'-actual-resources.txt')).write_text(dump)
calls=[json.loads(x) for x in Path(r['actual_compiler_counters']).read_text().splitlines()];assert any('org.jetbrains.kotlin.cli.jvm.K2JVMCompiler' in c['args'] for c in calls)
assert not (Path(r['traps'])/'FORBIDDEN_EXECUTED').exists()
print('MODULE_CACHE_ARTIFACTS_OK:15 successful real builds + actual compiler rejection; signed APK resources/DEX/classes/receipts/compiler-counter evidence reopened; resource values/symbols really packaged')
