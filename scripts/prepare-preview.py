#!/usr/bin/env python3
"""Compile our headless adapter against a local Layoutlib; no provisioning/downloads."""
from pathlib import Path
import argparse, hashlib, json, os, shutil, subprocess, tempfile
ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--studio', default=os.environ.get('WEX_LAYOUTLIB_STUDIO', '/opt/android-studio'))
p.add_argument('--offline', action='store_true')
a = p.parse_args()
studio = Path(a.studio).resolve()
source = ROOT / 'preview/WexPreviewRenderer.java'
discovery = ROOT / 'preview/WexPreviewDiscovery.java'
output = Path(os.environ.get('WEX_TOOLS_HOME', ROOT/'.wex')).resolve() / 'preview-renderer'
output.mkdir(parents=True, exist_ok=True)
data = studio / 'plugins/design-tools/resources/layoutlib/data'
paths = sorted(set(x.resolve() for folder in ['lib', 'plugins/android/lib', 'plugins/design-tools/lib'] for x in (studio/folder).glob('*.jar')))
paths.append((data/'layoutlib-extensions.jar').resolve())
assert paths[-1].is_file() and (studio/'plugins/design-tools/lib/layoutlib.jar').is_file(), 'Local Layoutlib installation missing'
java = Path(os.environ.get('WEX_PREVIEW_JAVA_HOME', '/usr/lib/jvm/java-21-openjdk')).resolve()
def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()
def stamp(path):
    return dict(path=str(path), bytes=path.stat().st_size, sha256=sha(path))
runtime = sorted(set(paths) | {x.resolve() for folder in ['fonts','linux/lib64','icu','hyphen-data','keyboards'] for x in (data/folder).rglob('*') if x.is_file()})
files = [stamp(f) for f in runtime]
build = json.loads((studio/'product-info.json').read_text())['buildNumber']
source_hash = sha(source)
discovery_hash = sha(discovery)
fingerprint = hashlib.sha256(json.dumps([files, source_hash, discovery_hash, str(java), build], sort_keys=True).encode()).hexdigest()
manifest = output/'tools.json'
try:
    old = json.loads(manifest.read_text())
except (FileNotFoundError, ValueError):
    old = {}
classes = output/'classes'
valid_classes = (classes/'WexPreviewRenderer.class').is_file() and bool(old.get('class_files'))
if valid_classes:
    valid_classes = all(Path(f['path']).is_file() and stamp(Path(f['path'])) == f for f in old['class_files'])
if old.get('compile_fingerprint') != fingerprint or not valid_classes:
    with tempfile.TemporaryDirectory(dir=output, prefix='compile-') as tmp:
        result = subprocess.run([str(java/'bin/javac'), '-proc:none', '-cp', os.pathsep.join(map(str, paths)), '-d', tmp, str(source), str(discovery)], text=True, capture_output=True)
        if result.returncode:
            import sys
            print(result.stderr, file=sys.stderr)
            raise SystemExit(result.returncode)
        if classes.exists():
            shutil.rmtree(classes)
        shutil.copytree(tmp, classes)
class_files = [stamp(f.resolve()) for f in sorted(classes.rglob('*.class'))]
report = dict(schema=1, studio=str(studio), studio_build=build, java=str(java/'bin/java'), classes=str(classes), source_sha256=source_hash, discovery_source_sha256=discovery_hash, compile_fingerprint=fingerprint, classpath=list(map(str, paths)), class_files=class_files, files=files+class_files)
with tempfile.NamedTemporaryFile(mode='w', dir=output, delete=False) as f:
    json.dump(report, f, indent=2)
    f.write('\n')
    temp = f.name
os.replace(temp, manifest)
print(f'PREVIEW_TOOLS_OK: local Layoutlib {build}; {len(files)+len(class_files)} verified runtime/class files; own JVM adapter; no downloads/Gradle')
