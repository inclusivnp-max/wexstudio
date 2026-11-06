#!/usr/bin/env python3
"""Real AGP + Kotlin + APK + cache + official K2 checks; no model/LSP mocks."""
import hashlib,json,os,subprocess,time,zipfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
PROJECT=ROOT/'tests/fixtures/android'
BIN=Path(os.environ.get('WEX_BIN',ROOT/'target/debug/wex'))
OUT=ROOT/'.verification/stage2';OUT.mkdir(parents=True,exist_ok=True)
results=[]

def cli(label,*args):
    start=time.monotonic()
    p=subprocess.run([str(BIN),*map(str,args)],cwd=ROOT,capture_output=True,text=True,timeout=300)
    (OUT/(label+'.stderr')).write_text(p.stderr)
    (OUT/(label+'.json')).write_text(p.stdout)
    if p.returncode:raise AssertionError(f'{label}: exit {p.returncode}\n{p.stderr[-5000:]}')
    result=json.loads(p.stdout)
    results.append({'case':label,'exit':p.returncode,'wall_ms':round((time.monotonic()-start)*1000),'cache_hit':result.get('cache_hit')})
    return result

def assert_models(result,variant='debug'):
    graph=result['graph'];assert graph['schema']==2
    modules={m['name']:m for m in graph['modules']};assert set(modules)=={':app',':shared'}
    assert modules[':app']['module_dependencies']==[':shared']
    assert any('core-1.13.0' in p and p.endswith('.jar') for p in modules[':app']['classpath']), modules[':app']['classpath']
    for m in modules.values():
        assert m['selected_variant']==variant
        assert set(m['variants'])=={'debug','release'}
        assert any(p.endswith('android.jar') and Path(p).is_file() for p in m['boot_classpath'])
        assert any('kotlin-stdlib' in p for p in m['classpath'])
        assert any(p.endswith('R.jar') for p in m['classpath'])
        assert any(p.endswith(f'/src/{variant}/kotlin') for p in m['source_roots'])
    assert any('buildConfig' in p for p in modules[':app']['generated_source_roots'])

try:
    p=subprocess.run([str(PROJECT/'gradlew'),'-p',str(PROJECT),':app:assembleDebug','--offline','--console=plain'],capture_output=True,text=True,timeout=300)
    (OUT/'real-apk-build.log').write_text(p.stdout+p.stderr);assert p.returncode==0,p.stderr[-5000:]
    apk=PROJECT/'app/build/outputs/apk/debug/app-debug.apk'
    with zipfile.ZipFile(apk) as z:
        assert {'AndroidManifest.xml','classes.dex','resources.arsc'}<=set(z.namelist())
    results.append({'case':'real-debug-apk','exit':0,'bytes':apk.stat().st_size,'sha256':hashlib.sha256(apk.read_bytes()).hexdigest()})
    first=cli('android-sync-miss','sync',PROJECT,'--force','--offline');assert first['cache_hit'] is False;assert_models(first)
    hit=cli('android-sync-hit-no-java','sync',PROJECT,'--bridge','/wex-test/bridge-must-not-start');assert hit['cache_hit'] is True
    source=PROJECT/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';original=source.read_bytes()
    try:
        source.write_bytes(original+b'\n// cache source edit test\n')
        hit=cli('android-source-edit-hit','sync',PROJECT,'--bridge','/wex-test/bridge-must-not-start');assert hit['cache_hit'] is True
    finally:source.write_bytes(original)
    build=PROJECT/'app/build.gradle.kts';original=build.read_bytes()
    try:
        build.write_bytes(original+b'\n// cache build edit test\n')
        miss=cli('android-build-edit-miss','sync',PROJECT,'--offline');assert miss['cache_hit'] is False
    finally:build.write_bytes(original)
    cli('android-restored-build-sync','sync',PROJECT,'--offline')
    settings=PROJECT/'settings.gradle.kts';original=settings.read_bytes()
    try:
        settings.write_bytes(original+b'\n// cache settings edit test\n')
        miss=cli('android-settings-edit-miss','sync',PROJECT,'--offline');assert miss['cache_hit'] is False
    finally:settings.write_bytes(original)
    cli('android-restored-settings-sync','sync',PROJECT,'--offline')
    catalog=PROJECT/'gradle/libs.versions.toml';assert not catalog.exists()
    try:
        catalog.write_text('[versions]\nwexTest = "1.0"\n')
        miss=cli('android-catalog-edit-miss','sync',PROJECT,'--offline');assert miss['cache_hit'] is False
    finally:catalog.unlink(missing_ok=True)
    cli('android-restored-catalog-sync','sync',PROJECT,'--offline')
    release=cli('android-release-model','sync',PROJECT,'--variant','release','--offline');assert_models(release,'release')
    hit=cli('android-release-hit-no-java','sync',PROJECT,'--variant','release','--bridge','/wex-test/bridge-must-not-start');assert hit['cache_hit'] is True
    debug=cli('android-debug-after-release','sync',PROJECT,'--bridge','/wex-test/bridge-must-not-start');assert debug['cache_hit'] is True;assert_models(debug)
    k2=cli('android-k2-real','k2-check',PROJECT)
    assert k2['result']=='K2_ANDROID_OK'
    assert {'greeting','greetingCount'}<=set(k2['completion_labels'])
    assert k2['disk_source_unchanged'] is True
    assert 'analysis.api.fir' in json.dumps(k2['introduced_error_diagnostics'])
    assert k2['clean_diagnostics']['items']==[] and k2['recovered_diagnostics']['items']==[]
    assert 'R.jar!' in json.dumps(k2['r_definition'])
    assert 'BuildConfig.java' in json.dumps(k2['build_config_definition'])
    report={'result':'ANDROID_E2E_OK','checks':results,'k2_result':k2['result'],'apk':str(apk)}
    (OUT/'android-e2e.json').write_text(json.dumps(report,indent=2)+'\n')
    print(f'ANDROID_E2E_OK: {len(results)} real-project checks; APK built; debug/release models; cache hit/miss; K2 FIR/completion/definitions')
except Exception:
    (OUT/'android-e2e-partial.json').write_text(json.dumps(results,indent=2)+'\n')
    raise
