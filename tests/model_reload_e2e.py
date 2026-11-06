#!/usr/bin/env python3
"""Private SDK/JDK fixture for actual Connection automatic model reload checks."""
from pathlib import Path
import json,os,shutil,sys
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=True);P=O/'project'
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
(P/'wex.lock.json').unlink(missing_ok=True)
probe=P/'app/src/main/kotlin/dev/wex/android/ReloadProbe.kt';probe.write_text('package dev.wex.android\nfun resourceProbe(): Int = R.string.app_name\nval variantDebug: Boolean = BuildConfig.DEBUG\n')
SDK=O/'sdk';(SDK/'build-tools/36.0.0').mkdir(parents=True);real_sdk=Path(os.environ.get('ANDROID_HOME',Path.home()/'Android/Sdk'))
(SDK/'platforms').symlink_to(real_sdk/'platforms',target_is_directory=True);shutil.copy2(real_sdk/'build-tools/36.0.0/aapt2',SDK/'build-tools/36.0.0/aapt2')
JDK=O/'jdk';(JDK/'bin').mkdir(parents=True);real_jdk=Path('/usr/lib/jvm/java-21-openjdk')
for name in ['lib','conf','jmods','include','legal']:
 if (real_jdk/name).exists():(JDK/name).symlink_to(real_jdk/name,target_is_directory=True)
shutil.copy2(real_jdk/'release',JDK/'release')
for name in ['java','javac','jar','jmod','jlink']:shutil.copy2(real_jdk/'bin'/name,JDK/'bin'/name)
traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for name in ['gradle','gradlew','mvn','bridge','adb','emulator']:
 p=traps/name;p.write_text('#!/bin/sh\necho '+name+' > '+str(marker)+'\nexit 99\n');p.chmod(0o755)
(O/'fixture.json').write_text(json.dumps({'project':str(P),'sdk':str(SDK),'jdk':str(JDK),'traps':str(traps),'marker':str(marker)},indent=2)+'\n')
print('MODEL_RELOAD_FIXTURE_READY: private analysis-only SDK/JDK; original tools unchanged; no APK tools')
