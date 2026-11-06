#!/usr/bin/env python3
"""Exercise actual javac debug tables and the standalone source provider, no phone."""
from pathlib import Path
import tempfile,subprocess,os
R=Path(__file__).resolve().parents[1]
J=Path(os.environ.get('WEX_DEBUG_JAVA_HOME','/usr/lib/jvm/java-21-openjdk'))/'bin'
subprocess.run(['python3',str(R/'scripts/prepare-debugger.py'),'--offline'],check=True)
with tempfile.TemporaryDirectory() as d:
 d=Path(d);source=d/'Outer.java';source.write_text('''package probe;
public class Outer {
  static int outer(int x) {
    int result=x+1;
    return result;
  }
  static class Nested {
    static int inner(int x) {
      int doubled=x*2;
      return doubled;
    }
  }
}
class Other {
  static int other(int x) {
    return x-1;
  }
}
''')
 classes=d/'build/classes/app';classes.mkdir(parents=True)
 subprocess.run([str(J/'javac'),'-g','-d',str(classes),str(source)],check=True)
 helper=d/'SourceMapTest.java';helper.write_text('''import java.nio.file.*;import java.util.*;
public class SourceMapTest {
 public static void main(String[] a) throws Exception {
  Class<?> c=Class.forName("WexDebugServer");
  var f=c.getDeclaredField("sources");f.setAccessible(true);f.set(null,List.of(Paths.get(a[0])));
  var index=c.getDeclaredMethod("index",Path.class);index.setAccessible(true);index.invoke(null,Paths.get(a[1]));
  var at=c.getDeclaredMethod("classAt",String.class,int.class);at.setAccessible(true);
  String uri=Paths.get(a[0]).toUri().toString();
  assert at.invoke(null,uri,4).equals("probe.Outer");
  assert at.invoke(null,uri,9).equals("probe.Outer$Nested");
  assert at.invoke(null,uri,16).equals("probe.Other");
  var source=c.getDeclaredMethod("source",String.class,String.class);source.setAccessible(true);
  assert source.invoke(null,"probe.Other","Other.java").equals(uri);
  assert source.invoke(null,"android.app.Outer","Outer.java")==null;
  System.out.println("DEBUG_SOURCE_MAP_OK: compiled LineNumberTable + SourceFile; nested/multiple classes; no wrong-package fallback");
 }
}''')
 cp=os.pathsep.join([str(R/'.wex/debugger/classes'),str(R/'.wex/debugger/lib/*')])
 subprocess.run([str(J/'javac'),'-cp',cp,'-d',str(d),str(helper)],check=True)
 subprocess.run([str(J/'java'),'-ea','--add-modules','jdk.jdi','-cp',str(d)+os.pathsep+cp,'SourceMapTest',str(source),str(d/'build')],check=True)
