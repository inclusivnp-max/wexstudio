import org.objectweb.asm.*;
import com.google.gson.*;
import java.nio.file.*;
import java.util.*;
public class PreviewDiscoveryBytecodeTest {
 static Path classes;static final String PREVIEW="Landroidx/compose/ui/tooling/preview/Preview;";
 static void save(String name,ClassWriter writer)throws Exception{Path file=classes.resolve(name+".class");Files.createDirectories(file.getParent());Files.write(file,writer.toByteArray());}
 static ClassWriter type(String name,boolean annotation){var w=new ClassWriter(0);w.visit(Opcodes.V17,Opcodes.ACC_PUBLIC|(annotation?Opcodes.ACC_ANNOTATION|Opcodes.ACC_INTERFACE|Opcodes.ACC_ABSTRACT:0),name,null,"java/lang/Object",annotation?new String[]{"java/lang/annotation/Annotation"}:null);w.visitSource("Fixture.kt",null);return w;}
 static void preview(ClassWriter w,String name){var a=w.visitAnnotation(PREVIEW,false);a.visit("name",name);a.visitEnd();}
 public static void main(String[] args)throws Exception {
  Path root=Path.of(args[0]);classes=root.resolve("classes");Files.createDirectories(classes);
  var a=type("test/A",true);a.visitAnnotation("Ltest/B;",false).visitEnd();preview(a,"CycleLeaf");a.visitEnd();save("test/A",a);
  var b=type("test/B",true);b.visitAnnotation("Ltest/A;",false).visitEnd();b.visitEnd();save("test/B",b);
  var r=type("test/Repeat",true);preview(r,"CustomRepeat");var repeat=r.visitAnnotation("Ljava/lang/annotation/Repeatable;",true);repeat.visit("value",Type.getType("Ltest/Repeat$Container;"));repeat.visitEnd();r.visitEnd();save("test/Repeat",r);
  var container=type("test/Repeat$Container",true);container.visitMethod(Opcodes.ACC_PUBLIC|Opcodes.ACC_ABSTRACT,"value","()[Ltest/Repeat;",null,null).visitEnd();container.visitEnd();save("test/Repeat$Container",container);
  var owner=type("test/Owner",false);var m=owner.visitMethod(Opcodes.ACC_PUBLIC|Opcodes.ACC_STATIC,"Scene","(Landroidx/compose/runtime/Composer;I)V",null,null);m.visitAnnotation("Ltest/A;",false).visitEnd();var c=m.visitAnnotation("Ltest/Repeat$Container;",false);var array=c.visitArray("value");array.visitAnnotation(null,"Ltest/Repeat;").visitEnd();array.visitAnnotation(null,"Ltest/Repeat;").visitEnd();array.visitEnd();c.visitEnd();m.visitCode();m.visitInsn(Opcodes.RETURN);m.visitMaxs(0,2);m.visitEnd();owner.visitEnd();save("test/Owner",owner);
  Path input=root.resolve("input.json"),output=root.resolve("output.json");var j=new JsonObject();j.add("project_classes",new Gson().toJsonTree(List.of(classes.toString())));j.add("classpath",new Gson().toJsonTree(List.of(classes.toString(),args[1])));j.addProperty("output",output.toString());Files.writeString(input,j.toString());
  WexPreviewDiscovery.main(new String[]{input.toString()});var result=JsonParser.parseString(Files.readString(output)).getAsJsonObject();
  if(result.getAsJsonArray("entries").size()!=3||result.getAsJsonArray("issues").size()!=1)throw new AssertionError(result);
  if(!result.getAsJsonArray("issues").get(0).getAsJsonObject().get("message").getAsString().contains("Cyclic"))throw new AssertionError(result);
  var identities=new HashSet<String>();for(var e:result.getAsJsonArray("entries"))if(!identities.add(e.getAsJsonObject().get("identity").getAsString()))throw new AssertionError("duplicate identity");
  if(result.get("project_classes_executed").getAsBoolean())throw new AssertionError("executed classes");
  System.out.println("PREVIEW_BYTECODE_TEST_OK: cyclic meta annotation bounded; valid leaf retained; linked custom repeat container expanded twice; unique identities; no project class loading");
 }
}
