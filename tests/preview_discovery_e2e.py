#!/usr/bin/env python3
"""Compiler-bytecode Preview/repeat/multipreview/aliases + real headless Layoutlib."""
from pathlib import Path
import hashlib,json,os,shutil,struct,subprocess,sys
R=Path(__file__).resolve().parents[1];OUT=Path(sys.argv[1]).resolve();OUT.mkdir(parents=True,exist_ok=True)
BIN=Path(os.environ.get('WEX_BIN',R/'target/debug/wex'));PROJECT=OUT/'project'
if not PROJECT.exists():
 shutil.copytree(R/'tests/fixtures/compose-android',PROJECT,ignore=shutil.ignore_patterns('.wex'))
 (PROJECT/'wex.preview.json').unlink()
 config=json.loads((PROJECT/'wex.json').read_text());config['modules'][0]['dependencies'].append('androidx.compose.ui:ui-tooling-preview-android:1.9.4');(PROJECT/'wex.json').write_text(json.dumps(config,indent=2)+'\n')
 shutil.copy2(R/'tests/fixtures/preview-annotations/PreviewCases.kt',PROJECT/'app/src/main/kotlin/dev/wex/compose/PreviewCases.kt')
 localized=PROJECT/'app/src/main/res/values-fr/strings.xml';localized.parent.mkdir(parents=True,exist_ok=True);localized.write_text('<resources><string name="app_name">Aperçu Wex</string></resources>\n')
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
before={str(p.relative_to(PROJECT)):sha(p) for p in PROJECT.rglob('*') if p.is_file() and '.wex' not in p.parts}
def run(label,*args,ok=True):
 p=subprocess.run([str(BIN),'--adb','127.0.0.1:1',*map(str,args)],capture_output=True,text=True,timeout=180)
 (OUT/(label+'.log')).write_text(p.stderr);(OUT/(label+'.exit')).write_text(str(p.returncode)+'\n');(OUT/(label+'.json')).write_text(p.stdout)
 assert (p.returncode==0)==ok,(label,p.returncode,p.stderr[-7000:])
 return json.loads(p.stdout) if ok else p
run('resolve','resolve',PROJECT,'--offline')
# Lock may legitimately be mediated by own resolver before the source-preservation baseline.
before={str(p.relative_to(PROJECT)):sha(p) for p in PROJECT.rglob('*') if p.is_file() and '.wex' not in p.parts}
listed=run('catalog','preview-list',PROJECT)
assert listed['result']=='PREVIEW_CATALOG_OK' and listed['disk_source_unchanged'];c=listed['catalog'];items=c['items']
assert c['backend']=='compiler-bytecode-asm-v1' and len(items)==15,(len(items),items)
assert not (PROJECT/'wex.preview.json').exists()
assert all(i['origin']=='@Preview' and i['source_file']=='PreviewCases.kt' for i in items)
assert all(i['entry']['composable'].startswith('dev.wex.compose.PreviewShowcase.') for i in items)
assert sum(i['problem'] is not None for i in items)==3
assert all('NotAnAnnotation' not in i['entry']['name'] for i in items)
assert any(len(i['annotation_path'])>=3 and i['entry']['name']=='MetaNight' for i in items)
assert any('PreviewLightDark' in ' '.join(i['annotation_path']) for i in items)
assert not any(k in s['name'].split(' · ',1)[0].lower() for s in listed['compilation_steps'] for k in ['dex','d8','sign','align','install'])
assert not any(Path(s['program']).name in ['d8','apksigner','zipalign','adb'] for s in listed['compilation_steps'])
def item(name,member=None):return next(i for i in items if i['entry']['name']==name and (not member or i['entry']['composable'].endswith('.'+member)))
image=OUT/'selected.png'
def render(label,i):
 r=run(label,'preview',PROJECT,'--entry',i['id'],'--output',image)
 assert r['result']=='PREVIEW_OK' and r['selected_id']==i['id'] and r['renderer']['result']=='HEADLESS_COMPOSE_LAYOUTLIB_OK' and r['renderer']['headless'] and not r['renderer']['emulator_used']
 assert r['renderer']['compose_groups']>0 and r['disk_source_unchanged'] and r['catalog']['backend']==c['backend']
 data=image.read_bytes();assert struct.unpack('>II',data[16:24])==(r['entry']['width_dp']*2,r['entry']['height_dp']*2)
 shutil.copy2(image,OUT/(label+'.png'));return r
normal=render('direct-private',item('Direct','DirectScene'));assert normal['renderer']['show_background'] and normal['renderer']['background_color']==0xff00ff00
from PIL import Image
with Image.open(OUT/'direct-private.png') as im:assert im.convert('RGBA').getpixel((im.width-1,im.height-1))==(0,255,0,255)
night=render('meta-night',item('MetaNight','MultiScene'));assert night['renderer']['locale']=='fr-FR' and night['entry']['dark'] and night['renderer']['font_scale']==1.25
# Change only locale in an isolated fixture; actual localized resource must change pixels.
cases=PROJECT/'app/src/main/kotlin/dev/wex/compose/PreviewCases.kt';original=cases.read_bytes()
try:
 cases.write_bytes(original.replace(b'locale = "fr-rFR"',b'locale = "en-US"'))
 english=render('meta-night-english',item('MetaNight','MultiScene'))
 assert english['selected_id']==night['selected_id'] and english['entry']['locale']=='en-US'
 assert english['png_blake3']!=night['png_blake3'],'locale did not affect actual rendered resources'
finally:cases.write_bytes(original)
landscape=render('repeat-landscape',item('Landscape'));assert landscape['renderer']['width']==1280 and landscape['renderer']['height']==480
previous=(sha(image),sha(image.with_suffix('.json')))
for label,selector in [('ambiguous','Direct'),('unknown','missing'),('required-parameter',item('RequiredParameter')['id']),('unsupported-system-ui',item('SystemUi')['id'])]:
 result=run(label,'preview',PROJECT,'--entry',selector,'--output',image,ok=False)
 assert ('Ambiguous' in result.stderr if label=='ambiguous' else 'missing' in result.stderr if label=='unknown' else 'unavailable' in result.stderr),result.stderr[-2000:]
 assert (sha(image),sha(image.with_suffix('.json')))==previous
# Stable catalog IDs across different private compiler work directories.
again=run('catalog-again','preview-list',PROJECT);assert [i['id'] for i in again['catalog']['items']]==[i['id'] for i in items]
for rel,h in before.items():assert sha(PROJECT/rel)==h,rel
assert not (PROJECT/'.wex/native/debug/current.json').exists()
assert not any((PROJECT/'.wex/preview-jobs').iterdir())
(OUT/'discovery-e2e.json').write_text(json.dumps({'result':'PREVIEW_DISCOVERY_E2E_OK','project':str(PROJECT),'catalog':c,'direct_private':normal,'meta_night':night,'landscape':landscape,'unsupported_preserves_prior_image':True,'ids_stable':True,'disk_source_unchanged':True,'no_apk_publication':True,'adb_address':'127.0.0.1:1'},indent=2)+'\n')
print('PREVIEW_DISCOVERY_E2E_OK: 15 compiler-emitted previews; aliases/constants/@JvmName/private/repeat/nested+library multipreview; 3 visible unavailable; actual Layoutlib portrait/night+French/landscape/background; stable IDs; failure preserves image; no Gradle/ADB/DEX/sign/APK publication; sources unchanged')
