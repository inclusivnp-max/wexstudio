//! Actual native Connection, file watching, isolated K2 replacement and dirty replay.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use wexandroidstudio::{
    editor::Document,
    gradle::SyncOptions,
    kotlin,
    language::{Connection, Event, ModelPhase, Request, RopeBuffer, RopeSnapshot},
    workspace_edit::Buffer,
};
struct Probe {
    connection: Connection,
    path: PathBuf,
    text: String,
    others: Vec<Buffer>,
    version: i32,
    applied: u64,
    failed: u64,
    events: Vec<Value>,
    ready: Vec<Value>,
}
impl Probe {
    async fn event(&mut self) -> Result<Event> {
        let event = tokio::time::timeout(Duration::from_secs(210), self.connection.events.recv())
            .await?
            .context("native connection closed")?;
        match &event {
            Event::Model(s) => {
                self.events.push(serde_json::to_value(s)?);
                if s.phase == ModelPhase::Applied {
                    self.applied = s.attempt;
                }
                if s.phase == ModelPhase::Failed {
                    self.failed = s.attempt;
                }
                eprintln!(
                    "MODEL {:?} rev{} attempt{} retained{} {}",
                    s.phase, s.revision, s.attempt, s.retained, s.detail
                );
            }
            Event::Ready {
                variant,
                modules,
                cache_hit,
                ..
            } => self
                .ready
                .push(json!({"variant":variant,"modules":modules,"cache_hit":cache_hit})),
            Event::Status(s) => {
                eprintln!("STATUS {s}");
            }
            _ => (),
        }
        Ok(event)
    }
    fn snapshot(&mut self, text: String) {
        self.text = text;
        self.version += 1;
        self.connection.document.send_rope(Some(RopeSnapshot {
            path: self.path.clone(),
            text: ropey::Rope::from_str(&self.text),
            version: self.version,
            others: self
                .others
                .iter()
                .map(|b| RopeBuffer {
                    path: b.path.clone(),
                    text: ropey::Rope::from_str(&b.text),
                    version: b.version,
                })
                .collect(),
        }));
    }
    async fn applied_after(&mut self, attempt: u64) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(210);
        while self.applied <= attempt {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "no replacement after attempt{attempt}: {:?}",
                self.events
            );
            let e = self.event().await?;
            if let Event::Model(s) = e
                && s.phase == ModelPhase::Failed
            {
                anyhow::bail!("unexpected model error: {}", s.detail);
            }
        }
        Ok(())
    }
    async fn failed_after(&mut self, attempt: u64) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
        while self.failed <= attempt {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "expected refresh failure missing"
            );
            self.event().await?;
        }
        Ok(())
    }
    async fn complete(
        &mut self,
        original: &str,
        needle: &str,
        partial: &str,
        expected: &str,
    ) -> Result<Vec<String>> {
        let text = original.replace(needle, partial);
        ensure!(text != original, "completion anchor unchanged");
        let at = text.find(partial).context("partial anchor missing")? + partial.len();
        let doc = Document::new(&text)?;
        let (line, character) = doc.lsp_position(text[..at].chars().count())?;
        self.snapshot(text);
        let version = self.version;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            self.connection
                .requests
                .send(Request::Complete {
                    line,
                    character,
                    path: self.path.clone(),
                    version,
                })
                .await?;
            loop {
                if let Event::Completion {
                    path,
                    version: v,
                    items,
                } = self.event().await?
                    && path == self.path
                    && v == version
                {
                    let labels = items.into_iter().map(|c| c.label).collect::<Vec<_>>();
                    if labels.iter().any(|s| s.starts_with(expected)) {
                        self.snapshot(original.to_owned());
                        return Ok(labels);
                    }
                    break;
                }
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "expected completion {expected} absent"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    async fn quiet(&mut self, seconds: u64) -> Result<()> {
        let end = tokio::time::Instant::now() + Duration::from_secs(seconds);
        while tokio::time::Instant::now() < end {
            if let Ok(e) = tokio::time::timeout(Duration::from_millis(200), self.event()).await {
                e?;
            }
        }
        Ok(())
    }
}
#[test]
#[ignore = "Requires MODEL_RELOAD_PROJECT/private SDK/JDK and actual K2"]
fn actual_model_watch_and_session_replacement() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let root=PathBuf::from(std::env::var("MODEL_RELOAD_PROJECT")?).canonicalize()?;let output=PathBuf::from(std::env::var("MODEL_RELOAD_OUTPUT")?);
        let path=root.join("app/src/main/kotlin/dev/wex/android/ReloadProbe.kt");let disk=std::fs::read_to_string(&path)?;
        let shared=root.join("shared/src/main/kotlin/dev/wex/shared/Greeter.kt");let shared_disk=std::fs::read_to_string(&shared)?;
        let parked=shared_disk.replacen("class Greeter {","class Greeter {\n    fun parkedOnly(): String = \"parked\"",1);ensure!(parked!=shared_disk,"parked anchor missing");
        let original=format!("{disk}\n// unsaved active overlay\nfun dirtyDependency(): String = dev.wex.shared.Greeter().parkedOnly()\n");
        let connection=Connection::start(&runtime,root.clone(),kotlin::default_server(),SyncOptions{offline:true,..Default::default()});
        let mut p=Probe{connection,path:path.clone(),text:String::new(),others:vec![Buffer{path:shared.clone(),text:parked,version:2}],version:10,applied:0,failed:0,events:vec![],ready:vec![]};
        p.snapshot(original.clone());p.applied_after(0).await?;
        p.complete(&original,".parkedOnly()",".parkedO","parkedOnly").await?;
        let first=p.applied;let pointer=root.join(".wex/analysis/debug/current.json");let first_pointer=std::fs::read(&pointer)?;
        // Changing only editor snapshots must not invoke SDK or replace K2. Disable
        // every PRIVATE tool execute bit, including java; the running JVM survives.
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let sdk=PathBuf::from(std::env::var("ANDROID_HOME")?);let jdk=PathBuf::from(std::env::var("JAVA_HOME")?);
            let mut tools=vec![sdk.join("build-tools/36.0.0/aapt2")];tools.extend(std::fs::read_dir(jdk.join("bin"))?.map(|e|e.map(|e|e.path())).collect::<std::io::Result<Vec<_>>>()?);
            for tool in &tools{std::fs::set_permissions(tool,std::fs::Permissions::from_mode(0o644))?;}
            let result=async {p.complete(&original,"R.string.app_name","R.string.app_","app_name").await?;p.quiet(2).await?;ensure!(p.applied==first&&std::fs::read(&pointer)?==first_pointer,"source edit triggered SDK/model replacement");Ok::<_,anyhow::Error>(())}.await;
            for tool in &tools{std::fs::set_permissions(tool,std::fs::Permissions::from_mode(0o755))?;}result?;
        }
        let resource=root.join("app/src/main/res/values/strings.xml");let base=std::fs::read_to_string(&resource)?;
        let added=base.replace("</resources>","<string name=\"watch_added\">watched</string></resources>");ensure!(added!=base,"resource anchor missing");
        std::fs::write(&resource,&added)?;p.applied_after(first).await?;
        let added_overlay=original.replace("R.string.app_name","R.string.watch_added");
        p.complete(&added_overlay,"R.string.watch_added","R.string.watch_","watch_added").await?;
        p.complete(&added_overlay,".parkedOnly()",".parkedO","parkedOnly").await?;
        let retained=p.applied;let last_good=std::fs::read(&pointer)?;let failed=p.failed;
        std::fs::write(&resource,"<resources><broken>")?;p.failed_after(failed).await?;
        ensure!(std::fs::read(&pointer)?==last_good,"bad resource replaced last-good model");
        p.complete(&added_overlay,"R.string.watch_added","R.string.watch_","watch_added").await?;p.quiet(2).await?;ensure!(p.applied==retained,"failed refresh replaced active K2");
        let config=root.join("wex.json");let mut cfg:Value=serde_json::from_slice(&std::fs::read(&config)?)?;
        let extra=root.join("extra/src");std::fs::create_dir_all(&extra)?;std::fs::write(extra.join("Extra.kt"),"package dev.wex.extra\nclass Extra { fun answer(): Int = 42 }\n")?;
        cfg["modules"].as_array_mut().unwrap().push(json!({"name":"extra","namespace":"dev.wex.extra","sources":["extra/src"],"dependencies":[]}));
        cfg["modules"][0]["projects"].as_array_mut().unwrap().push(json!("extra"));
        // A burst of content writes + config/lock mediation should publish only the
        // newest model, never a cancelled intermediate generation/session.
        for n in 0..3 {std::fs::write(&resource,added.replace("watched",&format!("burst{n}")))?;tokio::time::sleep(Duration::from_millis(100)).await;}
        std::fs::write(&config,serde_json::to_vec_pretty(&cfg)?)?;
        p.applied_after(retained).await?;
        let with_extra=format!("{added_overlay}\nfun extraProof(): Int = dev.wex.extra.Extra().answer()\n");
        p.complete(&with_extra,".answer()",".ans","answer").await?;
        p.complete(&with_extra,".parkedOnly()",".parkedO","parkedOnly").await?;
        let good_config=std::fs::read(&config)?;let before=p.applied;let failed=p.failed;
        std::fs::write(&config,"{")?;p.failed_after(failed).await?;
        p.complete(&with_extra,".answer()",".ans","answer").await?;
        std::fs::remove_file(&config)?;let failed=p.failed;p.failed_after(failed).await?;
        p.complete(&with_extra,".parkedOnly()",".parkedO","parkedOnly").await?;
        std::fs::write(&config,good_config)?;p.applied_after(before).await?;
        p.connection.requests.send(Request::Refresh{force:false,variant:Some("release".into())}).await?;
        let before=p.applied;p.applied_after(before).await?;
        let at=with_extra.find("BuildConfig.DEBUG").unwrap()+2;let doc=Document::new(&with_extra)?;let (line,character)=doc.lsp_position(with_extra[..at].chars().count())?;
        p.snapshot(with_extra.clone());let version=p.version;
        p.connection.requests.send(Request::Definition{line,character,path:path.clone(),version,navigation:123}).await?;
        let definition=loop {if let Event::Definition{path:p0,version:v,locations,navigation:123}=p.event().await? && p0==path&&v==version {break locations;}};
        let (generated,_)=wexandroidstudio::language::definition_path(&definition)?.context("release definition missing")?;
        ensure!(generated.starts_with(root.join(".wex/analysis/release"))&&std::fs::read_to_string(&generated)?.contains("DEBUG = false"),"variant swap not release");
        ensure!(std::fs::read_to_string(&path)?==disk&&std::fs::read_to_string(&shared)?==shared_disk,"reload wrote unsaved buffers");
        ensure!(!root.join(".wex/native").exists()&&!std::fs::read_dir(root.join(".wex/analysis"))?.filter_map(|e|e.ok()).any(|e|e.path().extension().is_some_and(|x|x=="apk")),"watcher built APK");
        let report=json!({"result":"MODEL_RELOAD_K2_OK","model_events":p.events,"ready":p.ready,"release_definition":definition,"dirty_active_and_parked_disk_unchanged":true,"resource_auto_refresh":true,"invalid_resource_keeps_old_k2":true,"config_add_module":true,"invalid_deleted_config_recovers_without_gradle":true,"sdk_disabled_body_edit_no_reload":true,"no_apk":true});
        p.connection.requests.send(Request::Stop).await?;(&mut p.connection.task).await?;
        std::fs::write(output,serde_json::to_vec_pretty(&report)?)?;
        eprintln!("MODEL_RELOAD_K2_OK: automatic resource/config changes; last-good K2 survives SDK/config errors; active+parked dirty buffers replayed; source edits with SDK execution disabled cause no refresh; real completion/definition and release DEBUG=false; no Gradle/APK");
        Ok(())
    })
}
