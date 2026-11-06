from pathlib import Path
import json,subprocess,tempfile,os
R=Path(__file__).resolve().parents[1]
t=json.loads((R/'.wex/preview-renderer/tools.json').read_text());cp=os.pathsep.join([t['classes'],*t['classpath']])
p=R/'tests/fixtures/compose-android/.wex/preview-tooling';lock=json.loads((p/'wex.lock.json').read_text());jar=next(p/'aar'/str(i)/'classes.jar' for i,a in enumerate(lock['artifacts']) if a['coordinate']['artifact']=='ui-tooling-preview-android')
with tempfile.TemporaryDirectory(prefix='wex-annotation-bytecode-') as work:
 subprocess.run([str(Path(t['java']).with_name('javac')),'-proc:none','-cp',cp,'-d',work,str(R/'tests/PreviewDiscoveryBytecodeTest.java')],check=True)
 subprocess.run([t['java'],'-cp',work+os.pathsep+cp,'PreviewDiscoveryBytecodeTest',work,str(jar.resolve())],check=True)
