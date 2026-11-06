#!/usr/bin/env python3
"""Actual Java formatter library + Javac/Java; isolated tool data; no Android mutation."""
from pathlib import Path
import argparse,hashlib,json,os,shutil,subprocess,sys
from native_window_process import run_window
p=argparse.ArgumentParser();p.add_argument('output',type=Path);a=p.parse_args();r=Path(__file__).resolve().parents[1];out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
project=out/'project';shutil.copytree(r/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex'))
tools=out/'tools';(tools/'java-formatter').mkdir(parents=True);pin=json.loads((r/'formatter/dependency.json').read_text());name='google-java-format-'+pin['version']+'-all-deps.jar';shutil.copy2(r/'.wex/java-formatter'/name,tools/'java-formatter'/name)
support=out/'support';support.mkdir();inventory={}
for name in ['formatter/WexJavaFormat.java','formatter/dependency.json','scripts/prepare-java-formatter.py']:
 dest=support/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(r/name,dest);inventory[name]=hashlib.sha256(dest.read_bytes()).hexdigest()
(support/'support.json').write_text(json.dumps({'schema':1,'files':inventory},indent=2)+'\n')
traps=out/'traps';traps.mkdir();marker=out/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','adb','emulator']:
 f=traps/name;f.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(tools),'WEX_SUPPORT_HOME':str(support),'WEX_FORMATTER_OFFLINE':'1','WEX_FORMAT_JAVA_HOME':os.path.relpath('/usr/lib/jvm/java-21-openjdk',r),'WEX_JAVA_FORMAT_PROJECT':str(project),'WEX_JAVA_FORMAT_OUTPUT':str(out/'java-format.json'),'WEX_JAVA_FORMAT_TRACE':str(out/'commands.jsonl'),'JAVA_TOOL_OPTIONS':'-javaagent:WEX_MUST_NOT_LOAD.jar','_JAVA_OPTIONS':'-javaagent:WEX_MUST_NOT_LOAD.jar','JDK_JAVA_OPTIONS':'-javaagent:WEX_MUST_NOT_LOAD.jar','CARGO_BUILD_JOBS':'1','RUST_TEST_THREADS':'1'}
original={str(f.relative_to(project)):hashlib.sha256(f.read_bytes()).hexdigest()for f in project.rglob('*')if f.is_file()}
code=run_window(['cargo','test','--test','java_format','--features','desktop','--locked','--no-run','--message-format=json'],env,out/'build.jsonl',out/'build-process.json',300,stderr_path=out/'build.log');assert code==0,(out/'build.log').read_text()[-6000:]
rows=[json.loads(l)for l in(out/'build.jsonl').read_text().splitlines()if l.startswith('{')];bins=[Path(x['executable'])for x in rows if x.get('reason')=='compiler-artifact'and x.get('executable')and x.get('target',{}).get('name')=='java_format'and x.get('profile',{}).get('test')];assert len(bins)==1
(out/'test-binary.json').write_text(json.dumps({'path':str(bins[0]),'sha256':hashlib.sha256(bins[0].read_bytes()).hexdigest(),'direct_parent_environment':True,'formatter_JDK_caller_relative':env['WEX_FORMAT_JAVA_HOME']},indent=2)+'\n')
code=run_window([bins[0],'genuine_google_java_format_review','--ignored','--nocapture'],env,out/'probe.log',out/'process.json',600);(out/'probe.exit').write_text(str(code)+'\n');assert code==0,(out/'probe.log').read_text()[-12000:]
# Corrupt isolated tool data or a private, self-consistent support manifest.
# The embedded bridge/bootstrap byte pins must still reject altered source.
jar=tools/'java-formatter'/('google-java-format-'+pin['version']+'-all-deps.jar');jar_bytes=jar.read_bytes()
negative=[]
for kind,filename,needle in [('jar',None,'missing/damaged'),('bridge','formatter/WexJavaFormat.java','bridge source changed'),('bootstrap','scripts/prepare-java-formatter.py','preparation source changed')]:
 backup=None
 if filename:
  target=support/filename;backup=target.read_bytes();target.write_bytes(backup+b'\n// tampered\n'if kind=='bridge'else backup+b'\n# tampered\n');inventory[filename]=hashlib.sha256(target.read_bytes()).hexdigest();(support/'support.json').write_text(json.dumps({'schema':1,'files':inventory},indent=2)+'\n')
 else:jar.write_bytes(b'not the pinned jar')
 try:
  neg_env={**env,'WEX_JAVA_FORMAT_EXPECT_ERROR':needle}
  result=run_window([bins[0],'genuine_java_formatter_failure_keeps_source','--ignored','--nocapture'],neg_env,out/(kind+'.log'),out/(kind+'-process.json'),60)
  assert result==0,(out/(kind+'.log')).read_text();negative.append({'case':kind,'exit':result,'result':'JAVA_FORMATTER_FAILURE_OK'})
 finally:
  if filename:target.write_bytes(backup);inventory[filename]=hashlib.sha256(backup).hexdigest();(support/'support.json').write_text(json.dumps({'schema':1,'files':inventory},indent=2)+'\n')
  else:jar.write_bytes(jar_bytes)
q=json.loads((out/'java-format.json').read_text());assert q['result']=='ACTUAL_JAVA_FORMATTER_OK' and q['disk_unchanged'] and q['selection']['expanded_context']
commands=[json.loads(l)for l in(out/'commands.jsonl').read_text().splitlines()];java=[x for x in commands if x['stage']=='spawn'and x['program'].endswith('/bin/java')]
assert len(java)>=8 and all(x['program']=='/usr/lib/jvm/java-21-openjdk/bin/java'and str(support/'formatter/WexJavaFormat.java')in x['args']for x in java)
assert any(x['stage']=='stop'and x['cancelled']and x['reaped'] for x in commands)
assert all(hashlib.sha256((project/f).read_bytes()).hexdigest()==h for f,h in original.items()) and not marker.exists() and not(project/'.wex/native').exists()
(out/'proof.json').write_text(json.dumps({'result':'GENUINE_JAVA_FORMATTER_E2E_OK','probe':q,'actual_formatter_calls':len(java),'original_sources':original,'commands':commands,'verified_negative_tools':negative,'no_Gradle_Maven_APK_device':True},indent=2)+'\n')
print('GENUINE_JAVA_FORMATTER_E2E_OK: actual pinned library file/range; real semantic execution/cancellation; relative JDK binding and JVM-option isolation; source unchanged; no Gradle/Maven/APK/device')
