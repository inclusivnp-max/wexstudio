//! Background editor connection: latest-buffer watch channel + bounded request/event queues.
use crate::{
    gradle::{SyncOptions, sync_project},
    kotlin::{CompletionItem, Diagnostic, KotlinSession, file_uri, uri_path},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{path::PathBuf, time::Duration};
use tokio::sync::{mpsc, watch};
#[path = "language_reload.rs"]
mod reload;
#[path = "language_snapshots.rs"]
mod snapshots;
use snapshots::channel as snapshot_channel;
pub use snapshots::{Receiver as SnapshotReceiver, RopeBuffer, RopeSnapshot, SnapshotSender};

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub enum ModelPhase {
    Waiting,
    Preparing,
    Warming,
    Applied,
    Failed,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelStatus {
    pub revision: u64,
    pub attempt: u64,
    pub phase: ModelPhase,
    pub detail: String,
    pub retained: bool,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub path: PathBuf,
    pub text: String,
    pub version: i32,
    pub others: Vec<crate::workspace_edit::Buffer>,
}
#[derive(Clone, Debug)]
pub enum ProjectOperation {
    PrepareRename,
    Rename(String),
    References,
}
#[derive(Debug)]
pub struct ProjectResponse {
    pub capture: crate::workspace_edit::Capture,
    pub operation: ProjectOperation,
    pub value: Value,
    pub plan: Option<crate::workspace_edit::Plan>,
}
#[derive(Debug)]
pub enum Request {
    Formatting {
        id: u64,
        path: PathBuf,
        version: i32,
        range: Option<crate::kotlin::Range>,
        cancel: watch::Receiver<bool>,
    },
    Complete {
        line: u32,
        character: u32,
        path: PathBuf,
        version: i32,
    },
    Definition {
        navigation: u64,
        line: u32,
        character: u32,
        path: PathBuf,
        version: i32,
    },
    ApplyCompletion {
        path: PathBuf,
        version: i32,
        item: Box<CompletionItem>,
    },
    Project {
        id: u64,
        operation: ProjectOperation,
        path: PathBuf,
        version: i32,
        line: u32,
        character: u32,
        buffers: Vec<RopeBuffer>,
    },
    Saved,
    Refresh {
        force: bool,
        variant: Option<String>,
    },
    Stop,
}
#[derive(Debug)]
pub enum Event {
    Formatting {
        id: u64,
        result: std::result::Result<Box<crate::formatting::Review>, String>,
    },
    Model(ModelStatus),
    OutsideModel {
        path: PathBuf,
        version: i32,
    },
    Project {
        id: u64,
        result: std::result::Result<ProjectResponse, String>,
    },
    WorkspaceEdit {
        path: PathBuf,
        version: i32,
        edit: Value,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Ready {
        variant: String,
        compose_enabled: Option<bool>,
        cache_hit: bool,
        modules: usize,
        server_info: Value,
    },
    Status(String),
    Completion {
        path: PathBuf,
        version: i32,
        items: Vec<CompletionItem>,
    },
    Diagnostics {
        path: PathBuf,
        version: i32,
        items: Vec<Diagnostic>,
    },
    Definition {
        navigation: u64,
        path: PathBuf,
        version: i32,
        locations: Value,
    },
}
pub struct Connection {
    pub document: SnapshotSender,
    pub requests: mpsc::Sender<Request>,
    pub events: mpsc::Receiver<Event>,
    pub task: tokio::task::JoinHandle<()>,
}
impl Connection {
    pub fn start_native(
        runtime: &tokio::runtime::Runtime,
        root: PathBuf,
        server: PathBuf,
        options: SyncOptions,
    ) -> Self {
        reload::start(runtime, root, server, options)
    }
    pub fn start(
        runtime: &tokio::runtime::Runtime,
        root: PathBuf,
        server: PathBuf,
        options: SyncOptions,
    ) -> Self {
        if root.join("wex.json").is_file() {
            return reload::start(runtime, root, server, options);
        }
        let (document, mut snapshots) = snapshot_channel();
        let (requests, mut commands) = mpsc::channel(16);
        let (tx, events) = mpsc::channel(128);
        let task=runtime.spawn(async move{
            let result:Result<()>=async{
                let (graph,cache_hit)=if root.join("wex.json").is_file(){let model=crate::analysis::prepare(&root,&options.variant,options.offline,options.force,&|event|{if let crate::run::RunEvent::Phase(note)=event {let _=tx.try_send(Event::Status(note));}}).await?;(model.graph,model.cache_hit)} else {let sync=sync_project(&root,&options).await?;(sync.graph,sync.cache_hit)};let mut session=KotlinSession::start(&root,&graph,&server).await?;
                let _=tx.send(Event::Ready{variant:options.variant.clone(),compose_enabled:None,cache_hit,modules:graph.modules.len(),server_info:session.capabilities["serverInfo"].clone()}).await;
                let mut workspace_ready=false;let mut current:Option<Snapshot>=None;let mut opened=std::collections::BTreeMap::<PathBuf,crate::workspace_edit::Buffer>::new();let mut wire=std::collections::BTreeMap::<PathBuf,i32>::new();let pull=!session.capabilities["capabilities"]["diagnosticProvider"].is_null();let mut needs_diagnostics=false;
                let mut tick=tokio::time::interval(Duration::from_millis(350));tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                async fn sync_buffer(session:&KotlinSession,current:&mut Option<Snapshot>,opened:&mut std::collections::BTreeMap<PathBuf,crate::workspace_edit::Buffer>,wire:&mut std::collections::BTreeMap<PathBuf,i32>,next:Option<Snapshot>)->Result<()>{
                    let mut desired=std::collections::BTreeMap::new();
                    if let Some(next)=&next {
                        for b in &next.others {desired.insert(b.path.clone(),b.clone());}
                        desired.insert(next.path.clone(),crate::workspace_edit::Buffer{path:next.path.clone(),text:next.text.clone(),version:next.version});
                    }
                    for p in opened.keys().filter(|p|!desired.contains_key(*p)) {session.close(p).await?;wire.remove(p);}
                    for (p,b) in &desired {
                        match opened.get(p) {
                            None=>{session.open(p,&b.text,b.version).await?;wire.insert(p.clone(),b.version);},
                            Some(old) if old.text!=b.text=>{let version=wire[p].checked_add(1).context("K2 wire version exhausted")?;session.change(p,&b.text,version).await?;wire.insert(p.clone(),version);},
                            _=>{}
                        }
                    }
                    *opened=desired;*current=next;Ok(())
                }
                // watch keeps the initial snapshot even if the UI sent it during server startup.
                let initial=snapshots.latest().await?;sync_buffer(&session,&mut current,&mut opened,&mut wire,initial).await?;needs_diagnostics|=current.is_some();
                loop{tokio::select!{
                    changed=snapshots.changed()=>{if changed.is_err(){break;}let next=match snapshots.latest().await {Ok(s)=>s,Err(e)=>{let _=tx.send(Event::Status(format!("K2 snapshot: {e:#}; editor and last-good K2 retained"))).await;continue;}};sync_buffer(&session,&mut current,&mut opened,&mut wire,next).await?;needs_diagnostics=true;},
                    request=commands.recv()=>{
                        let Some(request)=request else{break;};if matches!(request,Request::Stop){break;}
                        let latest=match snapshots.latest().await {Ok(s)=>s,Err(e)=>{let _=tx.send(snapshot_failure(&request,format!("K2 snapshot: {e:#}; editor and last-good K2 retained"))).await;continue;}};sync_buffer(&session,&mut current,&mut opened,&mut wire,latest).await?;
                        if current.is_none(){let _=tx.send(snapshot_failure(&request,"No current source snapshot".into())).await;continue;}
                        if let Some(buffer)=&current{let response=match request{
                            Request::Formatting{id,path,version,range,cancel}=>{let result=if path!=buffer.path||version!=buffer.version {Err("Source snapshot changed; preview again".into())}else{format_request(&session,&root,buffer,range,cancel).await.map(Box::new).map_err(|e|format!("{e:#}").chars().take(4096).collect())};Ok(Some(Event::Formatting{id,result}))},
                            Request::Complete{line,character,path,version}=>{if path!=buffer.path||version!=buffer.version{let _=tx.send(Event::Completion{path,version,items:vec![]}).await;continue;}session.completion(&buffer.path,line,character).await.map(|items|Some(Event::Completion{path:buffer.path.clone(),version:buffer.version,items}))},
                            Request::Definition{line,character,path,version,navigation}=>{if path!=buffer.path||version!=buffer.version{continue;}session.definition(&buffer.path,line,character).await.map(|locations|Some(Event::Definition{path:buffer.path.clone(),version:buffer.version,locations,navigation}))},
                            Request::ApplyCompletion{path,version,item}=>{
                                if path!=buffer.path||version!=buffer.version{continue;}
                                let command=item.extra.get("command").cloned().unwrap_or(Value::Null);
                                let result=session.client.request("workspace/executeCommand",serde_json::json!({"command":command["command"],"arguments":command["arguments"]}));
                                tokio::pin!(result);
                                loop{tokio::select!{
                                    response=&mut result=>{response?;break;},
                                    request=session.client.next_server_request()=>{if let Some(request)=request{let (reply,wait)=tokio::sync::oneshot::channel();let mut edit=request.params["edit"].clone();normalize_edit_versions(&mut edit,&wire,&opened)?;let _=tx.send(Event::WorkspaceEdit{path:buffer.path.clone(),version:buffer.version,edit,reply}).await;let applied=wait.await.unwrap_or(false);let _=request.reply.send(serde_json::json!({"applied":applied}));}else{anyhow::bail!("K2 edit request stream closed");}}
                                }}Ok(None)
                            },
                            Request::Project{id,operation,path,version,line,character,buffers}=>{
                                if !workspace_ready {
                                    let ready=tokio::time::timeout(Duration::from_secs(180),async {
                                        while let Some(event)=session.client.events.recv().await {if workspace_ready_event(&event){return Ok::<_,anyhow::Error>(());}}
                                        anyhow::bail!("K2 import stream closed")
                                    }).await;
                                    match ready {Ok(Ok(()))=>workspace_ready=true,other=>{let _=tx.send(Event::Project{id,result:Err(format!("K2 project import not ready: {other:?}"))}).await;continue;}}
                                }
                                if path!=buffer.path||version!=buffer.version {
                                    let _=tx.send(Event::Project{id,result:Err("Source snapshot changed; request again".into())}).await;continue;
                                }
                                let result=project_request(&session,&root,operation,&path,line,character,buffers,&wire,&opened).await.map_err(|e|format!("{e:#}"));
                                Ok(Some(Event::Project{id,result}))
                            },
                            Request::Saved=>session.saved(&buffer.path).await.map(|_|None),Request::Refresh{..}=>Ok(None),Request::Stop=>unreachable!(),
                        };match response{Ok(Some(event))=>{if tx.send(event).await.is_err(){break;}},Ok(None)=>{},Err(e)=>{let _=tx.send(Event::Status(format!("K2: {e:#}"))).await;}}}
                    },
                    event=session.client.events.recv()=>{let Some(event)=event else{anyhow::bail!("K2 notification stream closed");};workspace_ready|=workspace_ready_event(&event);if event["method"]=="textDocument/publishDiagnostics"&&let Some(buffer)=&current&&event["params"]["uri"]==file_uri(&buffer.path)?{
                        let version=event["params"]["version"].as_i64().unwrap_or(wire[&buffer.path] as i64) as i32;
                        if version==wire[&buffer.path]{let version=buffer.version;let items=serde_json::from_value(event["params"]["diagnostics"].clone())?;let _=tx.send(Event::Diagnostics{path:buffer.path.clone(),version,items}).await;}
                    }},
                    _=tick.tick(),if pull&&needs_diagnostics&&current.is_some()=>{
                        if let Some(buffer)=&current{match session.pull_diagnostics(&buffer.path).await{
                            Ok(report)=>{if report["kind"]!="unchanged"{let items=serde_json::from_value(report["items"].clone())?;let _=tx.send(Event::Diagnostics{path:buffer.path.clone(),version:buffer.version,items}).await;}},
                            Err(e)=>{let _=tx.send(Event::Status(format!("K2 diagnostics: {e:#}"))).await;}
                        }}needs_diagnostics=false;
                    }
                }}
                for path in opened.keys(){let _=session.close(path).await;}session.shutdown().await?;Ok(())
            }.await;
            if let Err(e)=result{let _=tx.send(Event::Status(format!("K2 connection: {e:#}"))).await;}
        });
        Self {
            document,
            requests,
            events,
            task,
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

static RETIRING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FORCED_STOPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FAILED_STOP_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub fn shutdown_stats() -> serde_json::Value {
    use std::sync::atomic::Ordering;
    serde_json::json!({"pending":RETIRING.load(Ordering::Acquire),"forced":FORCED_STOPS.load(Ordering::Acquire),"spawn_failed":FAILED_STOP_WORKERS.load(Ordering::Acquire)})
}
/// Called from App::on_exit, never an ordinary frame or speculative file close.
/// Keep the runtime alive off the GPU thread until the actual actor stops/seals.
pub fn retire_after_window(
    connection: Connection,
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
) {
    retire_with_timeout(connection, runtime, Duration::from_secs(12));
}
fn retire_with_timeout(
    connection: Connection,
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    timeout: Duration,
) {
    use std::sync::atomic::Ordering;
    RETIRING.fetch_add(1, Ordering::AcqRel);
    let result = std::thread::Builder::new()
        .name("wex-k2-stop".into())
        .spawn(move || {
            struct Ticket;
            impl Drop for Ticket {
                fn drop(&mut self) {
                    RETIRING.fetch_sub(1, Ordering::Release);
                }
            }
            let ticket = Ticket;
            // Local declaration order also keeps the ticket alive through runtime
            // destruction if an unexpected unwind occurs.
            let owner = runtime;
            let mut retiring = connection;
            retiring.events.close();
            owner.block_on(async {
                let stopped = tokio::time::timeout(timeout, async {
                    let _ = retiring.requests.send(Request::Stop).await;
                    let _ = (&mut retiring.task).await;
                })
                .await;
                if stopped.is_err() {
                    FORCED_STOPS.fetch_add(1, Ordering::AcqRel);
                    retiring.task.abort();
                    let _ = (&mut retiring.task).await;
                }
            });
            drop(retiring);
            drop(owner);
            drop(ticket);
        });
    if result.is_err() {
        FAILED_STOP_WORKERS.fetch_add(1, Ordering::AcqRel);
        RETIRING.fetch_sub(1, Ordering::Release);
    }
}
/// The native main calls this only AFTER the GPU event loop has returned.
pub fn drain_on_exit() -> Result<()> {
    use std::sync::atomic::Ordering;
    let deadline = std::time::Instant::now() + Duration::from_secs(16);
    while RETIRING.load(Ordering::Acquire) != 0 {
        ensure!(
            std::time::Instant::now() < deadline,
            "K2 shutdown still pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    ensure!(
        FAILED_STOP_WORKERS.load(Ordering::Acquire) == 0,
        "K2 shutdown worker did not start"
    );
    Ok(())
}
/// Normalize standard Location and LocationLink results, preserving archive URIs as text.
pub fn first_definition(value: &Value) -> Option<(String, crate::kotlin::Position)> {
    let value = if value.is_array() {
        value.as_array()?.first()?
    } else {
        value
    };
    let uri = value
        .get("uri")
        .or_else(|| value.get("targetUri"))?
        .as_str()?
        .to_owned();
    let range = value
        .get("range")
        .or_else(|| value.get("targetSelectionRange"))?;
    let pos = serde_json::from_value(range["start"].clone()).ok()?;
    Some((uri, pos))
}
pub fn definition_path(value: &Value) -> Result<Option<(PathBuf, crate::kotlin::Position)>> {
    first_definition(value)
        .map(|(uri, pos)| Ok((uri_path(&uri)?, pos)))
        .transpose()
}

/// K2 cancellation follows its complete frame; Java owns a separately cancellable child.
pub async fn format_request(
    session: &KotlinSession,
    root: &std::path::Path,
    buffer: &Snapshot,
    range: Option<crate::kotlin::Range>,
    mut cancel: watch::Receiver<bool>,
) -> Result<crate::formatting::Review> {
    ensure!(
        !*cancel.borrow() && cancel.has_changed().is_ok(),
        "Formatting cancelled"
    );
    let dir = root.to_owned();
    let path = buffer.path.clone();
    let text = buffer.text.clone();
    let version = buffer.version;
    let capture = tokio::task::spawn_blocking(move || {
        crate::formatting::Capture::read(&dir, &path, text, version)
    })
    .await??;
    ensure!(
        !*cancel.borrow() && cancel.has_changed().is_ok(),
        "Formatting cancelled"
    );
    if crate::formatting::Backend::for_path(&buffer.path)? == crate::formatting::Backend::GoogleJava
    {
        return crate::java_format::review(capture, range, cancel).await;
    }
    // Never cancel the write: response cancellation starts only after its complete frame.
    let response = session
        .begin_formatting(&buffer.path, range.as_ref())
        .await?;
    let value = tokio::select! {
        result=tokio::time::timeout(Duration::from_secs(30),response.wait())=>result.context("K2 formatting timed out")??,
        _=cancel.changed()=>anyhow::bail!("Formatting cancelled"),
    };
    ensure!(
        !*cancel.borrow() && cancel.has_changed().is_ok(),
        "Formatting cancelled"
    );
    let review = tokio::task::spawn_blocking(move || capture.review(&value, range)).await??;
    ensure!(
        !*cancel.borrow() && cancel.has_changed().is_ok(),
        "Formatting cancelled"
    );
    Ok(review)
}
/// Every open tab is synchronized before this request, including unsaved parked code.
/// A fresh capture and final disk check run off the UI thread.
#[allow(clippy::too_many_arguments)]
async fn project_request(
    session: &KotlinSession,
    root: &std::path::Path,
    operation: ProjectOperation,
    path: &std::path::Path,
    line: u32,
    character: u32,
    buffers: Vec<RopeBuffer>,
    wire: &std::collections::BTreeMap<PathBuf, i32>,
    opened: &std::collections::BTreeMap<PathBuf, crate::workspace_edit::Buffer>,
) -> Result<ProjectResponse> {
    let buffers = snapshots::materialize_buffers(buffers).await?;
    ensure!(
        buffers.iter().all(|b| opened
            .get(&b.path)
            .is_some_and(|o| o.version == b.version && o.text == b.text)),
        "Workspace snapshot changed; request again"
    );
    let dir = root.to_owned();
    let input = buffers.clone();
    let capture =
        tokio::task::spawn_blocking(move || crate::workspace_edit::Capture::read(&dir, &input))
            .await??;
    let mut value = match &operation {
        ProjectOperation::PrepareRename => session.prepare_rename(path, line, character).await,
        ProjectOperation::Rename(name) => session.rename(path, line, character, name).await,
        ProjectOperation::References => session.references(path, line, character).await,
    }?;
    if matches!(operation, ProjectOperation::Rename(_)) {
        normalize_edit_versions(&mut value, wire, opened)?;
    }
    let is_rename = matches!(operation, ProjectOperation::Rename(_));
    let edit = value.clone();
    let (capture, plan) = tokio::task::spawn_blocking(move || {
        capture.verify_disk()?;
        let plan = if is_rename {
            Some(capture.plan(&edit)?)
        } else {
            None
        };
        Ok::<_, anyhow::Error>((capture, plan))
    })
    .await??;
    Ok(ProjectResponse {
        capture,
        operation,
        value,
        plan,
    })
}
/// UI snapshot clocks may jump while K2 counts actual document changes. Maintain
/// a strict per-document wire revision, verify it, then translate to the UI clock.
fn normalize_edit_versions(
    edit: &mut Value,
    wire: &std::collections::BTreeMap<PathBuf, i32>,
    opened: &std::collections::BTreeMap<PathBuf, crate::workspace_edit::Buffer>,
) -> Result<()> {
    if let Some(changes) = edit
        .get_mut("documentChanges")
        .and_then(Value::as_array_mut)
    {
        for change in changes {
            if change.get("kind").is_some() {
                continue;
            }
            let doc = &mut change["textDocument"];
            if !doc["version"].is_null() {
                let path = uri_path(doc["uri"].as_str().context("missing edit URI")?)?;
                let expected = wire
                    .get(&path)
                    .context("versioned edit targets an unopened document")?;
                ensure!(
                    doc["version"].as_i64() == Some(*expected as i64),
                    "stale K2 wire edit version"
                );
                doc["version"] =
                    serde_json::json!(opened.get(&path).context("missing UI snapshot")?.version);
            }
        }
    }
    Ok(())
}

fn workspace_ready_event(event: &Value) -> bool {
    event["method"] == "intellij/ready-for-test"
        || (event["method"] == "intellij/workspaceImportState"
            && event["params"]["phase"] == "FINISHED"
            && event["params"]["folders"]
                .as_array()
                .is_some_and(|folders| {
                    !folders.is_empty() && folders.iter().all(|f| f["status"] == "SUCCESS")
                }))
}

fn snapshot_failure(request: &Request, error: String) -> Event {
    match request {
        Request::Formatting { id, .. } => Event::Formatting {
            id: *id,
            result: Err(error),
        },
        Request::Project { id, .. } => Event::Project {
            id: *id,
            result: Err(error),
        },
        Request::Complete { path, version, .. } => Event::Completion {
            path: path.clone(),
            version: *version,
            items: vec![],
        },
        _ => Event::Status(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn wire_edit_versions_are_verified_then_translated_to_ui_snapshot() {
        let p = PathBuf::from("/tmp/Main.kt");
        let uri = crate::kotlin::file_uri(&p).unwrap();
        let wire = std::collections::BTreeMap::from([(p.clone(), 3)]);
        let opened = std::collections::BTreeMap::from([(
            p.clone(),
            crate::workspace_edit::Buffer {
                path: p,
                text: "val x=1".into(),
                version: 42,
            },
        )]);
        let mut good =
            json!({"documentChanges":[{"textDocument":{"uri":uri,"version":3},"edits":[]}]});
        normalize_edit_versions(&mut good, &wire, &opened).unwrap();
        assert_eq!(good["documentChanges"][0]["textDocument"]["version"], 42);
        let mut stale =
            json!({"documentChanges":[{"textDocument":{"uri":uri,"version":2},"edits":[]}]});
        assert!(normalize_edit_versions(&mut stale, &wire, &opened).is_err());
        let mut changes = json!({"changes":{uri:[]}});
        let original = changes.clone();
        normalize_edit_versions(&mut changes, &wire, &opened).unwrap();
        assert_eq!(changes, original);
    }
    #[test]
    fn semantic_import_barrier_requires_success_not_merely_initialize() {
        assert!(!workspace_ready_event(
            &json!({"result":{"capabilities":{}}})
        ));
        assert!(!workspace_ready_event(
            &json!({"method":"intellij/workspaceImportState","params":{"phase":"FINISHED","folders":[{"status":"FAILED"}]}})
        ));
        assert!(workspace_ready_event(
            &json!({"method":"intellij/workspaceImportState","params":{"phase":"FINISHED","folders":[{"status":"SUCCESS"}]}})
        ));
    }
    #[test]
    fn retiring_actor_timeout_aborts_only_owned_task_and_finishes_runtime() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        );
        let flag = Arc::new(AtomicBool::new(false));
        let watched = flag.clone();
        let (started, ready) = std::sync::mpsc::channel();
        let task = runtime.spawn(async move {
            let _drop = Dropped(watched);
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let (document, _snapshots) = snapshot_channel();
        let (requests, _commands) = mpsc::channel(1);
        let (_tx, events) = mpsc::channel(1);
        let connection = Connection {
            document,
            requests,
            events,
            task,
        };
        retire_with_timeout(connection, runtime, Duration::from_millis(50));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !flag.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        while RETIRING.load(Ordering::Acquire) != 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
