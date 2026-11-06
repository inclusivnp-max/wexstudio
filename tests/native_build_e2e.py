#!/usr/bin/env python3
"""Real SDK/compiler tests on a project with no Gradle files; never deploys an APK."""
import json,os,subprocess,tempfile,zipfile,shutil,hashlib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
PROJECT=ROOT/'tests/fixtures/native-android'
BIN=Path(os.environ.get('WEX_BIN',ROOT/'target/debug/wex')).resolve()
OUT=ROOT/'.verification/stage4';OUT.mkdir(parents=True,exist_ok=True)
assert not list(PROJECT.rglob('gradlew')) and not list(PROJECT.rglob('*.gradle*'))
with tempfile.TemporaryDirectory() as directory:
    traps=Path(directory);marker=traps/'GRADLE_OR_MAVEN_EXECUTED'
    for name in ['gradle','gradlew','mvn','bridge']:
        p=traps/name;p.write_text('#!/bin/sh\nprintf forbidden > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
    env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ.get('PATH',''),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'GRADLE_USER_HOME':str(traps/'missing-gradle-home')}
    def build(label):
        result=subprocess.run([str(BIN),'build',str(PROJECT),'--module',':app','--offline'],env=env,text=True,capture_output=True,timeout=240)
        (OUT/(label+'.log')).write_text(result.stderr);(OUT/(label+'.exit')).write_text(str(result.returncode)+'\n')
        assert result.returncode==0,result.stderr
        x=json.loads(result.stdout);(OUT/(label+'.json')).write_text(result.stdout)
        assert x['backend']=='wex-native-v1' and x['kotlin_files']==2 and x['java_files']==1
        assert all(s['exit_code']==0 for s in x['steps'])
        names={s['name'] for s in x['steps']}
        for name in ['AAPT2 link + generated R','APK sign · debug key','Verify APK signature','Verify ZIP alignment']:assert name in names,name
        assert ('AAPT2 compile' in names)==any(r['compiled'] for r in x['resource_cache'])
        assert ('D8 · JVM bytecode → DEX' in names)==(x['dex_cache'][0]['state']!='hit')
        assert len(x['sdk_cache'])==1
        assert ('Android Java system image' in names)==(x['sdk_cache'][0]['state']!='hit')
        cached={r['module']:r['state'] for r in x['compilation_cache']}
        for module in ['shared','app']:
            assert cached[module] in ['hit','miss']
            assert ('Kotlin K2 · '+module in names)==(cached[module]=='miss')
        assert ('Javac · app' in names)==(cached['app']=='miss')
        verify=next(s for s in x['steps'] if s['name']=='Verify APK signature')['stdout']
        assert 'Verified using v2 scheme (APK Signature Scheme v2): true' in verify
        assert 'Verified using v3 scheme (APK Signature Scheme v3): true' in verify
        assert not marker.exists(),'Gradle/Maven was invoked'
        return x
    first=build('native-e2e-first');apk=Path(first['apk']['path'])
    with zipfile.ZipFile(apk) as z:
        assert z.read('assets/wex-native-proof.txt')==b'native-asset-ok\n'
        dex=b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'))
        for expected in [b'Ldev/wex/android/JavaGreeting;',b'Ldev/wex/android/MainActivity;',b'Ldev/wex/shared/Greeter;',b'WEX_NATIVE_OK']:assert expected in dex,expected
        assert b'Lorg/jetbrains/kotlin/cli/jvm/K2JVMCompiler;' not in dex
        assert 'resources.arsc' in z.namelist() and 'AndroidManifest.xml' in z.namelist()
    sdk=Path(os.environ.get('ANDROID_HOME',str(Path.home()/'Android/Sdk')));aapt=sdk/'build-tools/36.0.0/aapt2'
    badging=subprocess.run([str(aapt),'dump','badging',str(apk)],text=True,capture_output=True,check=True).stdout
    (OUT/'native-badging.txt').write_text(badging)
    assert "package: name='dev.wex.android.integration'" in badging and "minSdkVersion:'26'" in badging
    xml=subprocess.run([str(aapt),'dump','xmltree','--file','AndroidManifest.xml',str(apk)],text=True,capture_output=True,check=True).stdout
    (OUT/'native-manifest.txt').write_text(xml)
    for value in ['dev.wex.android.MainActivity','androidx.core.app.CoreComponentFactory','dev.wex.android.integration.androidx-startup','DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION']:assert value in xml,value
    second=build('native-e2e-second')
    assert first['blake3']==second['blake3'],'repeated build is not byte-identical'
    pointer=PROJECT/'.wex/native/debug/current.json';before=pointer.read_bytes()
    source=PROJECT/'app/src/main/java/dev/wex/android/JavaGreeting.java';original=source.read_bytes()
    try:
        source.write_text('this is deliberately invalid Java;\n')
        fail=subprocess.run([str(BIN),'build',str(PROJECT),'--offline'],env=env,text=True,capture_output=True,timeout=240)
        (OUT/'native-invalid-java.log').write_text(fail.stderr)
        assert fail.returncode!=0 and pointer.read_bytes()==before
        assert not marker.exists()
    finally:source.write_bytes(original)
    tampered=traps/'tampered.apk'
    with zipfile.ZipFile(Path(second['apk']['path'])) as src,zipfile.ZipFile(tampered,'w') as dest:
        for info in src.infolist():dest.writestr(info,src.read(info.filename)+ (b'changed' if info.filename=='assets/wex-native-proof.txt' else b''))
    verify=subprocess.run([str(sdk/'build-tools/36.0.0/apksigner'),'verify',str(tampered)],text=True,capture_output=True)
    (OUT/'native-tamper.log').write_text(verify.stdout+verify.stderr)
    assert verify.returncode!=0
    print('NATIVE_APK_E2E_OK: Kotlin+Java; AAR/resources/manifests/assets; DEX; v2/v3 signature; alignment; repeat-identical APK; failed build preserves prior artifact; tamper rejected; no Gradle/Maven')
