#!/usr/bin/env python3
"""Prepare a reproducible Tooling API client from Gradle + AGP public model JARs."""
import hashlib,json,os,subprocess,sys,urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
OUT=Path(os.environ.get('WEX_TOOLS_HOME',ROOT/'.wex')).resolve()/'bridge'
OUT.mkdir(parents=True,exist_ok=True)
version='9.6.0'
candidates=sorted((Path.home()/'.gradle/wrapper/dists'/f'gradle-{version}-bin').glob(f'*/gradle-{version}'))
home=Path(os.environ['WEX_GRADLE_HOME']) if 'WEX_GRADLE_HOME' in os.environ else (candidates[0] if candidates else None)
if home is None or not (home/'lib'/f'gradle-tooling-api-{version}.jar').is_file():
    raise SystemExit('Set WEX_GRADLE_HOME to a Gradle 9.6.0 distribution; project wrapper can provision it')
models=list((Path.home()/'.gradle/caches/modules-2/files-2.1/com.android.tools.build/builder-model/9.2.1').glob('*/builder-model-9.2.1.jar'))
model=models[0] if models else OUT/'builder-model-9.2.1.jar'
if not model.exists():
    urllib.request.urlretrieve('https://dl.google.com/dl/android/maven2/com/android/tools/build/builder-model/9.2.1/builder-model-9.2.1.jar',model)
classpath=os.pathsep.join([str(home/'lib/*'),str(model)])
sources=sorted((ROOT/'bridge/src').rglob('*.java'))
digest=hashlib.sha256((classpath+'\n').encode()+b''.join(p.read_bytes() for p in sources)+(ROOT/'bridge/generated.init.gradle').read_bytes()+model.read_bytes()).hexdigest()
stamp=OUT/'stamp'
classes=OUT/'classes'
if not stamp.exists() or stamp.read_text()!=digest or not (classes/'dev/wex/bridge/GradleBridge.class').exists():
    classes.mkdir(exist_ok=True)
    subprocess.run(['javac','--release','17','-cp',classpath,'-d',str(classes),*map(str,sources)],check=True)
    stamp.write_text(digest)
config={'classpath':os.pathsep.join([str(classes),classpath]),'gradle_home':str(home),'model_version':'9.2.1','source_hash':digest}
(OUT/'config.json').write_text(json.dumps(config,indent=2)+'\n')
print(config['classpath'])
