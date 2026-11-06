#!/usr/bin/env python3
"""Actual headless Layoutlib/Compose, native compile-only, prior-image failure guards."""
from pathlib import Path
import hashlib, json, os, shutil, struct, subprocess, sys, tempfile
ROOT=Path(__file__).resolve().parents[1]
PROJECT=Path(sys.argv[1]).resolve();OUT=Path(sys.argv[2]).resolve();OUT.mkdir(parents=True,exist_ok=True)
BIN=Path(os.environ.get('WEX_BIN',ROOT/'target/debug/wex'))
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def run(label,root,output,ok=True,env=None):
    result=subprocess.run([str(BIN),'--adb','127.0.0.1:1','preview',str(root),'--output',str(output)],capture_output=True,text=True,timeout=160,env=env)
    (OUT/(label+'.log')).write_text(result.stderr);(OUT/(label+'.exit')).write_text(str(result.returncode)+'\n')
    assert (result.returncode==0)==ok,(label,result.returncode,result.stderr[-10000:])
    if not ok:return result
    report=json.loads(result.stdout);(OUT/(label+'.json')).write_text(result.stdout)
    assert report['result']=='PREVIEW_OK' and report['native_backend']=='wex-native-compile-v1' and report['disk_source_unchanged']
    render=report['renderer'];assert render['result']=='HEADLESS_COMPOSE_LAYOUTLIB_OK' and render['headless'] and render['compose_groups']>0 and render['resource_ids']>0 and not render['emulator_used']
    data=output.read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',data[16:24])==(render['width'],render['height'])
    assert render['width']==report['entry']['width_dp']*2 and render['height']==report['entry']['height_dp']*2
    assert report['compose']['compiler_version']=='2.4.0' and report['compose']['modules']==['shared','app']
    steps=report['compilation_steps'];assert all(s['exit_code']==0 for s in steps)
    assert any('Kotlin K2' in s['name'] for s in steps) and any('AAPT2 link' in s['name'] for s in steps)
    # A resource owner can be named androidx.profileinstaller; it is not an
    # installation phase. Check the phase prefix AND actual tool executable.
    assert not any(any(k in s['name'].split(' · ',1)[0].lower() for k in ['d8','dex','sign','align','install']) for s in steps)
    assert not any(Path(s['program']).name in ['d8','apksigner','zipalign','adb'] for s in steps)
    assert not any((root/'.wex/preview-jobs').iterdir()),'snapshot leaked'
    return report
before={p:digest(p) for p in PROJECT.rglob('*') if p.is_file() and '.wex' not in p.parts}
pointer=PROJECT/'.wex/native/debug/current.json';published=digest(pointer) if pointer.exists() else None
normal=run('portrait',PROJECT,OUT/'portrait.png')
portrait_digest=digest(OUT/'portrait.png');portrait_receipt=digest(OUT/'portrait.json')
assert normal['renderer']['qualifiers'].find('-w360dp-h640dp-')>=0
with tempfile.TemporaryDirectory(prefix='wex-preview-test-',dir=OUT) as tmp:
    copy=Path(tmp)/'project';shutil.copytree(PROJECT,copy,ignore=shutil.ignore_patterns('.wex'))
    conf=copy/'wex.preview.json';config=json.loads(conf.read_text());e=config['entries'][0]
    e.update(width_dp=640,height_dp=360,dark=True,font_scale=1.25);conf.write_text(json.dumps(config))
    landscape=run('landscape-dark',copy,OUT/'landscape-dark.png')
    assert '-land-night-' in landscape['renderer']['qualifiers'] and landscape['renderer']['font_scale']==1.25
    previous=digest(OUT/'landscape-dark.png');receipt=digest(OUT/'landscape-dark.json')
    e['composable']='dev.wex.compose.MainActivityKt.DoesNotExist';conf.write_text(json.dumps(config))
    failed=run('missing-function',copy,OUT/'landscape-dark.png',False)
    assert 'Layoutlib' in failed.stderr and ('NoSuchMethod' in failed.stderr or 'errors' in failed.stderr or 'no view info' in failed.stderr)
    assert digest(OUT/'landscape-dark.png')==previous and digest(OUT/'landscape-dark.json')==receipt
    assert not any((copy/'.wex/preview-jobs').iterdir())
    # Tamper only with a private copy of generated renderer classes/inventory.
    tools=json.loads((ROOT/'.wex/preview-renderer/tools.json').read_text());original_classes=Path(tools['classes']);classes=Path(tmp)/'classes';shutil.copytree(original_classes,classes)
    tools['classes']=str(classes)
    for f in tools['files']:
        p=Path(f['path'])
        if p.is_relative_to(original_classes):f['path']=str(classes/p.relative_to(original_classes))
    target=classes/'WexPreviewRenderer.class';data=bytearray(target.read_bytes());data[-1]^=1;target.write_bytes(data)
    manifest=Path(tmp)/'tools.json';manifest.write_text(json.dumps(tools))
    env=os.environ.copy();env['WEX_PREVIEW_TOOLS']=str(manifest)
    failed=run('private-renderer-tamper',PROJECT,OUT/'portrait.png',False,env)
    assert 'preview runtime hash mismatch' in failed.stderr
    assert digest(OUT/'portrait.png')==portrait_digest and digest(OUT/'portrait.json')==portrait_receipt
for p,h in before.items():assert digest(p)==h,p
assert (digest(pointer) if pointer.exists() else None)==published,'preview changed published APK pointer'
(OUT/'preview-cli.json').write_text(json.dumps(dict(result='HEADLESS_PREVIEW_E2E_OK',portrait=normal,landscape_dark=landscape,missing_function_preserves_image=True,private_renderer_tamper_rejected=True,disk_source_unchanged=True,published_apk_unchanged=True,adb_address='127.0.0.1:1',emulator_used=False),indent=2)+'\n')
print('HEADLESS_PREVIEW_E2E_OK: real Compose/Layoutlib; native Kotlin/Java/resources only; portrait + landscape/night/fontScale; missing function/tamper preserve prior image; no DEX/sign/device/Gradle; disk/APK pointer unchanged')
