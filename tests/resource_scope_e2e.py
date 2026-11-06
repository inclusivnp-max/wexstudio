#!/usr/bin/env python3
"""Actual AAPT owner inventories + non-transitive R + binary AAR styleable ABI."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,uuid,zipfile
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True)
B=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();P=O/'project';MODE=sys.argv[2] if len(sys.argv)>2 else 'negative'
J=Path('/usr/lib/jvm/java-21-openjdk/bin')

def run(label,*args,expected=0):
 x=subprocess.run([str(B),*map(str,args)],capture_output=True,text=True,timeout=240)
 (O/(label+'.log')).write_text(x.stderr);(O/(label+'.json')).write_text(x.stdout);(O/(label+'.exit')).write_text(str(x.returncode)+'\n')
 assert x.returncode==expected,(label,x.returncode,x.stderr[-7000:]);return x

def write(p,s):p.parent.mkdir(parents=True,exist_ok=True);p.write_text(s)

def shared_probe(positive):
 return 'package dev.wex.shared\nfun resourcePositive(): Int = R.string.shared_proof\nfun resourceForbidden(): Int = R.string.'+('shared_proof' if positive else 'app_scope_only')+'\n'
def app_probe(positive):
 return 'package dev.wex.android\nfun resourcePositive(): Int = R.string.app_name\nfun resourceForbidden(): Int = '+('dev.wex.shared.R' if positive else 'R')+'.string.shared_proof\n'
shared=P/'shared/src/main/kotlin/dev/wex/shared/ResourceProbe.kt';app=P/'app/src/main/kotlin/dev/wex/android/ResourceProbe.kt'
if MODE=='failures':
 pointer=P/'.wex/native/debug/current.json';original=pointer.read_bytes();before=hashlib.sha256(original).hexdigest()
 java=shared.with_name('JavaResourceProbe.java')
 assert not java.exists()
 try:
  java.write_text('package dev.wex.shared; public class JavaResourceProbe { public static int leak() { return R.string.app_scope_only; } }')
  x=run('java-negative-build','build',P,'--offline',expected=1)
  assert 'app_scope_only' in x.stderr and 'Javac · shared failed' in x.stderr
  assert pointer.read_bytes()==original
 finally:java.unlink(missing_ok=True)
 bad=P/'shared/src/main/res/values/broken.xml';assert not bad.exists()
 try:
  bad.write_text('<resources><string name="broken">')
  x=run('bad-resource-build','build',P,'--offline',expected=1)
  assert 'AAPT2 compile failed' in x.stderr
  assert pointer.read_bytes()==original
 finally:bad.unlink(missing_ok=True)
 cfg=P/'wex.json';cfg_bytes=cfg.read_bytes()
 try:
  c=json.loads(cfg_bytes);next(m for m in c['modules'] if m['name']=='shared')['namespace']=c['build']['namespace'];cfg.write_text(json.dumps(c))
  run('namespace-collision-resolve','resolve',P,'--offline')
  x=run('namespace-collision-build','build',P,'--offline',expected=1)
  assert 'project modules share an R namespace' in x.stderr
  assert pointer.read_bytes()==original
 finally:
  cfg.write_bytes(cfg_bytes);run('restored-config-resolve','resolve',P,'--offline')
 assert hashlib.sha256(pointer.read_bytes()).hexdigest()==before
 (O/'failure-preservation.json').write_text(json.dumps({'result':'RESOURCE_FAILURE_PRESERVATION_OK','last_good_pointer_sha256':before,'java_parent_alias_rejected':True,'malformed_resource_rejected':True,'namespace_collision_rejected':True,'source_config_restored':True},indent=2)+'\n')
 print('RESOURCE_FAILURE_PRESERVATION_OK: real Javac rejects parent R alias; malformed resource and namespace collision fail; signed APK pointer unchanged; source/config restored')
 sys.exit()
if MODE=='positive':
 shared.write_text(shared_probe(True))
 # With shared corrected, app must independently reject its transitive R alias.
 x=run('app-negative-build','build',P,'--offline',expected=1);assert 'shared_proof' in x.stderr
 app.write_text(app_probe(True));x=run('positive-build','build',P,'--offline');report=json.loads(x.stdout)
 # The JDK executes real compiled AAR bytecode with its inlined styleable indices.
 runner=O/'BinaryStyleableCheck.java';runner.write_text('public class BinaryStyleableCheck { public static void main(String[] args) { int[] result=scope.library.StyleProof.ids(); if(result.length!=2 || result[0]==result[1] || result[0]>>>24!=0x7f || result[1]!=0x01010098) throw new AssertionError(java.util.Arrays.toString(result)); System.out.println("AAR_BINARY_STYLEABLE_OK:"+Integer.toHexString(result[0])+":"+Integer.toHexString(result[1])); } }')
 work=Path(report['build_dir']);gen=work/'generated-classes';lib=next(work.glob('aar/*/classes.jar'))
 # Locate by ZIP member, not extraction order.
 lib=next(p for p in work.glob('aar/*/classes.jar') if 'scope/library/StyleProof.class' in zipfile.ZipFile(p).namelist())
 cp=os.pathsep.join(map(str,[gen,lib]));subprocess.run([str(J/'javac'),'-cp',cp,str(runner)],check=True)
 check=subprocess.run([str(J/'java'),'-cp',cp+os.pathsep+str(O),'BinaryStyleableCheck'],capture_output=True,text=True,check=True)
 (O/'binary-styleable.log').write_text(check.stdout)
 with zipfile.ZipFile(report['apk']['path']) as z:
  dex=b''.join(z.read(n) for n in z.namelist() if n.endswith('.dex'))
  assert b'Lscope/library/StyleProof;' in dex
 sdk=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'))
 dump=subprocess.check_output([str(sdk/'build-tools/36.0.0/aapt2'),'dump','resources',report['apk']['path']],text=True);(O/'packaged-resources.txt').write_text(dump)
 for name in ['app_scope_only','shared_proof','library_scope_only','library_tone','local_view']:assert name in dump,name
 (O/'resource-scope.json').write_text(json.dumps({'result':'RESOURCE_SCOPE_E2E_OK','binary_styleable':check.stdout.strip(),'apk':report['apk'],'build_dir':report['build_dir'],'ownership':'nontransitive','gradle_invoked':False},indent=2)+'\n')
 print('RESOURCE_SCOPE_E2E_OK: actual compiler rejects parent/dependency R aliases; qualified resource access builds signed APK; owner-only AAPT fields; final IDs/styleable shapes; precompiled AAR index ABI executed')
 sys.exit()
assert MODE=='negative'
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'));(P/'wex.lock.json').unlink(missing_ok=True)
repo=O/'repo/scope/fixture/resource-library/1';repo.mkdir(parents=True)
# Real precompiled AAR: source R indexes are deliberately NOT final AAPT ID order.
work=O/'library';classes=work/'classes';classes.mkdir(parents=True)
write(work/'R.java','package scope.library; public final class R { public static final class styleable { public static final int[] Widget={0,0x01010098}; public static final int Widget_library_tone=0; public static final int Widget_android_textColor=1; } }')
write(work/'StyleProof.java','package scope.library; public final class StyleProof { public static int[] ids() { return new int[]{R.styleable.Widget[R.styleable.Widget_library_tone],R.styleable.Widget[R.styleable.Widget_android_textColor]}; } }')
subprocess.run([str(J/'javac'),'--release','17','-d',str(classes),str(work/'R.java'),str(work/'StyleProof.java')],check=True)
jar=work/'classes.jar'
with zipfile.ZipFile(jar,'w') as z:z.write(classes/'scope/library/StyleProof.class','scope/library/StyleProof.class')
with zipfile.ZipFile(repo/'resource-library-1.aar','w') as z:
 z.write(jar,'classes.jar');z.writestr('AndroidManifest.xml','<manifest package="scope.library"/>')
 z.writestr('res/values/values.xml','<resources><string name="library_scope_only">library-resource-ok</string><attr name="library_tone" format="color"/><declare-styleable name="Widget"><attr name="library_tone"/><attr name="android:textColor"/></declare-styleable></resources>')
 z.writestr('R.txt','int string library_scope_only 0x0\nint attr library_tone 0x0\nint[] styleable Widget {0, 0x01010098}\nint styleable Widget_library_tone 0\nint styleable Widget_android_textColor 1\n')
(repo/'resource-library-1.pom').write_text('<project><modelVersion>4.0.0</modelVersion><groupId>scope.fixture</groupId><artifactId>resource-library</artifactId><version>1</version><packaging>aar</packaging></project>')
c=json.loads((P/'wex.json').read_text());c['repositories'].insert(0,(O/'repo').as_uri());c['android']['application_id']='dev.wex.resource.e2e.case'+uuid.uuid4().hex[:10];next(m for m in c['modules'] if m['name']=='app')['dependencies'].append('scope.fixture:resource-library:1')
(P/'wex.json').write_text(json.dumps(c,indent=2)+'\n')
write(P/'app/src/main/res/values/ownership.xml','<resources><string name="app_scope_only">app-only</string><string name="app_alias">@string/shared_proof</string><string name="collision">app-overlay</string><attr name="app_tone" format="color"/><declare-styleable name="Widget"><attr name="app_tone"/></declare-styleable><style name="Widget.Base" parent=""><item name="android:textColor">#ffffff</item></style><string-array name="things"><item>A</item></string-array></resources>')
write(P/'shared/src/main/res/values/ownership.xml','<resources><string name="collision">shared-value</string><attr name="shared_tone" format="color"/><declare-styleable name="Widget"><attr name="shared_tone"/></declare-styleable></resources>')
write(P/'shared/src/main/res/layout/local.xml','<View xmlns:android="http://schemas.android.com/apk/res/android" android:id="@+id/local_view" android:layout_width="match_parent" android:layout_height="match_parent"/>')
write(shared,shared_probe(False));write(app,app_probe(False))
main=P/'app/src/main/kotlin/dev/wex/android/MainActivity.kt';s=main.read_text();s=s.replace('val debug = BuildConfig.DEBUG','''val styleIds = scope.library.StyleProof.ids()
        val styleName = resources.getResourceEntryName(styleIds[0])
        check(styleName == "library_tone" && styleIds[1] == android.R.attr.textColor)
        val qualified = getString(dev.wex.shared.R.string.shared_proof)
        val alias = getString(R.string.app_alias)
        check(alias == qualified)
        val library = getString(scope.library.R.string.library_scope_only)
        val collision = getString(dev.wex.shared.R.string.collision)
        check(collision == "app-overlay")
        check(dev.wex.shared.R.styleable.Widget.size == 1 && R.styleable.Widget.size == 1 && scope.library.R.styleable.Widget.size == 2)
        Log.i("WexResources", "WEX_RESOURCE_OK style=$styleName library=$library shared=$qualified collision=$collision alias=$alias runId=${intent.getStringExtra("wex.run_id")}")
        val debug = BuildConfig.DEBUG''');main.write_text(s)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','curl','adb']:
 f=traps/name;f.write_text('#!/bin/sh\necho '+name+' > '+str(marker)+'\nexit 99\n');f.chmod(0o755)
os.environ.update(PATH=str(traps)+os.pathsep+os.environ['PATH'],WEX_GRADLE_BRIDGE=str(traps/'bridge'))
run('resolve','resolve',P,'--offline');x=run('negative-analysis','analyze',P,'--offline');a=json.loads(x.stdout)
mods={m['name']:m for m in a['graph']['modules']}
def generated(module,package):
 root=Path(mods[module]['generated_source_roots'][0]);return (root/package.replace('.','/')/'R.java').read_text()
app_r=generated(':app','dev.wex.android');shared_r=generated(':shared','dev.wex.shared')
assert 'app_scope_only' in app_r and 'app_scope_only' not in shared_r and 'shared_proof' in shared_r and 'shared_proof' not in app_r
assert 'library_scope_only' not in app_r and 'library_scope_only' not in shared_r
assert 'app_alias' in app_r and 'app_alias' not in shared_r
assert 'shared_tone' not in app_r and 'app_tone' not in shared_r
assert 'local_view' in shared_r and 'local_view' not in app_r
assert 'Widget_Base' in app_r and 'things' in app_r and 'Widget_shared_tone = 0' in shared_r and 'Widget_app_tone = 0' in app_r
assert 'collision' in shared_r and 'collision' in app_r
header=next(Path(p) for p in mods[':app']['classpath'] if p.endswith('/symbols/app.jar'))
cp=O/'headers';cp.mkdir()
with zipfile.ZipFile(header) as z:
 z.extractall(cp)
 listing=subprocess.check_output([str(J/'javap'),'-constants',str(cp/'scope/library/R$styleable.class')],text=True)
 assert 'Widget_library_tone = 0' in listing and 'Widget_android_textColor = 1' in listing
 (O/'aar-header.txt').write_text(listing)
assert not (P/'.wex/native/debug/current.json').exists()
x=run('shared-negative-build','build',P,'--offline',expected=1);assert 'app_scope_only' in x.stderr
assert not (P/'.wex/native/debug/current.json').exists() and not marker.exists()
print('RESOURCE_SCOPE_NEGATIVE_OK: actual AAPT owned fields/layout IDs/arrays/styles; final linked IDs; colliding styleables remain owner-only; real Kotlin rejects parent R alias')
