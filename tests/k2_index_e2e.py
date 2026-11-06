#!/usr/bin/env python3
"""Genuine stopped K2 RocksDB-profile reuse, source refresh and tamper recovery."""
from pathlib import Path
import argparse,hashlib,json,os,re,shutil,subprocess,sys
from native_window_process import run_window
p=argparse.ArgumentParser();p.add_argument('output',type=Path);a=p.parse_args();r=Path(__file__).resolve().parents[1];out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);project=out/'project';shutil.copytree(r/'tests/fixtures/native-android',project,ignore=shutil.ignore_patterns('.wex'));(project/'INDEX_FIXTURE').write_text('disposable genuine K2 persistent profile fixture\n')
traps=out/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 f=traps/name;f.write_text('#!/bin/sh\nprintf '+name+' > "'+str(marker)+'"\nexit 99\n');f.chmod(0o755)
proof=out/'genuine-k2.json';binary=Path(os.environ.get('WEX_BIN',r/'target/debug/wex')).resolve();server=r/'.wex/tools/kotlin-lsp/kotlin-server-263.6379.0/bin/intellij-server'
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(r/'.wex'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'JAVA_HOME':'/usr/lib/jvm/java-21-openjdk','ANDROID_HOME':'/home/zapahtravki/Android/Sdk','IJ_JAVA_OPTIONS':'-XX:ActiveProcessorCount=2 -Xmx1024m','WEX_K2_INDEX_PROJECT':str(project),'WEX_K2_INDEX_OUTPUT':str(proof),'WEX_KOTLIN_LSP':str(server),'WEX_K2_SYSTEM_DIR':str(out/'k2-system'),'WEX_LSP_TRACE':str(out/'lsp.jsonl'),'CARGO_BUILD_JOBS':'1','RUST_TEST_THREADS':'1'}
x=subprocess.run([binary,'resolve',project,'--offline'],env=env,text=True,capture_output=True,timeout=180);(out/'resolve.json').write_text(x.stdout);(out/'resolve.log').write_text(x.stderr);assert x.returncode==0,x.stderr
sources={str(f.relative_to(project)):hashlib.sha256(f.read_bytes()).hexdigest()for f in project.rglob('*')if f.is_file()and'.wex'not in f.parts}
(out/'execution.json').write_text(json.dumps({'resolver_binary':str(binary),'resolver_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'resolver_only':True,'actual_probe':'current library test ELF (Cargo-built, directly executed)','original_sources':sources,'JVM_options':env['IJ_JAVA_OPTIONS']},indent=2)+'\n')
build=run_window(['cargo','test','--lib','--features','desktop','--locked','--no-run','--message-format=json'],env,out/'build.jsonl',out/'build-process.json',300,stderr_path=out/'build.log');assert build==0,(out/'build.log').read_text()[-6000:]
artifacts=[json.loads(line) for line in (out/'build.jsonl').read_text().splitlines() if line.startswith('{')]
executables=[Path(q['executable']) for q in artifacts if q.get('reason')=='compiler-artifact'and q.get('executable')and q.get('target',{}).get('name')=='wexandroidstudio'and q.get('profile',{}).get('test')]
assert len(executables)==1,executables
probe_binary=executables[0]
# Cargo extends LD_LIBRARY_PATH for `cargo test`; launch this actual test ELF
# directly under the unchanged production-parent environment, not that injected
# toolchain loader context. No host variable is removed or made artificially empty.
(out/'test-binary.json').write_text(json.dumps({'path':str(probe_binary),'sha256':hashlib.sha256(probe_binary.read_bytes()).hexdigest(),'direct_production_parent_environment':True,'parent_LD_LIBRARY_PATH_present':bool(env.get('LD_LIBRARY_PATH')),'parent_LD_PRELOAD_present':bool(env.get('LD_PRELOAD'))},indent=2)+'\n')
code=run_window([probe_binary,'genuine_k2_profiles_reuse_and_refresh_sources','--ignored','--nocapture'],env,out/'probe.log',out/'process.json',600);(out/'probe.exit').write_text(str(code)+'\n');assert code==0,(out/'probe.log').read_text()[-12000:];assert re.search(r'test result: ok\. 1 passed; 0 failed;', (out/'probe.log').read_text())
q=json.loads(proof.read_text());assert q['result']=='GENUINE_K2_INDEX_OK'and len(q['rounds'])==3 and [s['profile']['status']for s in q['rounds']]==['miss','hit','miss']
assert all(q[k]for k in ['same_workspace_on_hit','dirty_previous_session_did_not_leak','fresh_closed_source_definition','damaged_profile_quarantined','final_source_hashes_restored','main_disk_never_written','existing_closed_source_same_size_mtime_refreshed','same_context_key_all_rounds','no_apk_or_device'])
assert all(hashlib.sha256((project/f).read_bytes()).hexdigest()==s for f,s in sources.items())and not marker.exists()and not(project/'.wex/native').exists()
remaining=json.loads((out/'process.json').read_text())['remaining'];assert not remaining
(out/'k2-index-e2e.json').write_text(json.dumps({'result':'ACTUAL_K2_INDEX_REUSE_OK','probe':q,'source_hashes_verified':True,'no_Gradle_APK_device':True,'remaining':remaining,'no_universal_cold_latency_claim':True},indent=2)+'\n')
print('ACTUAL_K2_INDEX_REUSE_OK: actual cold/warm/tamper-recovery; persistent RocksDB/workspace; new closed-source definition; no prior dirty declaration leakage; genuine diagnostics/restore; same-size+mtime closed source refreshed; final source bytes restored; no Gradle/APK/device',flush=True)
