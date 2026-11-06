import com.android.layoutlib.bridge.Bridge;
import com.android.ide.common.rendering.api.*;
import com.android.ide.common.resources.*;
import com.android.ide.common.resources.configuration.*;
import com.android.resources.*;
import com.android.resources.aar.FrameworkResourceRepository;
import com.android.tools.res.apk.ApkResourceRepository;
import com.google.gson.*;
import org.kxml2.io.KXmlParser;
import java.nio.file.*;import java.io.*;import java.util.*;import java.util.zip.*;

public class WexPreviewRenderer {
 static class Parser extends KXmlParser implements ILayoutPullParser {
  Parser(String xml)throws Exception {setFeature(FEATURE_PROCESS_NAMESPACES,true);setInput(new StringReader(xml));}
  public Object getViewCookie(){return null;} public ResourceNamespace getLayoutNamespace(){return ResourceNamespace.RES_AUTO;}
 }
 static class Callback extends LayoutlibCallback {
  public Object loadView(String n,Class[] p,Object[] a)throws Exception{return Class.forName(n).getConstructor(p).newInstance(a);}
  public ResourceReference resolveResourceId(int id){return Bridge.resolveResourceId(id);}
  public int getOrGenerateResourceId(ResourceReference ref){return Bridge.getResourceId(ref.getResourceType(),ref.getName());}
  public ILayoutPullParser getParser(ResourceValue v){try{return new Parser(Files.readString(Path.of(v.getValue())));}catch(Exception e){throw new RuntimeException(e);}}
  public AdapterBinding getAdapterBinding(Object c,Map<String,String> a){return null;}public ActionBarCallback getActionBarCallback(){return new ActionBarCallback();}
  public org.xmlpull.v1.XmlPullParser createXmlParser(){KXmlParser p=new KXmlParser();try{p.setFeature(KXmlParser.FEATURE_PROCESS_NAMESPACES,true);}catch(Exception e){throw new RuntimeException(e);}return p;}
  public org.xmlpull.v1.XmlPullParser createXmlParserForPsiFile(String s){return createXmlParser();}
  public org.xmlpull.v1.XmlPullParser createXmlParserForFile(String s){try{var p=createXmlParser();p.setInput(new FileReader(s));return p;}catch(Exception e){throw new RuntimeException(e);}}
 }
 static Map<ResourceNamespace,Map<ResourceType,ResourceValueMap>> values(ResourceRepository repo,FolderConfiguration cfg) {
  var all=new HashMap<ResourceNamespace,Map<ResourceType,ResourceValueMap>>();
  for(var ns:repo.getNamespaces()) {var types=new EnumMap<ResourceType,ResourceValueMap>(ResourceType.class);all.put(ns,types);
   for(var type:repo.getResourceTypes(ns)){var values=ResourceValueMap.create();types.put(type,values);var entries=repo.getResources(ns,type);
    for(var name:entries.keySet()){var item=cfg.findMatchingConfigurable(entries.get(name));if(item!=null)values.put(item.getResourceValue());}
   }
  }return all;
 }

 static Path studio;
 static class ProjectLoader extends java.net.URLClassLoader {
  Map<String,String> aliases=new HashMap<>();
  ProjectLoader(List<String> paths)throws Exception {super(paths.stream().map(x->{try{return Path.of(x).toUri().toURL();}catch(Exception e){throw new RuntimeException(e);}}).toArray(java.net.URL[]::new),WexPreviewRenderer.class.getClassLoader());
   try(var z=new ZipFile(studio.resolve("plugins/design-tools/lib/layoutlib.jar").toFile())){var names=new HashSet<String>();for(var e:Collections.list(z.entries()))names.add(e.getName());for(var n:names)if(n.contains("/_Original_")&&n.endsWith(".class")&&!names.contains(n.replace("_Original_","")))aliases.put(n.replace("_Original_","").replaceFirst("\\.class$",""),n.replaceFirst("\\.class$",""));}
  }
  protected Class<?> loadClass(String n,boolean resolve)throws ClassNotFoundException {synchronized(getClassLoadingLock(n)) {
   Class<?> c=findLoadedClass(n);if(c==null&&(n.startsWith("androidx.")||n.startsWith("dev.")||n.startsWith("kotlinx."))&&findResource(n.replace('.','/')+".class")!=null)c=findClass(n);
   if(c==null)c=super.loadClass(n,false);if(resolve)resolveClass(c);return c;
  }}
  protected Class<?> findClass(String n)throws ClassNotFoundException {try(var in=findResource(n.replace('.','/')+".class").openStream()){
   byte[] data=in.readAllBytes();var reader=new org.objectweb.asm.ClassReader(data);var writer=new org.objectweb.asm.ClassWriter(0);
   reader.accept(new org.objectweb.asm.commons.ClassRemapper(writer,new org.objectweb.asm.commons.SimpleRemapper(aliases)),0);byte[] bytes=writer.toByteArray();return defineClass(n,bytes,0,bytes.length);
  }catch(Exception e){throw new ClassNotFoundException(n,e);}}
 }
 static class AppCallback extends Callback {
  ProjectLoader loader;String applicationId,resourcePackage;
  Map<Integer,ResourceReference> ids=new HashMap<>();Map<ResourceReference,Integer> rev=new HashMap<>();
  AppCallback(List<String> cp,String appId,String pkg)throws Exception {applicationId=appId;resourcePackage=pkg;loader=new ProjectLoader(cp);Thread.currentThread().setContextClassLoader(loader);
   for(String item:cp){Path path=Path.of(item);if(!item.endsWith(".jar"))continue;
    try(var z=new ZipFile(path.toFile())){for(var e:Collections.list(z.entries()))if(e.getName().contains("/R$")&&e.getName().endsWith(".class")){
     String n=e.getName().replace('/','.').replaceFirst("\\.class$","");Class<?> c=Class.forName(n,true,loader);String type=n.substring(n.indexOf("R$")+2);ResourceType t=ResourceType.fromClassName(type);if(t==null)continue;
     for(var f:c.getFields())if(f.getType()==int.class){int id=f.getInt(null);if((id>>>24)!=0x7f)continue;var ref=new ResourceReference(ResourceNamespace.RES_AUTO,t,f.getName());ids.put(id,ref);rev.put(ref,id);}
    }}
   }System.err.println("R IDS "+ids.size());
  }
  public ResourceReference resolveResourceId(int id){return (id>>>24)==1?Bridge.resolveResourceId(id):ids.get(id);}
  public int getOrGenerateResourceId(ResourceReference ref){if(ref.isFramework())return Bridge.getResourceId(ref.getResourceType(),ref.getName());Integer id=rev.get(ref);if(id==null)throw new IllegalArgumentException("Unknown app resource "+ref);return id;}
  public Class<?> findClass(String n)throws ClassNotFoundException{return Class.forName(n,true,loader);}
  public Object loadView(String n,Class[] p,Object[] a)throws Exception{return findClass(n).getConstructor(p).newInstance(a);}
  public String getApplicationId(){return applicationId;}public String getResourcePackage(){return resourcePackage;}
 }
 public static void main(String[] args)throws Exception {
  JsonObject input=JsonParser.parseString(Files.readString(Path.of(args[0]))).getAsJsonObject();
  studio=Path.of(input.get("studio").getAsString());var gson=new Gson();
  List<String> cp=gson.fromJson(input.get("classpath"),new com.google.gson.reflect.TypeToken<List<String>>(){}.getType());
  JsonObject build=input.getAsJsonObject("build");Path platformDir=Path.of(input.get("platform").getAsString());
  int width=input.get("width").getAsInt(),height=input.get("height").getAsInt();boolean dark=input.get("dark").getAsBoolean();
  if(width<2||height<2||width>2160||height>3840)throw new IllegalArgumentException("preview dimensions");
  var errors=new ArrayList<String>();var log=new ILayoutLog(){public void error(String t,String m,Throwable e,Object c,Object d){errors.add(t+" "+m);System.err.println("ERR "+t+" "+m);if(e!=null)e.printStackTrace();}public void error(String t,String m,Object c,Object d){errors.add(t+" "+m);System.err.println("ERR "+t+" "+m);}public void warning(String t,String m,Object c,Object d){System.err.println("WARN "+t+" "+m);}};
  Path data=studio.resolve("plugins/design-tools/resources/layoutlib/data");
  var props=new Properties();try(var in=Files.newInputStream(platformDir.resolve("build.prop"))){props.load(in);}var platform=new HashMap<String,String>();for(var name:props.stringPropertyNames())platform.put(name,props.getProperty(name));platform.put("ro.product.cpu.abilist","x86_64");platform.put("ro.product.cpu.abilist64","x86_64");
  var repo=FrameworkResourceRepository.create(platformDir.resolve("data/res"),Set.of("en"),null,false);
  var enums=new HashMap<String,Map<String,Integer>>();for(var item:repo.getResources(ResourceNamespace.ANDROID,ResourceType.ATTR).values())if(item.getResourceValue() instanceof AttrResourceValue attr&&!attr.getAttributeValues().isEmpty())enums.put(item.getName(),attr.getAttributeValues());
  Bridge bridge=new Bridge();if(!bridge.init(platform,data.resolve("fonts").toFile(),data.resolve("linux/lib64").toString(),Files.list(data.resolve("icu")).filter(p->p.toString().endsWith(".dat")).findFirst().orElseThrow().toString(),data.resolve("hyphen-data").toString(),new String[]{data.resolve("keyboards/Generic.kcm").toString()},enums,log))throw new Exception("init failed");
  int wdp=width/2,hdp=height/2;boolean landscape=width>height;String qualifiers="en-rUS-ldltr-sw"+Math.min(wdp,hdp)+"dp-w"+wdp+"dp-h"+hdp+"dp-normal-notlong-notround-"+(landscape?"land":"port")+"-"+(dark?"night":"notnight")+"-xhdpi-finger-keysexposed-nokeys-navexposed-nonav-v"+input.get("api").getAsInt();var cfg=FolderConfiguration.getConfigForQualifierString(qualifiers);
  String locale=input.has("locale")?input.get("locale").getAsString():"en-US";var language=java.util.Locale.forLanguageTag(locale);if(language.getLanguage().isEmpty())throw new IllegalArgumentException("Invalid preview locale");
  var localeQualifier=LocaleQualifier.getQualifier("b+"+language.toLanguageTag().replace('-','+'));if(localeQualifier==null)throw new IllegalArgumentException("Invalid locale qualifier");cfg.setLocaleQualifier(localeQualifier);
  boolean rtl=Set.of("ar","fa","he","ur").contains(language.getLanguage());cfg.setLayoutDirectionQualifier(new LayoutDirectionQualifier(rtl?LayoutDirection.RTL:LayoutDirection.LTR));qualifiers=cfg.getQualifierString();
  var callback=new AppCallback(cp,build.getAsJsonObject("apk").get("application_id").getAsString(),input.get("resource_package").getAsString());var app=new ApkResourceRepository(build.getAsJsonObject("apk").get("path").getAsString(),id->callback.resolveResourceId(id));
  var map=values(repo,cfg);map.putAll(values(app,cfg));System.err.println("Namespaces "+map.keySet());
  var resources=ResourceResolver.create(map,ResourceReference.style(ResourceNamespace.ANDROID,dark?"Theme.Material.NoActionBar":"Theme.Material.Light.NoActionBar"));resources.setLogger(log);
  String target=input.get("composable").getAsString();if(!target.matches("[A-Za-z_$][\\w$]*(?:\\.[A-Za-z_$][\\w$]*)+"))throw new IllegalArgumentException("invalid composable");
  var xml="<androidx.compose.ui.tooling.ComposeViewAdapter xmlns:android=\"http://schemas.android.com/apk/res/android\" xmlns:tools=\"http://schemas.android.com/tools\" android:layout_width=\"match_parent\" android:layout_height=\"match_parent\" tools:composableName=\""+target+"\" tools:paintBounds=\"false\"/>";
  var hardware=new HardwareConfig(width,height,Density.XHIGH,320,320,ScreenSize.NORMAL,landscape?ScreenOrientation.LANDSCAPE:ScreenOrientation.PORTRAIT,ScreenRound.NOTROUND,false);
  var params=new SessionParams(new Parser(xml),SessionParams.RenderingMode.NORMAL,"wex",hardware,resources,callback,input.get("min_sdk").getAsInt(),input.get("api").getAsInt(),log);params.setForceNoDecor();params.setLocale(language.toLanguageTag());params.setRtlSupport(true);if(input.has("show_background")&&input.get("show_background").getAsBoolean())params.setTransparentBackground();params.setUiMode(dark?0x21:0x11);params.setFontScale(input.get("font_scale").getAsFloat());
  var session=bridge.createSession(params);System.err.println("CREATE "+session.getResult());if(!session.getResult().isSuccess())throw new Exception(session.getResult().toString(),session.getResult().getException());
  for(int i=1;i<=4;i++){session.setElapsedFrameTimeNanos(i*1_000_000_000L);var rendered=session.render(10000);System.err.println("RENDER "+rendered);if(!rendered.isSuccess())throw new Exception("render failed: "+rendered,rendered.getException());}
  if(!errors.isEmpty())throw new Exception("Layoutlib errors: "+errors);
  var image=session.getImage();if(image==null)throw new Exception("no image");
  boolean showBackground=input.has("show_background")&&input.get("show_background").getAsBoolean();long background=input.has("background_color")?input.get("background_color").getAsLong():0;
  if(showBackground){var composite=new java.awt.image.BufferedImage(image.getWidth(),image.getHeight(),java.awt.image.BufferedImage.TYPE_INT_ARGB);var graphics=composite.createGraphics();graphics.setColor(new java.awt.Color((int)background,true));graphics.fillRect(0,0,image.getWidth(),image.getHeight());graphics.drawImage(image,0,0,null);graphics.dispose();image=composite;}
  javax.imageio.ImageIO.write(image,"png",new File(input.get("output").getAsString()));
  var views=session.getRootViews();if(views.isEmpty())throw new Exception("No root views");
  Object adapter=views.get(0).getViewObject();
  if(!adapter.getClass().getName().equals("androidx.compose.ui.tooling.ComposeViewAdapter"))throw new Exception("Unexpected root "+adapter);
  Object viewInfos=adapter.getClass().getMethod("getViewInfos$ui_tooling").invoke(adapter);
  if(!(viewInfos instanceof List<?> list)||list.isEmpty())throw new Exception("Compose produced no view info");
  JsonObject receipt=new JsonObject();receipt.addProperty("result","HEADLESS_COMPOSE_LAYOUTLIB_OK");receipt.addProperty("engine",bridge.getClass().getName());receipt.addProperty("headless",java.awt.GraphicsEnvironment.isHeadless());receipt.addProperty("width",image.getWidth());receipt.addProperty("height",image.getHeight());receipt.addProperty("composable",target);receipt.addProperty("compose_groups",((List<?>)viewInfos).size());receipt.addProperty("resource_ids",callback.ids.size());receipt.addProperty("emulator_used",false);receipt.addProperty("qualifiers",qualifiers);receipt.addProperty("font_scale",input.get("font_scale").getAsFloat());receipt.addProperty("night",dark);receipt.addProperty("locale",language.toLanguageTag());receipt.addProperty("show_background",showBackground);receipt.addProperty("background_color",background);
  Files.writeString(Path.of(input.get("receipt").getAsString()),gson.toJson(receipt));System.out.println("HEADLESS_COMPOSE_LAYOUTLIB_OK "+image.getWidth()+"x"+image.getHeight());session.dispose();bridge.dispose();
 }
}
