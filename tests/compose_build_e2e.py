#!/usr/bin/env python3
"""Real offline Compose compiler/SDK regression; no Gradle/Maven, no device mutation."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,tempfile,zipfile
ROOT=Path(__file__).resolve().parents[1]
assert len(sys.argv)==3,'Usage: compose_build_e2e.py PROJECT OUTPUT'
project=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve();out.mkdir(parents=True,exist_ok=True)
binary=Path(os.environ.get('WEX_BIN',ROOT/'target/debug/wex')).resolve()
cache=Path(os.environ.get('WEX_MAVEN_CACHE',Path.home()/'.cache/wex/maven')).resolve()
config=json.loads((project/'wex.json').read_text());version=config['build']['kotlin_version']
assert config['android_variants'] and config['build']['compose']['reports']
assert not list(project.rglob('gradlew')) and not list(project.rglob('*.gradle*'))
sources={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in project.rglob('*') if p.suffix in ('.kt','.java') and '.wex' not in p.parts}
with tempfile.TemporaryDirectory(prefix='wex-compose-verify-') as folder:
    folder=Path(folder);traps=folder/'traps';traps.mkdir();marker=folder/'FORBIDDEN_EXECUTED'
    for name in ['gradle','gradlew','mvn','bridge']:
        p=traps/name;p.write_text('#!/bin/sh\nprintf forbidden > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
    env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ.get('PATH',''),'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
    def command(label,args,environment=env):
        result=subprocess.run([str(binary),*map(str,args)],env=environment,text=True,capture_output=True,timeout=420)
        (out/(label+'.stdout')).write_text(result.stdout);(out/(label+'.log')).write_text(result.stderr);(out/(label+'.exit')).write_text(str(result.returncode)+'\n')
        return result
    locked=command('locked-without-executables',['resolve',project,'--offline','--locked'],{**env,'PATH':'/nonexistent'})
    assert locked.returncode==0,locked.stderr
    lock=json.loads(locked.stdout);assert lock['mediation']=='android-jvm-module-v1/highest-reachable-fixed-v2/project-bom-pins-v1'
    selected={a['coordinate']['group']+':'+a['coordinate']['artifact']:a['coordinate']['version'] for a in lock['artifacts']}
    assert selected['androidx.startup:startup-runtime']!='1.0.0'
    assert selected['androidx.compose.runtime:runtime-android']=='1.9.4'
    assert 'androidx.compose.runtime:runtime-annotation-jvm' not in selected
    assert any(f['path'].endswith('.module') for f in lock['files'])
    def build(label,root):
        result=command(label,['build',root,'--offline']);assert result.returncode==0,result.stderr[-5000:]
        report=json.loads(result.stdout);(out/(label+'.json')).write_text(json.dumps(report,indent=2)+'\n')
        assert report['backend']=='wex-native-v1' and report['kotlin_files']==2 and report['java_files']==1
        assert all(s['exit_code']==0 for s in report['steps'])
        info=report['compose'];assert info['compiler_version']==version and info['modules']==['shared','app']
        assert Path(info['plugin_jar']).name==f'kotlin-compose-compiler-plugin-embeddable-{version}.jar'
        for name in ['shared','app']:
            step=next(s for s in report['steps'] if s['name']=='Kotlin K2 · '+name);args=step['args']
            assert '-Xplugin='+info['plugin_jar'] in args
            assert info['plugin_jar'] not in args[args.index('-classpath')+1]
            assert f"plugin:androidx.compose.compiler.plugins.kotlin:sourceInformation={str(info['source_information']).lower()}" in args
            metrics=Path(report['build_dir'])/'compose'/name/'metrics'/(name+'-module.json')
            if info['reports']:
                assert metrics.exists();m=json.loads(metrics.read_text());assert m['totalComposables']>=1
                assert m['featureFlags']['StrongSkipping']==info['strong_skipping']
                reports=list((metrics.parent.parent/'reports').glob('*-composables.txt'));assert reports
                shutil.copy2(metrics,out/(label+'-'+name+'-metrics.json'))
                for text in reports:shutil.copy2(text,out/(label+'-'+name+'-'+text.name))
            else:assert not metrics.parent.parent.exists()
        signature=next(s for s in report['steps'] if s['name']=='Verify APK signature')['stdout']
        assert 'Verified using v2 scheme (APK Signature Scheme v2): true' in signature
        assert 'Verified using v3 scheme (APK Signature Scheme v3): true' in signature
        graph=json.loads(Path(report['graph_file']).read_text());assert all(info['plugin_jar'] not in m['classpath'] for m in graph['modules'])
        assert not marker.exists()
        return report
    first=build('first',project);second=build('repeat',project)
    assert first['blake3']==second['blake3'],'Compose APK must be byte-identical across repeated builds'
    javap=Path(os.environ.get('JAVA_HOME','/usr/lib/jvm/java-21-openjdk'))/'bin/javap'
    def bytecode(report,label):
        result=subprocess.run([str(javap),'-classpath',str(Path(report['build_dir'])/'classes/shared'),'-c','-p','dev.wex.compose.shared.CounterCardKt'],capture_output=True,text=True,check=True)
        (out/(label+'-bytecode.txt')).write_text(result.stdout);return result.stdout
    lowered=bytecode(first,'first')
    for token in ['androidx.compose.runtime.Composer','Composer.startRestartGroup','ComposerKt.sourceInformation:','CounterLabel.value']:assert token in lowered,token
    with zipfile.ZipFile(first['apk']['path']) as apk:
        dex=b''.join(apk.read(n) for n in apk.namelist() if n.endswith('.dex'))
        for descriptor in [b'Ldev/wex/compose/MainActivity;',b'Ldev/wex/compose/shared/CounterCardKt;',b'Ldev/wex/compose/shared/CounterLabel;',b'Landroidx/compose/runtime/Composer;']:assert descriptor in dex,descriptor
        assert b'Lorg/jetbrains/kotlin/cli/jvm/K2JVMCompiler;' not in dex
        assert b'Landroidx/compose/compiler/' not in dex
    optional=folder/'optional';shutil.copytree(project,optional,ignore=shutil.ignore_patterns('.wex','build','__pycache__'))
    options=json.loads((optional/'wex.json').read_text());options['build']['compose'].update(source_information=False,strong_skipping=False)
    (optional/'wex.json').write_text(json.dumps(options,indent=2)+'\n')
    result=command('optional-resolve',['resolve',optional,'--offline']);assert result.returncode==0,result.stderr
    disabled=build('optional-flags',optional);assert 'ComposerKt.sourceInformation:' not in bytecode(disabled,'optional')
    # Reports are an independent switch, not a prerequisite for Compose lowering.
    options['build']['compose']['reports']=False;(optional/'wex.json').write_text(json.dumps(options,indent=2)+'\n')
    result=command('no-reports-resolve',['resolve',optional,'--offline']);assert result.returncode==0,result.stderr
    no_reports=build('no-reports',optional)
    assert 'Composer.startRestartGroup' in bytecode(no_reports,'no-reports')
    # Corrupt only a private plugin copy. Every other cache entry is read-only via symlink.
    toolchain=Path.home()/'.cache/wex/toolchains'/f'kotlin-{version}-compose'
    tools=json.loads((toolchain/'wex.lock.json').read_text());mirror=folder/'cache'
    plugin_relative=next(a['file'] for a in tools['artifacts'] if a['coordinate']['artifact']=='kotlin-compose-compiler-plugin-embeddable')
    original_plugin=cache/plugin_relative;original_digest=hashlib.sha256(original_plugin.read_bytes()).hexdigest()
    for entry in {f['path'] for f in [*lock['files'],*tools['files']]}:
        dest=mirror/entry;dest.parent.mkdir(parents=True,exist_ok=True)
        if entry==plugin_relative:dest.write_bytes((cache/entry).read_bytes()+b'WEX_PRIVATE_TAMPER_TEST')
        else:dest.symlink_to(cache/entry)
    pointer=project/'.wex/native/debug/current.json';before=pointer.read_bytes()
    corrupt=command('private-plugin-tamper',['build',project,'--offline'],{**env,'WEX_MAVEN_CACHE':str(mirror)})
    assert corrupt.returncode!=0 and 'locked artifact hash mismatch' in corrupt.stderr
    assert pointer.read_bytes()==before,'failed plugin verification published an APK'
    assert hashlib.sha256(original_plugin.read_bytes()).hexdigest()==original_digest
    for p,h in sources.items():assert hashlib.sha256(Path(p).read_bytes()).hexdigest()==h,p
    assert not marker.exists()
    report={'result':'COMPOSE_BUILD_OK','project':str(project),'apk':first['apk']['path'],'blake3':first['blake3'],'plugin':first['compose'],'selected_dependencies':selected,'repeat_identical':True,'actual_lowering_verified':True,'optional_flags_verified':True,'reports_independent':True,'plugin_not_in_dex_or_app_classpath':True,'private_tamper_rejected':True,'previous_apk_preserved':True,'global_cache_unchanged':True,'sources_unchanged':True,'no_gradle_or_maven':True}
    (out/'compose-build.json').write_text(json.dumps(report,indent=2)+'\n')
    print('COMPOSE_BUILD_OK: matching Kotlin plugin in both modules; Composer lowering/metrics; optional source info/StrongSkipping/reports; repeated signed APK identical; no compiler in DEX; private plugin tamper rejected; old APK/cache/sources preserved; no Gradle/Maven')
