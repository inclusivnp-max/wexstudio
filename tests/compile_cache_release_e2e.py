#!/usr/bin/env python3
"""Actual release cache keeps signing preflight and variant isolation, ephemeral key."""
from pathlib import Path
import json,os,shutil,subprocess,sys,tempfile,secrets,re
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);P=O/'project';B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();J=Path('/usr/lib/jvm/java-21-openjdk')
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True);cfg=json.loads((P/'wex.json').read_text());rows={}
with tempfile.TemporaryDirectory(prefix='wex-module-cache-release-') as private:
 private=Path(private);store=secrets.token_urlsafe(24);key=store;env={**os.environ,'WEX_CACHE_RELEASE_STORE':store,'WEX_CACHE_RELEASE_KEY':key};ks=private/'release.jks';traps=private/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
 for n in ['gradle','gradlew','mvn','bridge','adb','emulator']:
  p=traps/n;p.write_text('#!/bin/sh\necho '+n+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
 env['PATH']=str(traps)+os.pathsep+env['PATH'];env['WEX_GRADLE_BRIDGE']=str(traps/'bridge')
 x=subprocess.run([str(J/'bin/keytool'),'-genkeypair','-storetype','JKS','-keystore',str(ks),'-alias','release','-storepass:env','WEX_CACHE_RELEASE_STORE','-keypass:env','WEX_CACHE_RELEASE_KEY','-keyalg','RSA','-keysize','2048','-validity','3650','-dname','CN=Wex Module Cache Test'],env=env,text=True,capture_output=True);assert x.returncode==0
 listing=subprocess.check_output([str(J/'bin/keytool'),'-J-Duser.language=en','-list','-v','-keystore',str(ks),'-alias','release','-storepass:env','WEX_CACHE_RELEASE_STORE'],env=env,text=True,stderr=subprocess.DEVNULL);pin=re.search(r'SHA256:\s*([A-Fa-f0-9:]+)',listing)[1].replace(':','').lower()
 cfg['build']['release_signing']=dict(keystore=str(ks),key_alias='release',store_password_env='WEX_CACHE_RELEASE_STORE',key_password_env='WEX_CACHE_RELEASE_KEY',certificate_sha256=pin);(P/'wex.json').write_text(json.dumps(cfg,indent=2)+'\n')
 def invoke(label,args,environment=env,ok=True):
  x=subprocess.run([str(B),*map(str,args)],env=environment,text=True,capture_output=True,timeout=300);assert store not in x.stdout+x.stderr and key not in x.stdout+x.stderr;assert (x.returncode==0)==ok,(label,x.stderr[-4000:]);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.log')).write_text(x.stderr);(O/(label+'.exit')).write_text(str(x.returncode)+'\n');return x
 invoke('resolve',['resolve',P,'--offline'])
 for label,variant,state in [('debug','debug','miss'),('release','release','miss'),('release-repeat','release','hit')]:
  x=json.loads(invoke(label,['build',P,'--offline','--variant',variant]).stdout);assert {r['module']:r['state'] for r in x['compilation_cache']}=={'app':state,'shared':state};assert x['signing']['kind']==variant and x['debuggable']==(variant=='debug')
  names=[s['name'] for s in x['steps']];assert 'Verify ZIP alignment' in names and any('sign' in n.lower() for n in names)
  for n in ['app','shared']:assert (f'Kotlin K2 · {n}' in names)==(state=='miss')
  if variant=='release':assert x['signing']['certificate_sha256']==pin and any('preflight' in n.lower() for n in names),names
  generated=Path(x['build_dir'])/'module-generated/app/dev/wex/android/BuildConfig.java';assert ('DEBUG = '+str(variant=='debug').lower()) in generated.read_text()
  rows[label]=x;print('RELEASE_CACHE_CASE_OK',label,state,flush=True)
 assert rows['release']['blake3']==rows['release-repeat']['blake3'];assert rows['release']['compilation_cache'][0]['key']!=rows['debug']['compilation_cache'][0]['key']
 pointer=P/'.wex/native/release/current.json';prior=pointer.read_bytes();bad=invoke('wrong-password',['build',P,'--offline','--variant','release'],{**env,'WEX_CACHE_RELEASE_STORE':'wrong'},False);assert pointer.read_bytes()==prior and 'AAPT2' not in bad.stderr and 'MODULE_CLASSES' not in bad.stderr
 assert not marker.exists()
 (O/'release-cache-e2e.json').write_text(json.dumps({'result':'RELEASE_MODULE_CACHE_OK','reports':rows,'wrong_password_preflight_before_cache':True,'last_good_apk_preserved':True,'debug_release_isolated':True,'repeat_identical':True,'credential_values_not_logged':True,'no_gradle_or_device':True},indent=2)+'\n');print('RELEASE_MODULE_CACHE_OK: debug/release keys+BuildConfig isolated; repeated actual signed release APK class hits; signer/preflight still execute; wrong credential fails before cache/SDK and preserves last-good; no secrets logged')
