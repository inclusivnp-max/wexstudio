import com.google.gson.*;
import org.objectweb.asm.*;
import org.objectweb.asm.tree.*;
import java.nio.file.*;
import java.io.*;
import java.util.*;
import java.util.zip.*;

/** Reads compiler-emitted BINARY annotations, never loads/initializes project classes. */
public final class WexPreviewDiscovery {
 static final String PREVIEW="Landroidx/compose/ui/tooling/preview/Preview;";
 static final String CONTAINER="Landroidx/compose/ui/tooling/preview/Preview$Container;";
 static final int MAX_CLASS=8*1024*1024;
 final List<Path> cp=new ArrayList<>();
 final Map<String,ClassNode> cache=new HashMap<>();
 final Set<String> missing=new HashSet<>();
 final JsonArray entries=new JsonArray(), issues=new JsonArray();
 int reads=0;long totalBytes=0;
 byte[] read(InputStream stream)throws Exception {
  byte[] bytes=stream.readNBytes(MAX_CLASS+1);totalBytes+=bytes.length;
  if(bytes.length>MAX_CLASS||totalBytes>128L*1024*1024||++reads>20000)throw new IOException("Preview class inventory exceeds limits");return bytes;
 }
 ClassNode parse(byte[] bytes) {ClassNode node=new ClassNode(Opcodes.ASM9);new ClassReader(bytes).accept(node,ClassReader.SKIP_FRAMES);return node;}
 ClassNode lookup(String descriptor)throws Exception {
  if(cache.containsKey(descriptor))return cache.get(descriptor);if(missing.contains(descriptor))return null;
  if(!descriptor.startsWith("L")||!descriptor.endsWith(";")||descriptor.contains("..")||descriptor.contains("\\"))throw new IOException("Invalid annotation descriptor");
  if(cache.size()+missing.size()>=4096)throw new IOException("Preview annotation lookup limit");
  String name=descriptor.substring(1,descriptor.length()-1)+".class";
  for(Path item:cp) {
   if(Files.isDirectory(item)) {Path file=item.resolve(name);if(Files.isRegularFile(file)){try(var in=Files.newInputStream(file)){ClassNode node=parse(read(in));cache.put(descriptor,node);return node;}}}
   else if(item.toString().endsWith(".jar")){try(ZipFile zip=new ZipFile(item.toFile())){ZipEntry entry=zip.getEntry(name);if(entry!=null){try(var in=zip.getInputStream(entry)){ClassNode node=parse(read(in));cache.put(descriptor,node);return node;}}}}
  }
  missing.add(descriptor);return null;
 }
 static List<AnnotationNode> annotations(List<AnnotationNode> visible,List<AnnotationNode> invisible) {
  var list=new ArrayList<AnnotationNode>();if(visible!=null)list.addAll(visible);if(invisible!=null)list.addAll(invisible);return list;
 }
 static Map<String,Object> values(AnnotationNode node) {
  var map=new TreeMap<String,Object>();if(node.values!=null)for(int i=0;i<node.values.size();i+=2)map.put((String)node.values.get(i),node.values.get(i+1));return map;
 }
 void issue(String member,String message) {
  if(issues.size()>=1024)throw new IllegalStateException("Preview issue limit");
  var json=new JsonObject();json.addProperty("member",member);json.addProperty("message",message);issues.add(json);
 }
 boolean ignored(String descriptor) {
  return descriptor.startsWith("Ljava/")||descriptor.startsWith("Ljavax/")||descriptor.startsWith("Lkotlin/")||descriptor.startsWith("Lorg/jetbrains/")||descriptor.startsWith("Landroid/annotation/")||descriptor.startsWith("Landroidx/annotation/")||descriptor.startsWith("Landroidx/compose/runtime/");
 }
 void expand(AnnotationNode annotation,ClassNode owner,MethodNode method,LinkedHashSet<String> path,Map<String,Integer> occurrences)throws Exception {
  String member=owner.name.replace('/','.')+"."+method.name;
  if(path.size()>=16){issue(member,"Multipreview depth exceeds16");return;}
  if(!path.add(annotation.desc)){issue(member,"Cyclic multipreview: "+annotation.desc);return;}
  try {
   if(annotation.desc.equals(CONTAINER)) {
    Object nested=values(annotation).get("value");if(!(nested instanceof List<?> list))throw new IOException("Malformed Preview container");
    for(Object a:list){if(!(a instanceof AnnotationNode node)||!node.desc.equals(PREVIEW))throw new IOException("Invalid Preview container member");expand(node,owner,method,path,occurrences);}return;
   }
   if(annotation.desc.equals(PREVIEW)) {
    if(entries.size()>=512)throw new IOException("Preview entry limit512");
    ClassNode preview=lookup(PREVIEW);if(preview==null)throw new IOException("Actual Compose Preview annotation class missing");
    var parameters=new TreeMap<String,Object>();for(MethodNode m:preview.methods)if(m.annotationDefault!=null)parameters.put(m.name,m.annotationDefault);parameters.putAll(values(annotation));
    var item=new JsonObject();String branch=String.join("/",path);int occurrence=occurrences.merge(branch,1,Integer::sum);
    item.addProperty("identity",member+method.desc+"@"+branch+"#"+occurrence);item.addProperty("member",member);item.addProperty("descriptor",method.desc);item.addProperty("source_file",owner.sourceFile==null?"":owner.sourceFile);
    int line=0;for(AbstractInsnNode ins:method.instructions)if(ins instanceof LineNumberNode n){line=n.line;break;}item.addProperty("line",line);
    item.add("annotation_path",new Gson().toJsonTree(path));item.add("parameters",new Gson().toJsonTree(parameters));
    String problem="";
    if((method.access&Opcodes.ACC_STATIC)==0)problem="Instance previews require an instance provider";
    else if(!method.desc.equals("(Landroidx/compose/runtime/Composer;I)V"))problem="Preview requires a zero-argument lowered Composable; parameter providers/default arguments need support";
    item.addProperty("problem",problem);entries.add(item);return;
   }
   if(ignored(annotation.desc))return;
   Object repeated=values(annotation).get("value");
   if(repeated instanceof List<?> list&&!list.isEmpty()&&list.get(0) instanceof AnnotationNode child) {
    ClassNode contained=lookup(child.desc);boolean linked=false;
    if(contained!=null)for(AnnotationNode meta:annotations(contained.visibleAnnotations,contained.invisibleAnnotations)) {
     Object container=values(meta).get("value");
     if(meta.desc.equals("Ljava/lang/annotation/Repeatable;")&&container instanceof Type type&&type.getDescriptor().equals(annotation.desc))linked=true;
    }
    if(linked){for(Object nested:list){if(!(nested instanceof AnnotationNode node)||!node.desc.equals(child.desc))throw new IOException("Invalid repeatable multipreview container");expand(node,owner,method,path,occurrences);}return;}
   }
   ClassNode custom=lookup(annotation.desc);if(custom==null||(custom.access&Opcodes.ACC_ANNOTATION)==0)return;
   for(AnnotationNode nested:annotations(custom.visibleAnnotations,custom.invisibleAnnotations))expand(nested,owner,method,path,occurrences);
  } finally {path.remove(annotation.desc);}
 }
 void scan(ClassNode owner)throws Exception {
  if((owner.access&Opcodes.ACC_ANNOTATION)!=0)return;
  for(MethodNode method:owner.methods) {
   var occurrences=new HashMap<String,Integer>();
   for(AnnotationNode annotation:annotations(method.visibleAnnotations,method.invisibleAnnotations))expand(annotation,owner,method,new LinkedHashSet<>(),occurrences);
  }
 }
 public static void main(String[] args)throws Exception {
  var input=JsonParser.parseString(Files.readString(Path.of(args[0]))).getAsJsonObject();var scanner=new WexPreviewDiscovery();
  if(input.getAsJsonArray("classpath").size()>4096||input.getAsJsonArray("project_classes").size()>512)throw new IOException("Preview classpath limit");
  for(JsonElement p:input.getAsJsonArray("classpath"))scanner.cp.add(Path.of(p.getAsString()));
  var project=new TreeMap<String,ClassNode>();var definitions=new HashMap<String,byte[]>();
  for(JsonElement p:input.getAsJsonArray("project_classes")) {
   Path item=Path.of(p.getAsString());
   if(Files.isDirectory(item)) {try(var walk=Files.walk(item)){for(Path f:walk.filter(f->Files.isRegularFile(f)&&f.toString().endsWith(".class")).limit(20001).sorted().toList()){try(var in=Files.newInputStream(f)){byte[] bytes=scanner.read(in);ClassNode c=scanner.parse(bytes);byte[] previous=definitions.putIfAbsent(c.name,bytes);if(previous!=null&&!Arrays.equals(previous,bytes))throw new IOException("Conflicting project class definition: "+c.name);project.put(c.name,c);}}}}
   else {try(ZipFile zip=new ZipFile(item.toFile())){for(ZipEntry entry:Collections.list(zip.entries()))if(entry.getName().endsWith(".class")){try(var in=zip.getInputStream(entry)){byte[] bytes=scanner.read(in);ClassNode c=scanner.parse(bytes);byte[] previous=definitions.putIfAbsent(c.name,bytes);if(previous!=null&&!Arrays.equals(previous,bytes))throw new IOException("Conflicting project class definition: "+c.name);project.put(c.name,c);}}}}
  }
  for(ClassNode node:project.values())scanner.cache.put("L"+node.name+";",node);
  for(ClassNode node:project.values())scanner.scan(node);
  var result=new JsonObject();result.addProperty("result","COMPILED_PREVIEW_DISCOVERY_OK");result.addProperty("classes_read",scanner.reads);result.addProperty("project_classes",project.size());result.addProperty("project_classes_executed",false);result.add("entries",scanner.entries);result.add("issues",scanner.issues);
  Files.writeString(Path.of(input.get("output").getAsString()),new Gson().toJson(result));System.out.println("COMPILED_PREVIEW_DISCOVERY_OK "+scanner.entries.size());
 }
}
