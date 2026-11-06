#!/usr/bin/env python3
"""Real release signing/variants, isolated keys, SDK verification, optional explicit VM Run."""
from pathlib import Path
import hashlib,json,os,re,secrets,shutil,subprocess,sys,tempfile,time
ROOT=Path(__file__).resolve().parents[1];SOURCE=Path(sys.argv[1]).resolve();OUT=Path(sys.argv[2]).resolve();OUT.mkdir(parents=True,exist_ok=True)
serial=sys.argv[3] if len(sys.argv)>3 else None
BIN=Path(os.environ.get('WEX_BIN',ROOT/'target/debug/wex'));IDE=Path(os.environ.get('WEX_IDE',ROOT/'target/debug/wex-ide'))
SDK=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'));JAVA=Path(os.environ.get('JAVA_HOME','/usr/lib/jvm/java-21-openjdk'))/'bin'
original={p:hashlib.sha256(p.read_bytes()).hexdigest() for p in SOURCE.rglob('*') if p.is_file() and '.wex' not in p.parts}
project=OUT/'project'
if project.exists():shutil.rmtree(project)
shutil.copytree(SOURCE,project,ignore=shutil.ignore_patterns('.wex'))
config=json.loads((project/'wex.json').read_text());config['android']['application_id']+='.releaseverify'+hashlib.sha256(str(OUT).encode()).hexdigest()[:8]
main=next(p for p in project.rglob('MainActivity.kt') if '.wex' not in p.parts)
main.write_text(main.read_text().replace('        Log.i("WexRun", "WEX_RUN_OK', '        Log.i("WexRun", "WEX_RELEASE_OK debug=${BuildConfig.DEBUG} runId=$runId")\n        Log.i("WexRun", "WEX_RUN_OK',1))
package=config['android']['application_id'];records={};all_output=[]
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
with tempfile.TemporaryDirectory(prefix='wex-release-keys-') as private:
    private=Path(private);store=secrets.token_urlsafe(24);key=secrets.token_urlsafe(24)
    env={**os.environ,'WEX_RELEASE_TEST_STORE':store,'WEX_RELEASE_TEST_KEY':key,'WEX_DEBUG_KEYSTORE':str(private/'debug.jks')}
    traps=private/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
    for name in ['gradle','gradlew','mvn','bridge','adb']:
        p=traps/name;p.write_text('#!/bin/sh\nprintf forbidden > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
    env['PATH']=str(traps)+os.pathsep+env.get('PATH','');env['WEX_GRADLE_BRIDGE']=str(traps/'bridge')
    keystore=private/'release.jks'
    create=subprocess.run([str(JAVA/'keytool'),'-genkeypair','-storetype','JKS','-keystore',str(keystore),'-alias','release','-storepass:env','WEX_RELEASE_TEST_STORE','-keypass:env','WEX_RELEASE_TEST_KEY','-keyalg','RSA','-keysize','2048','-validity','3650','-dname','CN=Wex Release Integration'],env=env,capture_output=True,text=True)
    assert create.returncode==0,create.stderr
    def keytool(*args):
        result=subprocess.run([str(JAVA/'keytool'),*map(str,args)],env=env,capture_output=True,text=True)
        assert result.returncode==0,result.stderr
        assert store not in result.stdout+result.stderr and key not in result.stdout+result.stderr
        return result
    ca=private/'ca.jks';ca_pem=private/'ca.pem';csr=private/'leaf.csr';issued=private/'leaf.pem'
    keytool('-genkeypair','-storetype','JKS','-keystore',ca,'-alias','ca','-storepass:env','WEX_RELEASE_TEST_STORE','-keypass:env','WEX_RELEASE_TEST_KEY','-keyalg','RSA','-keysize','2048','-validity','3650','-dname','CN=Wex Integration CA','-ext','bc:c')
    keytool('-exportcert','-rfc','-keystore',ca,'-alias','ca','-storepass:env','WEX_RELEASE_TEST_STORE','-file',ca_pem)
    keytool('-certreq','-keystore',keystore,'-alias','release','-storepass:env','WEX_RELEASE_TEST_STORE','-keypass:env','WEX_RELEASE_TEST_KEY','-file',csr)
    keytool('-gencert','-rfc','-keystore',ca,'-alias','ca','-storepass:env','WEX_RELEASE_TEST_STORE','-keypass:env','WEX_RELEASE_TEST_KEY','-infile',csr,'-outfile',issued,'-validity','3650')
    keytool('-importcert','-noprompt','-keystore',keystore,'-alias','ca','-storepass:env','WEX_RELEASE_TEST_STORE','-file',ca_pem)
    keytool('-importcert','-noprompt','-keystore',keystore,'-alias','release','-storepass:env','WEX_RELEASE_TEST_STORE','-keypass:env','WEX_RELEASE_TEST_KEY','-file',issued)
    shutil.copy2(ca_pem,OUT/'release-ca.pem');shutil.copy2(issued,OUT/'release-leaf.pem')
    listing=subprocess.check_output([str(JAVA/'keytool'),'-J-Duser.language=en','-list','-v','-keystore',str(keystore),'-alias','release','-storepass:env','WEX_RELEASE_TEST_STORE'],env=env,text=True,stderr=subprocess.DEVNULL)
    assert 'Certificate chain length: 2' in listing
    pin=re.search(r'SHA256:\s*([A-Fa-f0-9:]+)',listing)[1].replace(':','').lower();key_before=sha(keystore)
    config['build']['release_signing']=dict(keystore=str(keystore),key_alias='release',store_password_env='WEX_RELEASE_TEST_STORE',key_password_env='WEX_RELEASE_TEST_KEY',certificate_sha256=pin)
    raw=json.dumps(config,indent=2)+'\n';(project/'wex.json').write_text(raw)
    def cmd(label,args,environment=env,ok=True):
        r=subprocess.run([str(BIN),*map(str,args)],env=environment,capture_output=True,text=True,timeout=360)
        assert store not in r.stdout+r.stderr and key not in r.stdout+r.stderr,'credential leaked in captured CLI output'
        all_output.extend([r.stdout,r.stderr]);(OUT/(label+'.log')).write_text(r.stderr);(OUT/(label+'.stdout')).write_text(r.stdout);(OUT/(label+'.exit')).write_text(str(r.returncode)+'\n')
        assert (r.returncode==0)==ok,(label,r.returncode,r.stderr[-6000:]);return r
    cmd('resolve',['resolve',project,'--offline'])
    def build(label,variant,environment=env):
        r=cmd(label,['build',project,'--variant',variant,'--offline'],environment);report=json.loads(r.stdout);records[label]=report
        assert report['variant']==variant and report['debuggable']==(variant=='debug')
        assert report['signing']['kind']==variant
        assert report['compose']['modules']==['shared','app']
        d8=[s for s in report['steps'] if s['name'].startswith('D8')]
        assert bool(d8)==(report['dex_cache'][0]['state']!='hit')
        if d8:assert ('--debug' if variant=='debug' else '--release') in d8[0]['args']
        apk=Path(report['apk']['path']);assert apk.is_file();shutil.copy2(apk,OUT/(label+'.apk'))
        verified=subprocess.check_output([str(SDK/'build-tools/36.0.0/apksigner'),'verify','--verbose','--print-certs',str(apk)],text=True,env={k:v for k,v in env.items() if k not in ['WEX_RELEASE_TEST_STORE','WEX_RELEASE_TEST_KEY']})
        assert 'Verified using v2 scheme (APK Signature Scheme v2): true' in verified and 'Verified using v3 scheme (APK Signature Scheme v3): true' in verified
        (OUT/(label+'-signature.txt')).write_text(verified)
        xml=subprocess.check_output([str(SDK/'build-tools/36.0.0/aapt2'),'dump','xmltree',str(apk),'--file','AndroidManifest.xml'],text=True)
        assert re.search(r'android:debuggable[^=]*='+('true' if variant=='debug' else 'false')+r'\b',xml),xml
        (OUT/(label+'-manifest.txt')).write_text(xml)
        generated=Path(report['build_dir'])/'module-generated/app/dev/wex/compose/BuildConfig.java'
        assert ('DEBUG = true' if variant=='debug' else 'DEBUG = false') in generated.read_text()
        shutil.copy2(generated,OUT/(label+'-BuildConfig.java'))
        if variant=='release':assert report['signing']['certificate_sha256']==pin and not any('Create persistent debug' in s['name'] for s in report['steps'])
        return report
    noenv=env.copy();noenv.pop('WEX_RELEASE_TEST_STORE');noenv.pop('WEX_RELEASE_TEST_KEY')
    debug=build('debug','debug',noenv);debug_pointer=project/'.wex/native/debug/current.json';debug_hash=sha(debug_pointer)
    release=build('release','release');pointer=project/'.wex/native/release/current.json';previous=sha(pointer);apk_hash=sha(Path(release['apk']['path']))
    repeat=build('repeat','release');assert repeat['blake3']==release['blake3'];assert sha(debug_pointer)==debug_hash
    previous=sha(pointer)
    failures={}
    for label,override,environment in [('missing-env',None,noenv),('wrong-store-password',None,{**env,'WEX_RELEASE_TEST_STORE':'wrong-'+store}),('wrong-key-password',None,{**env,'WEX_RELEASE_TEST_KEY':'wrong-'+key}),('wrong-pin',{'certificate_sha256':'00'*32},env),('missing-alias',{'key_alias':'missing'},env),('missing-keystore',{'keystore':str(private/'absent.jks')},env)]:
        invalid=json.loads(raw)
        if override:invalid['build']['release_signing'].update(override)
        (project/'wex.json').write_text(json.dumps(invalid,indent=2)+'\n')
        try:
            r=cmd(label,['build',project,'--variant','release','--offline'],environment,False);failures[label]=dict(exit=r.returncode,prior_pointer_preserved=sha(pointer)==previous,prior_apk_preserved=sha(Path(release['apk']['path']))==apk_hash)
            assert failures[label]['prior_pointer_preserved'] and failures[label]['prior_apk_preserved']
            if label!='wrong-key-password':assert 'AAPT2' not in r.stderr,'preflight failure entered compilation'
        finally:(project/'wex.json').write_text(raw)
    assert sha(keystore)==key_before and not (private/'absent.jks').exists()
    for variant in ['debug','release']:
        semantic_env={**env,'WEX_K2_SYSTEM_DIR':str(OUT/(variant+'-k2-system')),'WEX_LSP_TRACE':str(OUT/(variant+'-k2-trace.jsonl'))}
        result=cmd(variant+'-k2',['k2-check',project,'--variant',variant],semantic_env);semantic=json.loads(result.stdout)
        assert semantic['result']=='K2_COMPOSE_OK' and semantic['variant']==variant and semantic['disk_source_unchanged']
        assert 'BuildConfig' in json.dumps(semantic['build_config_definition'])
        assert '/'+variant+'/' in json.dumps(semantic['build_config_definition']),semantic['build_config_definition']
    device=None;gui=None
    if serial:
        assert serial.startswith('emulator-'),'this suite is explicitly mtvm-only'
        sys.path.insert(0,str(ROOT/'tests'));from adb_direct import Adb
        adb=Adb(serial);assert adb.shell('getprop','ro.kernel.qemu')['stdout'].strip()=='1'
        try:
            result=subprocess.run([sys.executable,str(ROOT/'tests/compose_device_e2e.py'),str(project),str(OUT/'device'),serial,'release'],env=env,capture_output=True,text=True,timeout=420)
            assert store not in result.stdout+result.stderr and key not in result.stdout+result.stderr
            (OUT/'device.log').write_text(result.stdout+result.stderr);(OUT/'device.exit').write_text(str(result.returncode)+'\n');assert result.returncode==0,result.stderr[-6000:]
            device=json.loads((OUT/'device/compose-device.json').read_text());assert device['run']['deployment']['native_build']['signing']['certificate_sha256']==pin
            # Existing device suite stops the app; IDE Run performs an independent rebuild/deploy.
            env_ui={**env,'WEX_K2_SYSTEM_DIR':str(OUT/'gui-k2-system'),'WEX_LSP_TRACE':str(OUT/'gui-k2-trace.jsonl')};env_ui.pop('WAYLAND_DISPLAY',None)
            result=subprocess.run([str(IDE),str(project),'--run-smoke',str(OUT/'release-ide.png'),'--serial',serial,'--variant','release'],env=env_ui,capture_output=True,text=True,timeout=300)
            assert store not in result.stdout+result.stderr and key not in result.stdout+result.stderr
            (OUT/'gui.log').write_text(result.stdout+result.stderr);(OUT/'gui.exit').write_text(str(result.returncode)+'\n');assert result.returncode==0,result.stderr[-6000:]
            gui=json.loads((OUT/'release-ide.json').read_text());run=gui['deployment']
            assert gui['variant_picker_clicked'] and gui['selected_variant']=='release' and gui['run_button_clicked']
            assert Path(gui['variant_picker_screenshot']).read_bytes().startswith(b'\x89PNG\r\n\x1a\n')
            assert run['serial']==serial and run['variant']=='release' and not run['native_build']['debuggable']
            assert run['native_build']['signing']['certificate_sha256']==pin and gui['k2_diagnostics_observed']
            logs=adb.shell('logcat','-d','-v','brief','--pid',str(run['pid']),'-s','WexRun')['stdout']
            assert 'WEX_RELEASE_OK debug=false runId='+run['run_id'] in logs
            denied=adb.shell('run-as',package,'id',check=False);assert denied['exit_code']!=0 and 'not debuggable' in denied['stderr']+denied['stdout']
            (OUT/'runtime-release-logcat.txt').write_text(logs);(OUT/'run-as-denied.json').write_text(json.dumps(denied,indent=2)+'\n')
            stopped=adb.shell('am','force-stop','--user','current',package)
            for _ in range(50):
                if not adb.shell('pidof',package,check=False)['stdout'].strip():break
                time.sleep(.1)
            assert not adb.shell('pidof',package,check=False)['stdout'].strip()
            gui['stop']={'shell':stopped,'pid_gone':True};(OUT/'release-ide.json').write_text(json.dumps(gui,indent=2)+'\n')
        finally:
            adb.shell('am','force-stop','--user','current',package,check=False)
            adb.shell('pm','uninstall',package,check=False)
    assert not marker.exists()
    for p,h in original.items():assert sha(p)==h,p
    # Search persisted diagnostics/receipts/configs, not encrypted keystore/APK bytes.
    for p in OUT.rglob('*'):
        if p.is_file() and p.suffix in ['.json','.jsonl','.log','.stdout','.txt','.xml','.java']:
            data=p.read_text(errors='replace');assert store not in data and key not in data,'persisted credential leaked'
    report=dict(result='NATIVE_RELEASE_OK',certificate_chain_length=2,certificate_sha256=pin,debug_certificate=debug['signing']['certificate_sha256'],builds=records,failures=failures,secrets_not_persisted=True,debug_independent_of_release_secrets=True,keystore_unchanged=True,disk_source_unchanged=True,explicit_serial=serial,device_verified=device is not None,gui_verified=gui is not None)
    (OUT/'release-suite.json').write_text(json.dumps(report,indent=2)+'\n')
    print('NATIVE_RELEASE_OK: configured key/certificate pin; debuggable=false/BuildConfig false/D8 release; repeat-identical v2/v3 APK; 6 failure paths preserve prior APK; no credentials in diagnostics; debug independent; K2 debug/release; '+('mtvm '+serial+' + real variant-picker/Run/Logcat/run-as denied/Stop' if serial else 'no device test')+'; no Gradle/Maven')
