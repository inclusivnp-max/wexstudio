#!/usr/bin/env python3
"""Actual native Compose APK -> direct ADB -> frame -> tap/recomposition -> Stop."""
from pathlib import Path
import hashlib,json,os,re,struct,subprocess,sys,tempfile,time,xml.etree.ElementTree as ET
from adb_direct import Adb
ROOT=Path(__file__).resolve().parents[1]
assert len(sys.argv) in [4,5],'Usage: compose_device_e2e.py PROJECT OUTPUT EXPLICIT_SERIAL [debug|release]'
project=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve();serial=sys.argv[3]
assert serial.startswith('emulator-') and serial[9:].isdigit(),'explicit mtvm emulator serial required'
out.mkdir(parents=True,exist_ok=True)
binary=Path(os.environ.get('WEX_BIN',ROOT/'target/debug/wex')).resolve()
assert not list(project.rglob('*.gradle*')) and not list(project.rglob('gradlew'))
sources={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in project.rglob('*') if p.suffix in ('.kt','.java') and '.wex' not in p.parts}
variant=sys.argv[4] if len(sys.argv)>4 else 'debug';assert variant in ['debug','release']
adb=Adb(serial)
assert adb.shell('getprop','ro.kernel.qemu')['stdout'].strip()=='1','expected actual emulator'
package=json.loads((project/'wex.json').read_text())['android']['application_id']
remote='/data/local/tmp/wex-compose-test.xml'
with tempfile.TemporaryDirectory() as tmp:
    traps=Path(tmp);marker=traps/'GRADLE_OR_MAVEN_EXECUTED'
    for name in ['gradle','gradlew','mvn','bridge']:
        p=traps/name;p.write_text('#!/bin/sh\nprintf forbidden > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
    command=[str(binary),'run',str(project),'--serial',serial,'--variant',variant,'--offline','--log-seconds','12','--require-marker','WEX_COMPOSE_FRAME count=0']
    env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ.get('PATH',''),'WEX_GRADLE_BRIDGE':str(traps/'bridge')}
    try:
        result=subprocess.run(command,env=env,text=True,capture_output=True,timeout=360)
        (out/'run.log').write_text(result.stderr);(out/'run.stdout').write_text(result.stdout);(out/'run.exit').write_text(str(result.returncode)+'\n');(out/'run-command.json').write_text(json.dumps(command,indent=2)+'\n')
        assert result.returncode==0,result.stderr[-6000:]
        receipt=json.loads(result.stdout);run=receipt['deployment'];package=run['apk']['application_id']
        assert run['serial']==serial and run['device']['emulator'] and run['build_backend']=='wex-native-v1'
        assert run['variant']==variant and run['native_build']['debuggable']==(variant=='debug')
        assert run['install']['output']['exit_code']==0 and receipt['logcat']['marker_found']
        assert run['native_build']['compose']['modules']==['shared','app']
        assert all(s['exit_code']==0 for s in run['native_build']['steps'])
        def hierarchy(label, count):
            deadline=time.monotonic()+45
            while True:
                adb.shell('uiautomator','dump',remote)
                xml=adb.execute('cat',remote);tree=ET.fromstring(xml)
                if any(n.get('text')==f'Count: {count}' for n in tree.iter('node')):
                    (out/(label+'.xml')).write_bytes(xml);return tree
                assert time.monotonic()<deadline,xml.decode(errors='replace')
                time.sleep(.3)
        def screenshot(label):
            b=adb.execute('screencap','-p');assert b[:8]==b'\x89PNG\r\n\x1a\n'
            w,h=struct.unpack('>II',b[16:24]);assert w>=320 and h>=480
            path=out/(label+'.png');path.write_bytes(b);return {'path':str(path),'width':w,'height':h,'sha256':hashlib.sha256(b).hexdigest()}
        before=hierarchy('01-compose-counter',0);shots=[screenshot('01-compose-counter')]
        matches=[n for n in before.iter('node') if n.get('content-desc')=='wex-increment']
        assert len(matches)==1,'expected unique increment semantics'
        node=matches[0];parents={child:parent for parent in before.iter() for child in parent}
        while node.get('clickable')!='true':
            assert node in parents,'increment semantics has no clickable ancestor'
            node=parents[node]
        assert node.get('package')==package and node.get('enabled')=='true'

        x1,y1,x2,y2=map(int,re.findall(r'\d+',node.get('bounds')));x,y=(x1+x2)//2,(y1+y2)//2
        tap=adb.shell('input','tap',x,y)
        hierarchy('02-compose-recomposed',1);shots.append(screenshot('02-compose-recomposed'))
        logs=adb.shell('logcat','-d','-v','brief','--pid',str(run['pid']),'-s','WexRun')['stdout']
        assert f'WEX_COMPOSE_FRAME count=1 runId={run["run_id"]}' in logs,logs
        assert adb.shell('pidof',package)['stdout'].strip()==str(run['pid'])
        (out/'recomposition-logcat.txt').write_text(logs)
        assert shots[0]['sha256']!=shots[1]['sha256']
        for p,h in sources.items():assert hashlib.sha256(Path(p).read_bytes()).hexdigest()==h,p
        assert not marker.exists(),'Gradle/Maven invoked'
        stopped=adb.shell('am','force-stop','--user','current',package)
        deadline=time.monotonic()+5
        while adb.shell('pidof',package,check=False)['stdout'].strip():
            assert time.monotonic()<deadline,'app did not stop';time.sleep(.1)
        adb.shell('rm',remote)
        report={'result':'COMPOSE_DEVICE_OK','serial':serial,'run':receipt,'tap':{'x':x,'y':y,'node':node.attrib,'shell':tap},'screenshots':shots,'recomposition_logcat':logs,'disk_source_unchanged':True,'stop':{'shell':stopped,'pid_gone':True},'no_gradle_or_maven':True}
        (out/'compose-device.json').write_text(json.dumps(report,indent=2)+'\n')
        print('COMPOSE_DEVICE_OK: '+serial+'; real Material3 Expressive/Compose frame0 -> tap -> frame1; Kotlin+Java shared module; native APK/install/PID Logcat; 2 device screenshots; Stop PID gone; no Gradle/Maven')
    finally:
        adb.shell('am','force-stop','--user','current',package,check=False)
        adb.shell('rm','-f',remote,check=False)
