#!/usr/bin/env python3
"""Build a relocatable, manifest-verified native package; tools/SDK are external."""
from pathlib import Path
import argparse,ctypes,errno,hashlib,json,os,platform,shutil,subprocess,tarfile,tempfile,tomllib
R=Path(__file__).resolve().parents[1];p=argparse.ArgumentParser();p.add_argument('--binaries',type=Path,default=R/'target/release');p.add_argument('--output',type=Path,required=True);a=p.parse_args();out=a.output.absolute();assert platform.system()=='Linux' and platform.machine()=='x86_64';assert not out.exists(),'Output already exists';out.parent.mkdir(parents=True,exist_ok=True)
assets=['formatter/WexJavaFormat.java','formatter/dependency.json','scripts/prepare-java-formatter.py','scripts/install-kotlin-lsp.py','scripts/prepare-debugger.py','scripts/prepare-preview.py','scripts/prepare-bridge.py','scripts/gradle-installed.py','scripts/gradle-bridge.sh','debugger/WexDebugServer.java','debugger/dependencies.json','preview/WexPreviewRenderer.java','preview/WexPreviewDiscovery.java','bridge/generated.init.gradle','bridge/src/dev/wex/bridge/GradleBridge.java','bridge/src/dev/wex/bridge/GeneratedArtifacts.java']
def sha(path):
 with Path(path).open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
with tempfile.TemporaryDirectory(prefix='.wex-package-',dir=out.parent) as private:
 stage=Path(private)/out.name;stage.mkdir();(stage/'bin').mkdir();support=stage/'share/wex';support.mkdir(parents=True)
 for name in ['wex','wex-ide']:
  source=a.binaries/name;data=source.read_bytes();assert data[:4]==b'\x7fELF' and data[4]==2 and int.from_bytes(data[18:20],'little')==62,'Expected native Linux x86_64 ELF';sections=subprocess.check_output(['readelf','-SW',str(source)],text=True);assert '.debug_' not in sections and '.symtab' not in sections,'Release binaries must be stripped';shutil.copy2(source,stage/'bin'/name)
 frontend=(stage/'bin/wex-ide').stat().st_size;assert frontend<=20_000_000,('Native frontend exceeds strict uncompressed 20MB budget',frontend)
 inventory={}
 for name in assets:
  dest=support/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(R/name,dest);inventory[name]=sha(dest)
 (support/'support.json').write_text(json.dumps({'schema':1,'files':inventory,'external_tools_bundled':False},indent=2,sort_keys=True)+'\n')
 for source,dest in [('assets/icons/LICENSE-MaterialSymbols.txt','share/licenses/LICENSE-MaterialSymbols.txt'),('assets/icons/README.txt','share/licenses/MaterialSymbols-README.txt'),('assets/icons/manifest.json','share/licenses/MaterialSymbols-manifest.json'),('packaging/terminal-NOTICES.txt','share/licenses/terminal-NOTICES.txt'),('packaging/wexstudio','wexstudio'),('packaging/install-desktop.py','install-desktop.py'),('packaging/README.txt','README.txt'),('packaging/wexstudio.desktop.in','share/applications/wexstudio.desktop.in'),('packaging/wexstudio.svg','share/icons/wexstudio.svg')]:
  d=stage/dest;d.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(R/source,d)
 files={str(f.relative_to(stage)):{'sha256':sha(f),'bytes':f.stat().st_size} for f in sorted(stage.rglob('*')) if f.is_file()};version=tomllib.loads((R/'Cargo.toml').read_text())['package']['version'];ldd=subprocess.check_output(['ldd',str(stage/'bin/wex-ide')],text=True);assert 'libjvm' not in ldd and 'libjava' not in ldd
 (stage/'manifest.json').write_text(json.dumps({'schema':1,'version':version,'target':'linux-x86_64','native_frontend_bytes':frontend,'native_frontend_budget_bytes':20_000_000,'cli_bytes':files['bin/wex']['bytes'],'support_bytes':sum(row['bytes'] for name,row in files.items() if name.startswith('share/wex/')),'package_payload_bytes':sum(row['bytes'] for row in files.values()),'external_tools_bundled':False,'os_shared_libraries':ldd,'files':files},indent=2,sort_keys=True)+'\n')
 libc=ctypes.CDLL(None,use_errno=True);libc.renameat2.argtypes=[ctypes.c_int,ctypes.c_char_p,ctypes.c_int,ctypes.c_char_p,ctypes.c_uint];result=libc.renameat2(-100,os.fsencode(stage),-100,os.fsencode(out),1)
 if result:raise OSError(ctypes.get_errno(),os.strerror(ctypes.get_errno()))
archive=out.with_name(out.name+'.tar.gz')
with archive.open('xb') as raw:
 with tarfile.open(fileobj=raw,mode='w:gz') as tar:tar.add(out,arcname=out.name)
receipt={'result':'NATIVE_PACKAGE_OK','root':str(out),'archive':str(archive),'archive_bytes':archive.stat().st_size,'archive_sha256':sha(archive),'native_frontend_bytes':frontend,'native_frontend_budget_bytes':20_000_000,'combined_uncompressed_bytes':sum(f.stat().st_size for f in out.rglob('*') if f.is_file()),'external_tools_bundled':False};out.with_name(out.name+'-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps(receipt,indent=2))
