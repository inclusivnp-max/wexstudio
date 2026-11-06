#!/usr/bin/env python3
from pathlib import Path
import argparse,hashlib,json,os,re,shutil,subprocess,sys
from native_window_process import run_window
p=argparse.ArgumentParser();p.add_argument('output',type=Path);p.add_argument('--all-languages',action='store_true');a=p.parse_args();r=Path(__file__).resolve().parents[1];out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);project=out/'project';shutil.copytree(r/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex'));(project/'INDEX_FIXTURE').write_text('disposable genuine K2 persistent profile fixture\n')
traps=out/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/name;f.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
proof=out/'genuine-k2.json';binary=Path(os.environ.get('WEX_BIN',r/'target/debug/wex')).resolve();server=r/'.wex/tools/kotlin-lsp/kotlin-server-263.6379.0/bin/intellij-server'
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(r/'.wex'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','IJ_JAVA_OPTIONS':'-XX:ActiveProcessorCount=2 -Xmx1024m','WEX_FORMAT_PROJECT':str(project),'WEX_FORMAT_OUTPUT':str(proof),'WEX_KOTLIN_LSP':str(server),'WEX_K2_SYSTEM_DIR':str(out/'k2-system'),'WEX_LSP_TRACE':str(out/'lsp.jsonl'),'CARGO_BUILD_JOBS':'1','RUST_TEST_THREADS':'1'}
x=subprocess.run([binary,'resolve',project,'--offline'],env=env,text=True,capture_output=True,timeout=180);(out/'resolve.json').write_text(x.stdout);(out/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
sources={str(f.relative_to(project)):hashlib.sha256(f.read_bytes()).hexdigest()for f in project.rglob('*')if f.is_file()and'.wex'not in f.parts}
(out/'execution.json').write_text(json.dumps({'resolver_binary':str(binary),'resolver_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'resolver_only':True,'actual_probe':'current library test ELF (Cargo-built, directly executed)','original_sources':sources,'JVM_options':env['IJ_JAVA_OPTIONS']},indent=2)+'\n')
build=run_window(['cargo','test','--test','formatting_k2','--features','desktop','--locked','--no-run','--message-format=json'],env,out/'build.jsonl',out/'build-process.json',300,stderr_path=out/'build.log');assert build==0,(out/'build.log').read_text()[-6000:]
artifacts=[json.loads(line) for line in (out/'build.jsonl').read_text().splitlines() if line.startswith('{')]
executables=[Path(q['executable']) for q in artifacts if q.get('reason')=='compiler-artifact'and q.get('executable')and q.get('target',{}).get('name')=='formatting_k2'and q.get('profile',{}).get('test')]
assert len(executables)==1,executables
probe_binary=executables[0]
# Cargo extends LD_LIBRARY_PATH for `cargo test`; launch this actual test ELF
# directly under the unchanged production-parent environment, not that injected
# toolchain loader context. No host variable is removed or made artificially empty.
(out/'test-binary.json').write_text(json.dumps({'path':str(probe_binary),'sha256':hashlib.sha256(probe_binary.read_bytes()).hexdigest(),'direct_production_parent_environment':True,'parent_LD_LIBRARY_PATH_present':bool(env.get('LD_LIBRARY_PATH')),'parent_LD_PRELOAD_present':bool(env.get('LD_PRELOAD'))},indent=2)+'\n')
code=run_window([probe_binary,('genuine_k2_document_and_range_formatting' if a.all_languages else 'genuine_k2_kotlin_formatting'),'--ignored','--nocapture'],env,out/'probe.log',out/'process.json',600);(out/'probe.exit').write_text(str(code)+'\n');assert code==0,(out/'probe.log').read_text()[-12000:];assert re.search(r'test result: ok\. 1 passed; 0 failed;', (out/'probe.log').read_text())
q=json.loads(proof.read_text());assert q["result"]==("ACTUAL_K2_FORMATTING_OK" if a.all_languages else "ACTUAL_K2_KOTLIN_FORMATTING_OK") and q["disk_sources_unchanged"] and len(q["cases"])==(2 if a.all_languages else 1)
assert all(hashlib.sha256((project/f).read_bytes()).hexdigest()==h for f,h in sources.items()) and not marker.exists() and not(project/".wex/native").exists()
print("GENUINE_FORMATTING_PROBE_OK: actual Kotlin format and selection; Java requirement remains pending; current SDK/K2; no source writes/Gradle/APK/device")
