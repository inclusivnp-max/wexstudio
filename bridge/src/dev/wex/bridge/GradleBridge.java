package dev.wex.bridge;

import com.android.builder.model.v2.models.*;
import com.android.builder.model.v2.ide.*;
import com.google.gson.Gson;
import org.gradle.tooling.*;
import org.gradle.tooling.model.gradle.BasicGradleProject;
import java.io.*;
import java.util.*;

/** One short-lived Tooling API client; Gradle's own daemon is reused. stdout is JSON only. */
public final class GradleBridge {
    public static void main(String[] args) throws Exception {
        if (args.length < 2) throw new IllegalArgumentException("GradleBridge ROOT VARIANT [--offline]");
        File root = new File(args[0]).getCanonicalFile();
        GradleConnector connector = GradleConnector.newConnector().forProjectDirectory(root);
        String cached = System.getenv("WEX_CACHED_GRADLE");
        if (cached == null || !new File(cached, "bin/gradle").isFile())
            throw new IllegalArgumentException("Gradle distribution download disabled; WEX_CACHED_GRADLE required");
        connector.useInstallation(new File(cached));
        // Rust validates that this is the already installed project-wrapper version.
        try (ProjectConnection connection = connector.connect()) {
            var action = connection.action(new AndroidModels(args[1]));
            List<String> flags = new ArrayList<>(List.of("--console=plain", "-Dorg.gradle.configuration-cache=false"));
            flags.addAll(List.of("--init-script",System.getProperty("wex.bridge.init"),"-Dwex.bridge.classes="+System.getProperty("wex.bridge.classes"),"-Pwex.variant="+args[1]));
            if (Arrays.asList(args).contains("--offline")) flags.add("--offline");
            action.withArguments(flags).setStandardOutput(System.err).setStandardError(System.err);
            System.out.println(new Gson().toJson(action.run()));
        }
    }

    public static final class AndroidModels implements BuildAction<Map<String,Object>>, Serializable {
        private static final long serialVersionUID = 1L;
        private final String selected;
        AndroidModels(String selected) { this.selected = selected; }
        @Override public Map<String,Object> execute(BuildController controller) {
            List<Map<String,Object>> modules = new ArrayList<>();
            for (BasicGradleProject project : controller.getBuildModel().getProjects()) {
                BasicAndroidProject basic = controller.findModel(project, BasicAndroidProject.class);
                if (basic == null) continue;
                AndroidProject android = controller.getModel(project, AndroidProject.class);
                Variant variant = android.getVariants().stream().filter(v -> v.getName().equals(selected)).findFirst()
                    .orElseThrow(() -> new IllegalArgumentException(project.getPath()+": missing variant "+selected));
                VariantDependencies deps = controller.getModel(project, VariantDependencies.class, ModelBuilderParameter.class, p -> {
                    p.setVariantName(selected); p.setDontBuildRuntimeClasspath(true);
                    p.setDontBuildUnitTestRuntimeClasspath(true); p.setDontBuildAndroidTestRuntimeClasspath(true);
                    p.setDontBuildTestFixtureRuntimeClasspath(true); p.setDontBuildScreenshotTestRuntimeClasspath(true);
                    p.setDontBuildHostTestRuntimeClasspath(Collections.emptyMap()); p.setAdditionalArtifactsInModel(false);
                });
                if (!deps.getMainArtifact().getUnresolvedDependencies().isEmpty())
                    throw new IllegalStateException(project.getPath()+": unresolved Android compile dependencies: "+deps.getMainArtifact().getUnresolvedDependencies());
                Set<String> sources=new LinkedHashSet<>(), resources=new LinkedHashSet<>(), classpath=new LinkedHashSet<>(), projectDeps=new LinkedHashSet<>();
                source(basic.getMainSourceSet().getSourceProvider(), sources, resources);
                BasicVariant bv=basic.getVariants().stream().filter(v->v.getName().equals(selected)).findFirst().orElseThrow();
                if(bv.getMainArtifact().getVariantSourceProvider()!=null) source(bv.getMainArtifact().getVariantSourceProvider(),sources,resources);
                if(bv.getMainArtifact().getMultiFlavorSourceProvider()!=null) source(bv.getMainArtifact().getMultiFlavorSourceProvider(),sources,resources);
                for(SourceSetContainer set: basic.getBuildTypeSourceSets()) if(set.getSourceProvider().getName().equals(bv.getBuildType())) source(set.getSourceProvider(), sources, resources);
                for(SourceSetContainer set: basic.getProductFlavorSourceSets()) if(bv.getProductFlavors().contains(set.getSourceProvider().getName())) source(set.getSourceProvider(), sources, resources);
                // Resolve only libraries reachable from compile dependencies, never runtime/test graphs.
                Set<String> keys=new LinkedHashSet<>();
                for(GraphItem item:deps.getMainArtifact().getCompileDependencies()) collect(item, keys);
                for(String key:keys) {
                    Library library=deps.getLibraries().get(key);
                    if(library==null) throw new IllegalStateException("Missing library "+key);
                    if(library.getProjectInfo()!=null) projectDeps.add(library.getProjectInfo().getProjectPath());
                    if(library.getAndroidLibraryData()!=null) add(classpath,library.getAndroidLibraryData().getCompileJarFiles());
                    else if(library.getArtifact()!=null && library.getArtifact().getName().endsWith(".jar")) classpath.add(library.getArtifact().getAbsolutePath());
                }
                AndroidArtifact artifact=variant.getMainArtifact();
                add(classpath,artifact.getGeneratedClassPaths().values());
                GeneratedArtifacts generated=controller.getModel(project,GeneratedArtifacts.class);
                classpath.addAll(generated.getResourceJars());
                Map<String,Object> m=new LinkedHashMap<>();
                m.put("name",project.getPath());m.put("directory",project.getProjectDirectory().getAbsolutePath());
                m.put("namespace",android.getNamespace());m.put("project_type",basic.getProjectType().toString());
                m.put("source_roots",new ArrayList<>(sources));m.put("resource_roots",new ArrayList<>(resources));
                m.put("generated_source_roots",paths(artifact.getGeneratedSourceFolders()));
                m.put("generated_resource_roots",paths(artifact.getGeneratedResourceFolders()));
                m.put("boot_classpath",paths(basic.getBootClasspath()));m.put("classpath",new ArrayList<>(classpath));
                m.put("module_dependencies",new ArrayList<>(projectDeps));m.put("selected_variant",selected);
                m.put("variants",basic.getVariants().stream().map(BasicVariant::getName).sorted().toList());
                m.put("classes_roots",paths(artifact.getClassesFolders()));
                m.put("assemble_task",project.getPath()+":"+artifact.getAssembleTaskName());
                m.put("application_id",artifact.getApplicationId());
                File listing=artifact.getAssembleTaskOutputListingFile();m.put("apk_listing_file",listing==null?"":listing.getAbsolutePath());
                modules.add(m);
                ProjectSyncIssues issues=controller.findModel(project,ProjectSyncIssues.class);
                if(issues!=null) for(SyncIssue issue:issues.getSyncIssues()) if(issue.getSeverity()==SyncIssue.SEVERITY_ERROR)
                    throw new IllegalStateException(project.getPath()+": "+issue.getMessage());
            }
            if(modules.isEmpty()) throw new IllegalArgumentException("No Android modules found");
            return new LinkedHashMap<>(Map.of("modules",modules,"model_api","com.android.builder.model.v2","model_version","9.2.1"));
        }
        private static void source(SourceProvider s,Set<String> sources,Set<String> resources) {
            add(sources,s.getJavaDirectories());add(sources,s.getKotlinDirectories());add(resources,s.getResDirectories());
        }
        private static void collect(GraphItem item,Set<String> keys) {if(keys.add(item.getKey())) for(GraphItem child:item.getDependencies()) collect(child,keys);}
        private static void add(Set<String> target,Collection<File> files) {for(File f:files) target.add(f.getAbsolutePath());}
        private static List<String> paths(Collection<File> files) {return files.stream().map(File::getAbsolutePath).sorted().toList();}
    }
}
