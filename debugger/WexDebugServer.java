// Standalone source/runtime providers for the unmodified Microsoft Java Debug Server.
// The Rust frontend remains JVM-free; this process runs only during debug sessions.
import com.microsoft.java.debug.core.*;
import com.microsoft.java.debug.core.adapter.*;
import com.microsoft.java.debug.core.protocol.Types.SourceBreakpoint;
import com.sun.jdi.Bootstrap;
import java.lang.reflect.*;
import java.net.URI;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.CompletableFuture;
import java.util.regex.*;

public final class WexDebugServer {
    private static List<Path> sources;
    private record Compiled(String name,String file,SortedSet<Integer> lines) { }
    private static final List<Compiled> compiled=new ArrayList<>();
    private static byte[] attribute(java.io.DataInputStream in) throws Exception {
        int n=in.readInt();if(n<0||n>16*1024*1024)throw new java.io.IOException("Class attribute length");
        byte[] b=in.readNBytes(n);if(b.length!=n)throw new java.io.EOFException();return b;
    }
    private static void member(java.io.DataInputStream in,Object[] cp,SortedSet<Integer> lines) throws Exception {
        in.skipNBytes(6);int count=in.readUnsignedShort();
        for(int a=0;a<count;a++) {
            String name=(String)cp[in.readUnsignedShort()];byte[] bytes=attribute(in);
            if(!name.equals("Code"))continue;
            var code=new java.io.DataInputStream(new java.io.ByteArrayInputStream(bytes));code.skipNBytes(4);
            int n=code.readInt();if(n<0||n>65535)throw new java.io.IOException("Invalid bytecode length");code.skipNBytes(n);
            code.skipNBytes(8L*code.readUnsignedShort());int nested=code.readUnsignedShort();
            for(int b=0;b<nested;b++) {String key=(String)cp[code.readUnsignedShort()];byte[] value=attribute(code);
                if(key.equals("LineNumberTable")){var table=new java.io.DataInputStream(new java.io.ByteArrayInputStream(value));int size=table.readUnsignedShort();for(int c=0;c<size;c++){table.readUnsignedShort();lines.add(table.readUnsignedShort());}}
            }
        }
    }
    private static Compiled parseClass(Path path) throws Exception {
        if(Files.size(path)>16*1024*1024)throw new java.io.IOException("Oversized class file");
        try(var in=new java.io.DataInputStream(Files.newInputStream(path))) {
            if(in.readInt()!=0xcafebabe)throw new java.io.IOException("Class magic");in.skipNBytes(4);
            Object[] cp=new Object[in.readUnsignedShort()];
            for(int i=1;i<cp.length;i++) {int tag=in.readUnsignedByte();switch(tag){
                case 1:cp[i]=in.readUTF();break;
                case 7:cp[i]=in.readUnsignedShort();break;
                case 8,16,19,20:in.skipNBytes(2);break;
                case 3,4,9,10,11,12,17,18:in.skipNBytes(4);break;
                case 5,6:in.skipNBytes(8);i++;break;
                case 15:in.skipNBytes(3);break;
                default:throw new java.io.IOException("Class constant tag "+tag);
            }}
            in.readUnsignedShort();String name=((String)cp[(Integer)cp[in.readUnsignedShort()]]).replace('/','.');in.readUnsignedShort();in.skipNBytes(2L*in.readUnsignedShort());
            SortedSet<Integer> lines=new TreeSet<>();int fields=in.readUnsignedShort();for(int i=0;i<fields;i++)member(in,cp,lines);
            int methods=in.readUnsignedShort();for(int i=0;i<methods;i++)member(in,cp,lines);
            String source=null;int attrs=in.readUnsignedShort();for(int i=0;i<attrs;i++){String key=(String)cp[in.readUnsignedShort()];byte[] value=attribute(in);if(key.equals("SourceFile"))source=(String)cp[new java.io.DataInputStream(new java.io.ByteArrayInputStream(value)).readUnsignedShort()];}
            return new Compiled(name,source,lines);
        }
    }
    private static String packageOf(String name){int dot=name.lastIndexOf('.');return dot<0?"":name.substring(0,dot);}
    private static void index(Path build) throws Exception {
        Path classes=build.resolve("classes");if(!Files.isDirectory(classes))return;
        try(var walk=Files.walk(classes)){for(Path p:walk.filter(x->x.toString().endsWith(".class")&&Files.isRegularFile(x)).limit(100000).toList())compiled.add(parseClass(p));}
    }

    private static String source(String className, String sourcePath) {
        String name=sourcePath.replace('\\','/');
        for(Compiled c:compiled)if(c.name.equals(className)&&c.file!=null){name=c.file;break;}
        String base=name.substring(name.lastIndexOf('/')+1);
        List<Path> matches=sources.stream().filter(p->p.getFileName().toString().equals(base)).toList();
        if(matches.isEmpty()) {
            String simple=className.substring(className.lastIndexOf('.')+1).split("\\$")[0];
            if(simple.endsWith("Kt"))simple=simple.substring(0,simple.length()-2);
            final String stem=simple;
            matches=sources.stream().filter(p->p.getFileName().toString().matches(Pattern.quote(stem)+"\\.(kt|java)")).toList();
        }
        String pack=className.contains(".")?className.substring(0,className.lastIndexOf('.')):"";
        for(Path p:matches) {
            try { if(packageName(Files.readString(p)).equals(pack))return p.toUri().toString(); } catch(Exception ignored) { }
        }
        return null;
    }
    private static String packageName(String text) {
        Matcher m=Pattern.compile("(?m)^\\s*package\\s+([\\w.]+)").matcher(text);
        return m.find()?m.group(1):"";
    }
    private static String classAt(String uri, int line) throws Exception {
        Path p=uri.startsWith("file:")?Paths.get(URI.create(uri)):Paths.get(uri);
        String text=Files.readString(p);String pack=packageName(text);
        Optional<Compiled> exact=compiled.stream().filter(c->p.getFileName().toString().equals(c.file)&&packageOf(c.name).equals(pack)&&c.lines.contains(line)).min(Comparator.comparingInt(c->c.name.length()));
        if(exact.isPresent())return exact.get().name;
        String prefix=String.join("\n",text.lines().limit(line).toList());
        Matcher m=Pattern.compile("(?m)^(?:\\s*(?:public|private|internal|protected|open|abstract|data|sealed|final|static)\\s+)*\\s*(?:class|object|interface|enum(?:\\s+class)?)\\s+(\\w+)").matcher(prefix);
        String clazz=null;
        if(m.find())clazz=m.group(1);
        if(clazz==null)clazz=p.getFileName().toString().replaceFirst("\\.(kt|java)$","")+(p.toString().endsWith(".kt")?"Kt":"");
        return (pack.isEmpty()?"":pack+".")+clazz;
    }
    private static Object provider(Class<?> type) {
        return Proxy.newProxyInstance(type.getClassLoader(),new Class<?>[]{type},(proxy,method,args)-> {
            String n=method.getName();
            if(n.equals("toString"))return "Wex "+type.getSimpleName();
            if(n.equals("hashCode"))return System.identityHashCode(proxy);
            if(n.equals("equals"))return proxy==args[0];
            if(n.equals("getVirtualMachineManager"))return Bootstrap.virtualMachineManager();
            if(n.equals("supportsRealtimeBreakpointVerification")||n.equals("isInEvaluation"))return false;
            if(n.equals("getBreakpointLocations")) {
                SourceBreakpoint[] b=(SourceBreakpoint[])args[1];JavaBreakpointLocation[] result=new JavaBreakpointLocation[b.length];
                for(int i=0;i<b.length;i++) {
                    result[i]=new JavaBreakpointLocation(b[i].line,b[i].column);
                    result[i].setClassName(classAt((String)args[0],b[i].line));
                }
                return result;
            }
            if(n.equals("getFullyQualifiedName")) {
                int[] lines=(int[])args[1];String[] result=new String[lines.length];
                for(int i=0;i<lines.length;i++)result[i]=classAt((String)args[0],lines[i]);return result;
            }
            if(n.equals("getSourceFileURI"))return source((String)args[0],(String)args[1]);
            if(n.equals("getSourceContents"))return Files.readString(Paths.get(URI.create((String)args[0])));
            if(n.equals("getEventHub"))return io.reactivex.Observable.never();
            if(n.equals("findMethodInvocations")||n.equals("codeComplete"))return List.of();
            if(n.startsWith("evaluate")||n.equals("invokeMethod")||n.equals("redefineClasses"))return CompletableFuture.failedFuture(new UnsupportedOperationException("Expression evaluation / hot reload is not enabled"));
            if(method.isDefault())return InvocationHandler.invokeDefault(proxy,method,args);
            return null;
        });
    }
    @SuppressWarnings({"unchecked","rawtypes"})
    public static void main(String[] args) throws Exception {
        // Explicit manifest avoids recursively scanning build/dependency caches.
        var array=com.google.gson.JsonParser.parseString(Files.readString(Paths.get(args[0]))).getAsJsonArray();
        sources=new ArrayList<>();for(var e:array)sources.add(Paths.get(e.getAsString()));
        index(Paths.get(args[1]));
        DebugSettings.getCurrent().updateSettings("{\"showToString\":false,\"showLogicalStructure\":false,\"maxStringLength\":2000,\"jdwpRequestTimeout\":10000,\"asyncJDWP\":\"off\"}");
        ProviderContext context=new ProviderContext();
        for(Class type:List.of(IVirtualMachineManagerProvider.class,ISourceLookUpProvider.class,IEvaluationProvider.class,IHotCodeReplaceProvider.class,ICompletionsProvider.class))context.registerProvider(type,(IProvider)provider(type));
        new ProtocolServer(System.in,System.out,context).run();
    }
}
