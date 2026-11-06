#!/usr/bin/env python3
"""Install one SHA256-pinned formatter tool, never app dependencies/Gradle/Maven."""
from pathlib import Path
import argparse,fcntl,hashlib,json,os,stat,tempfile,urllib.request
p=argparse.ArgumentParser();p.add_argument('--offline',action='store_true');a=p.parse_args()
r=Path(__file__).resolve().parents[1];pin=json.loads((r/'formatter/dependency.json').read_text())
d=Path(os.environ.get('WEX_TOOLS_HOME',r/'.wex')).resolve()/'java-formatter';d.mkdir(parents=True,exist_ok=True)
jar=d/('google-java-format-'+pin['version']+'-all-deps.jar')
def valid():
 try:
  s=jar.lstat()
  return stat.S_ISREG(s.st_mode) and s.st_size<=16*1024*1024 and hashlib.sha256(jar.read_bytes()).hexdigest()==pin['sha256']
 except FileNotFoundError:return False
if not valid():
 if a.offline:raise SystemExit('Pinned Java formatter missing/damaged; prepare-java-formatter without --offline, then Retry')
 with (d/'install.lock').open('a') as lock:
  try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
  except BlockingIOError:raise SystemExit('Java formatter preparation busy; Retry')
  if not valid():
   with urllib.request.urlopen(pin['url'],timeout=45)as response:data=response.read(16*1024*1024+1)
   if len(data)>16*1024*1024 or hashlib.sha256(data).hexdigest()!=pin['sha256']:raise SystemExit('Java formatter download hash mismatch')
   with tempfile.NamedTemporaryFile(dir=d,prefix='.download-',delete=False)as f:
    name=Path(f.name)
    try:f.write(data);f.flush();os.fsync(f.fileno())
    except BaseException:name.unlink(missing_ok=True);raise
   try:os.replace(name,jar)
   finally:name.unlink(missing_ok=True)
assert valid(),'Java formatter verification failed'
print(json.dumps({'schema':1,'backend':'google-java-format','version':pin['version'],'jar':str(jar),'sha256':pin['sha256'],'app_classpath':False}))
