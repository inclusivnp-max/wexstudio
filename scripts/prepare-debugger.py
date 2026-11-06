#!/usr/bin/env python3
"""Pinned Java Debug Server core + standalone providers. No Maven/Gradle/JDT LS."""
from pathlib import Path
import hashlib,json,os,subprocess,urllib.request,argparse
R=Path(__file__).resolve().parents[1]
a=argparse.ArgumentParser();a.add_argument('--offline',action='store_true');args=a.parse_args()
D=Path(os.environ.get('WEX_TOOLS_HOME',R/'.wex')).resolve()/'debugger';lib=D/'lib';lib.mkdir(parents=True,exist_ok=True)
for dep in json.loads((R/'debugger/dependencies.json').read_text()):
 p=lib/dep['file']
 if not p.exists():
  if args.offline:raise SystemExit('Missing pinned debugger library: '+str(p))
  data=urllib.request.urlopen(dep['url'],timeout=60).read()
  assert hashlib.sha256(data).hexdigest()==dep['sha256'],'debugger download hash mismatch'
  tmp=p.with_suffix('.tmp');tmp.write_bytes(data);tmp.replace(p)
 assert hashlib.sha256(p.read_bytes()).hexdigest()==dep['sha256'],'debugger library hash mismatch: '+str(p)
java=Path(os.environ.get('WEX_DEBUG_JAVA_HOME','/usr/lib/jvm/java-21-openjdk'))
classes=D/'classes';classes.mkdir(exist_ok=True)
subprocess.run([str(java/'bin/javac'),'--release','21','--add-modules','jdk.jdi','-cp',str(lib/'*'),'-d',str(classes),str(R/'debugger/WexDebugServer.java')],check=True)
print('DEBUGGER_TOOLS_OK: Microsoft Java Debug Server 0.53.1 + JDK JDI; pinned hashes; no Maven/Gradle')
