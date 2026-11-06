//! Native watcher + candidate K2: keep the old session until actual import/diagnostics.
use super::*;
use crate::{
    model_watch::{Gate, Ticket, Watcher},
    project::ProjectGraph,
    workspace_edit::Buffer,
};
use std::{collections::BTreeMap, path::Path, time::Instant};
struct State {
    session: KotlinSession,
    current: Option<Snapshot>,
    opened: BTreeMap<PathBuf, Buffer>,
    wire: BTreeMap<PathBuf, i32>,
    graph: ProjectGraph,
    imported: bool,
    needs_diagnostics: bool,
    pull: bool,
}
impl State {
    fn owns(&self, path: &Path) -> bool {
        self.graph.modules.iter().any(|m| {
            m.source_roots
                .iter()
                .chain(&m.generated_source_roots)
                .any(|p| path.starts_with(p))
        })
    }
    fn current_owned(&self) -> bool {
        self.current.as_ref().is_some_and(|s| self.owns(&s.path))
    }
    async fn sync(&mut self, next: Option<Snapshot>) -> Result<()> {
        let mut desired = BTreeMap::new();
        if let Some(s) = &next {
            for b in &s.others {
                if self.owns(&b.path) {
                    desired.insert(b.path.clone(), b.clone());
                }
            }
            if self.owns(&s.path) {
                desired.insert(
                    s.path.clone(),
                    Buffer {
                        path: s.path.clone(),
                        text: s.text.clone(),
                        version: s.version,
                    },
                );
            }
        }
        for p in self.opened.keys().filter(|p| !desired.contains_key(*p)) {
            self.session.close(p).await?;
            self.wire.remove(p);
        }
        for (p, b) in &desired {
            match self.opened.get(p) {
                None => {
                    self.session.open(p, &b.text, b.version).await?;
                    self.wire.insert(p.clone(), b.version);
                }
                Some(old) if old.text != b.text => {
                    let v = self.wire[p]
                        .checked_add(1)
                        .context("K2 wire version exhausted")?;
                    self.session.change(p, &b.text, v).await?;
                    self.wire.insert(p.clone(), v);
                }
                _ => (),
            }
        }
        self.opened = desired;
        self.current = next;
        self.needs_diagnostics = true;
        Ok(())
    }
    async fn diagnostics(&self) -> Result<Option<Event>> {
        let Some(s) = &self.current else {
            return Ok(None);
        };
        if !self.owns(&s.path) {
            return Ok(Some(Event::OutsideModel {
                path: s.path.clone(),
                version: s.version,
            }));
        }
        ensure!(self.pull, "native K2 pull diagnostics required");
        let report = self.session.pull_diagnostics(&s.path).await?;
        self.diagnostic_report(report)
    }
    async fn diagnostics_guarded(
        &self,
        revisions: watch::Receiver<Option<crate::model_watch::Revision>>,
        applied: u64,
    ) -> Result<Option<Event>> {
        if revisions.borrow().as_ref().is_none_or(|r| r.id != applied) {
            return Ok(None);
        }
        let Some(s) = &self.current else {
            return Ok(None);
        };
        if !self.owns(&s.path) {
            return Ok(Some(Event::OutsideModel {
                path: s.path.clone(),
                version: s.version,
            }));
        }
        ensure!(self.pull, "native K2 pull diagnostics required");
        // Finish the frame before making its RESPONSE wait cancellable. Dropping
        // a generic request future during write_all could corrupt the old stream.
        let request = self.session.begin_pull_diagnostics(&s.path).await?;
        let Some(report) = diagnostics_while_current(request.wait(), revisions, applied).await
        else {
            return Ok(None);
        };
        self.diagnostic_report(report?)
    }
    fn diagnostic_report(&self, report: Value) -> Result<Option<Event>> {
        let s = self.current.as_ref().context("diagnostic source missing")?;
        if report["kind"] == "unchanged" {
            return Ok(None);
        }
        let items = report["items"]
            .as_array()
            .context("K2 diagnostics missing items")?;
        Ok(Some(Event::Diagnostics {
            path: s.path.clone(),
            version: s.version,
            items: serde_json::from_value(Value::Array(items.clone()))?,
        }))
    }
}
fn active_model_current(gate: &Gate, observed: Option<u64>, applied: u64) -> bool {
    gate.settled() && gate.revision == applied && observed == Some(applied)
}
async fn diagnostics_while_current<T>(
    rpc: impl std::future::Future<Output = T>,
    mut revisions: watch::Receiver<Option<crate::model_watch::Revision>>,
    applied: u64,
) -> Option<T> {
    let current = revisions
        .borrow_and_update()
        .as_ref()
        .is_some_and(|r| r.id == applied);
    if !current {
        return None;
    }
    tokio::select! {
        biased;
        _ = revisions.changed() => None,
        value = rpc => {
            revisions.borrow().as_ref().is_some_and(|r| r.id == applied).then_some(value)
        }
    }
}
pub(super) fn same(a: &Option<Snapshot>, b: &Option<Snapshot>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.path == b.path
                && a.text == b.text
                && a.version == b.version
                && a.others.len() == b.others.len()
                && a.others
                    .iter()
                    .zip(&b.others)
                    .all(|(x, y)| x.path == y.path && x.text == y.text && x.version == y.version)
        }
        _ => false,
    }
}
struct Candidate {
    state: State,
    cache_hit: bool,
    compose_enabled: bool,
    first: Option<Event>,
}
struct Job(tokio::task::JoinHandle<Result<Candidate>>);
impl Drop for Job {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn status(
    tx: &mpsc::Sender<Event>,
    ticket: Ticket,
    phase: ModelPhase,
    detail: String,
    retained: bool,
) {
    let _ = tx.try_send(Event::Model(ModelStatus {
        revision: ticket.revision,
        attempt: ticket.attempt,
        phase,
        detail,
        retained,
    }));
}
async fn phase(
    tx: &mpsc::Sender<Event>,
    ticket: Ticket,
    phase: ModelPhase,
    detail: String,
    retained: bool,
) {
    let _ = tx
        .send(Event::Model(ModelStatus {
            revision: ticket.revision,
            attempt: ticket.attempt,
            phase,
            detail,
            retained,
        }))
        .await;
}
async fn warm(
    root: PathBuf,
    server: PathBuf,
    options: SyncOptions,
    mut snapshots: SnapshotReceiver,
    tx: mpsc::Sender<Event>,
    ticket: Ticket,
    retained: bool,
) -> Result<Candidate> {
    let model = crate::analysis::prepare(
        &root,
        &options.variant,
        options.offline,
        options.force,
        &|event| {
            if let crate::run::RunEvent::Phase(note) = event {
                status(&tx, ticket, ModelPhase::Preparing, note, retained);
            }
        },
    )
    .await?;
    phase(
        &tx,
        ticket,
        ModelPhase::Warming,
        "Importing candidate K2; replaying editor buffers".into(),
        retained,
    )
    .await;
    let session = KotlinSession::start_isolated(&root, &model.graph, &server).await?;
    let note = match session.index_cache.status.as_str() {
        "hit" => "Reusing verified K2 disk index · checking current source",
        "miss" => "Preparing K2 disk index · importing current source",
        "busy" => "Active K2 index in use · importing independent candidate",
        _ => "Preparing independent K2 index · importing current source",
    };
    status(&tx, ticket, ModelPhase::Warming, note.into(), retained);
    let pull = !session.capabilities["capabilities"]["diagnosticProvider"].is_null();
    let mut state = State {
        session,
        current: None,
        opened: BTreeMap::new(),
        wire: BTreeMap::new(),
        graph: model.graph,
        imported: false,
        needs_diagnostics: true,
        pull,
    };
    let initial = snapshots.latest().await?;
    state.sync(initial).await?;
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            next=snapshots.changed()=>{next?;let next=snapshots.latest().await?;state.sync(next).await?;},
            event=state.session.client.events.recv()=>{let event=event.context("candidate K2 import stream closed")?;state.imported|=workspace_ready_event(&event);},
            _=tick.tick(),if state.imported=>{
                let first=state.diagnostics().await?;
                if (first.is_some()||state.current.is_none())&&snapshots.matches(&state.current) {
                    state.session.index_checkpoint();return Ok(Candidate{state,cache_hit:model.cache_hit,compose_enabled:model.model.compose.is_some(),first});
                }
                let latest=snapshots.latest().await?;state.sync(latest).await?;
            }
        }
    }
}
pub(super) fn start(
    runtime: &tokio::runtime::Runtime,
    root: PathBuf,
    server: PathBuf,
    options: SyncOptions,
) -> Connection {
    let (document, mut snapshots) = snapshot_channel();
    let (requests, mut commands) = mpsc::channel(16);
    let (tx, events) = mpsc::channel(128);
    let task=runtime.spawn(async move {
        let result:Result<()>=async {
            let mut watcher=Watcher::start(root.clone());let mut gate=Gate::default();let clock=Instant::now();
            let mut job=None::<Job>;let mut active=None::<State>;let mut applied=0;let mut desired_options=options.clone();let mut first=true;
            let mut tick=tokio::time::interval(Duration::from_millis(150));tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {tokio::select!{
                changed=watcher.changes.changed()=>{
                    changed?;let revision=watcher.changes.borrow_and_update().clone();
                    if let Some(revision)=revision && gate.observe(revision.id,clock.elapsed().as_millis() as u64) {
                        job.take();phase(&tx,Ticket{revision:revision.id,attempt:0},ModelPhase::Waiting,revision.error.unwrap_or_else(||"Model inputs changed · waiting for edits to settle".into()),active.is_some()).await;
                    }
                },
                changed=snapshots.changed()=>{
                    if changed.is_err(){break;}
                    let latest=match snapshots.latest().await {Ok(s)=>s,Err(e)=>{let _=tx.send(Event::Status(format!("K2 snapshot: {e:#}; editor and last-good K2 retained"))).await;continue;}};if let Some(state)=&mut active {state.sync(latest).await?;}
                },
                request=commands.recv()=>{
                    let Some(request)=request else {break;};
                    if matches!(request,Request::Stop){break;}
                    if let Request::Refresh{force,variant}=request {
                        let now=clock.elapsed().as_millis() as u64;
                        if let Some(variant)=variant && matches!(variant.as_str(),"debug"|"release") && desired_options.variant!=variant {
                            desired_options.variant=variant;gate.restart(force,now);job.take();
                        } else if force||gate.failed {gate.retry(force,now);job.take();}
                        continue;
                    }
                    let latest=match snapshots.latest().await {Ok(s)=>s,Err(e)=>{let _=tx.send(snapshot_failure(&request,format!("K2 snapshot: {e:#}; editor and last-good K2 retained"))).await;continue;}};
                    if let Some(state)=&mut active {
                        state.sync(latest).await?;
                        let fresh=watcher.changes.borrow().as_ref().is_some_and(|r|r.id==applied)&&gate.settled();
                        match dispatch(state,request,&root,&tx,fresh,applied,&watcher.changes).await {
                            Ok(Some(event))=>{let event=if let Event::Project{id,..}=&event && watcher.changes.borrow().as_ref().is_some_and(|r|r.id!=applied) {Event::Project{id:*id,result:Err("Model changed; request a fresh semantic preview".into())}} else if let Event::Formatting{id,..}=&event && watcher.changes.borrow().as_ref().is_some_and(|r|r.id!=applied) {Event::Formatting{id:*id,result:Err("Model changed; request a fresh formatting preview".into())}} else {event};let _=tx.send(event).await;},
                            Ok(None)=>(),Err(e)=>{let _=tx.send(Event::Status(format!("K2: {e:#}"))).await;}
                        }
                    } else {let _=tx.send(snapshot_failure(&request,"K2 waiting for a valid model".into())).await;}
                },
                event=async {active.as_mut().unwrap().session.client.events.recv().await},if active.is_some()=>{
                    let Some(event)=event else {
                        active.take();job.take();gate.restart(false,clock.elapsed().as_millis() as u64);
                        phase(&tx,gate.next_ticket(),ModelPhase::Waiting,"K2 exited; starting a fresh session".into(),false).await;continue;
                    };
                    let state=active.as_mut().unwrap();state.imported|=workspace_ready_event(&event);
                    if active_model_current(&gate,watcher.changes.borrow().as_ref().map(|r|r.id),applied) && event["method"]=="textDocument/publishDiagnostics" && let Some(s)=&state.current && state.current_owned() && event["params"]["uri"]==file_uri(&s.path)? {
                        let version=event["params"]["version"].as_i64().unwrap_or(state.wire[&s.path] as i64);
                        if version==state.wire[&s.path] as i64 {let items=serde_json::from_value(event["params"]["diagnostics"].clone())?;let _=tx.send(Event::Diagnostics{path:s.path.clone(),version:s.version,items}).await;}
                    }
                },
                _=tick.tick()=>{
                    // Snapshot fence also catches changes queued while an RPC was pending.
                    let revision=watcher.changes.borrow().clone();
                    if let Some(revision)=revision && revision.id!=gate.revision {
                        gate.observe(revision.id,clock.elapsed().as_millis() as u64);job.take();phase(&tx,Ticket{revision:revision.id,attempt:0},ModelPhase::Waiting,"Model inputs changed; previous K2 retained".into(),active.is_some()).await;
                    }
                    if job.as_ref().is_some_and(|j|j.0.is_finished()) {
                        let mut completed=job.take().unwrap();let ticket=gate.inflight.context("candidate ticket missing")?;
                        let result=(&mut completed.0).await;drop(completed);
                        match result {
                            Ok(Ok(mut candidate)) if snapshots.matches(&candidate.state.current) && watcher.changes.borrow().as_ref().is_some_and(|r|r.id==ticket.revision)=>{
                                if gate.finish(ticket,true) {
                                    candidate.state.needs_diagnostics=false;applied=ticket.revision;
                                    if let Some(mut previous)=active.replace(candidate.state) {tokio::spawn(async move {let _=previous.session.shutdown().await;});}
                                    let state=active.as_ref().unwrap();
                                    let _=tx.send(Event::Model(ModelStatus{revision:ticket.revision,attempt:ticket.attempt,phase:ModelPhase::Applied,detail:"Model imported · editor buffers preserved".into(),retained:false})).await;
                                    let _=tx.send(Event::Ready{variant:state.graph.selected_variant.clone(),compose_enabled:Some(candidate.compose_enabled),cache_hit:candidate.cache_hit,modules:state.graph.modules.len(),server_info:state.session.capabilities["serverInfo"].clone()}).await;
                                    if let Some(first)=candidate.first {let _=tx.send(first).await;}
                                }
                            },
                            Ok(Ok(_))=>{gate.restart(false,clock.elapsed().as_millis() as u64);},
                            error=>{if gate.finish(ticket,false) {phase(&tx,ticket,ModelPhase::Failed,match error {Ok(Err(e))=>format!("{e:#}"),Err(e)=>format!("candidate task: {e}"),_=>unreachable!()},active.is_some()).await;}}
                        }
                    }
                    if job.is_none() && let Some(ticket)=gate.begin(clock.elapsed().as_millis() as u64) {
                        let input_error=watcher.changes.borrow().as_ref().and_then(|r|r.error.clone());
                        if let Some(error)=input_error {
                            gate.finish(ticket,false);phase(&tx,ticket,ModelPhase::Failed,error,active.is_some()).await;continue;
                        }
                        let r=root.clone();let s=server.clone();let mut opts=desired_options.clone();opts.force=gate.force||(first&&options.force);first=false;let doc=snapshots.clone();let sender=tx.clone();let retained=active.is_some();
                        phase(&tx,ticket,ModelPhase::Preparing,"Preparing Android model; previous K2 stays available".into(),retained).await;
                        job=Some(Job(tokio::spawn(async move {tokio::time::timeout(Duration::from_secs(300),warm(r,s,opts,doc,sender,ticket,retained)).await.context("candidate model/import timed out")?})));
                    }
                    if let Some(state)=&mut active && state.needs_diagnostics && state.imported && active_model_current(&gate,watcher.changes.borrow().as_ref().map(|r|r.id),applied) {
                        let result=state.diagnostics_guarded(watcher.changes.clone(),applied).await;
                        // A watcher revision can arrive while the RPC is awaited.
                        // Never publish obsolete resource/model errors, and don't
                        // let new old-model pulls occupy the candidate adoption loop.
                        if active_model_current(&gate,watcher.changes.borrow().as_ref().map(|r|r.id),applied) {
                            match result {Ok(Some(e))=>{let _=tx.send(e).await;},Ok(None)=>(),Err(e)=>{let _=tx.send(Event::Status(format!("K2 diagnostics: {e:#}"))).await;}}
                        }
                        state.needs_diagnostics=false;
                    }
                }
            }}
            drop(job);if let Some(mut state)=active {state.session.shutdown().await?;}Ok(())
        }.await;
        if let Err(e)=result {let _=tx.send(Event::Status(format!("K2 connection: {e:#}"))).await;}
    });
    Connection {
        document,
        requests,
        events,
        task,
    }
}
#[allow(clippy::too_many_arguments)]
async fn dispatch(
    state: &mut State,
    request: Request,
    root: &Path,
    tx: &mpsc::Sender<Event>,
    fresh: bool,
    applied: u64,
    revisions: &watch::Receiver<Option<crate::model_watch::Revision>>,
) -> Result<Option<Event>> {
    let Some(buffer) = state.current.clone() else {
        return Ok(Some(snapshot_failure(
            &request,
            "No current source snapshot".into(),
        )));
    };
    if matches!(&request, Request::Formatting { .. }) && !state.owns(&buffer.path) {
        return Ok(Some(snapshot_failure(
            &request,
            "Source is outside current model".into(),
        )));
    }
    ensure!(
        state.owns(&buffer.path),
        "Current file is outside the analysis model"
    );
    match request {
        Request::Formatting {
            id,
            path,
            version,
            range,
            cancel,
        } => {
            let result = if !fresh {
                Err("Model is updating/stale; retry formatting after refresh".into())
            } else if path != buffer.path || version != buffer.version {
                Err("Source snapshot changed; preview again".into())
            } else {
                format_request(&state.session, root, &buffer, range, cancel)
                    .await
                    .map(Box::new)
                    .map_err(|e| format!("{e:#}").chars().take(4096).collect())
            };
            let result = if revisions.borrow().as_ref().is_none_or(|r| r.id != applied) {
                Err("Model changed during formatting; request again".into())
            } else {
                result
            };
            Ok(Some(Event::Formatting { id, result }))
        }
        Request::Complete {
            line,
            character,
            path,
            version,
        } => {
            if path != buffer.path || version != buffer.version {
                return Ok(Some(Event::Completion {
                    path,
                    version,
                    items: vec![],
                }));
            }
            Ok(Some(Event::Completion {
                path,
                version,
                items: state
                    .session
                    .completion(&buffer.path, line, character)
                    .await?,
            }))
        }
        Request::Definition {
            navigation,
            line,
            character,
            path,
            version,
        } => {
            if path != buffer.path || version != buffer.version {
                return Ok(None);
            }
            Ok(Some(Event::Definition {
                navigation,
                path,
                version,
                locations: state
                    .session
                    .definition(&buffer.path, line, character)
                    .await?,
            }))
        }
        Request::Project {
            id,
            operation,
            path,
            version,
            line,
            character,
            buffers,
        } => {
            if !fresh {
                return Ok(Some(Event::Project {
                    id,
                    result: Err(
                        "Model is updating/stale; retry semantic preview after refresh".into(),
                    ),
                }));
            }
            if path != buffer.path || version != buffer.version {
                return Ok(Some(Event::Project {
                    id,
                    result: Err("Source snapshot changed; request again".into()),
                }));
            }
            let result = project_request(
                &state.session,
                root,
                operation,
                &path,
                line,
                character,
                buffers,
                &state.wire,
                &state.opened,
            )
            .await
            .map_err(|e| format!("{e:#}"));
            Ok(Some(Event::Project { id, result }))
        }
        Request::ApplyCompletion {
            path,
            version,
            item,
        } => {
            ensure!(
                fresh,
                "Model is updating/stale; retry import completion after refresh"
            );
            if path != buffer.path || version != buffer.version {
                return Ok(None);
            }
            let command = item.extra.get("command").cloned().unwrap_or(Value::Null);
            let request = state.session.client.request(
                "workspace/executeCommand",
                serde_json::json!({"command":command["command"],"arguments":command["arguments"]}),
            );
            tokio::pin!(request);
            loop {
                tokio::select! {reply=&mut request=>{reply?;break;},incoming=state.session.client.next_server_request()=>{
                    let incoming=incoming.context("K2 edit stream closed")?;
                if revisions.borrow().as_ref().is_some_and(|r|r.id!=applied) {let _=incoming.reply.send(serde_json::json!({"applied":false}));anyhow::bail!("Model changed during import completion");}let (reply,wait)=tokio::sync::oneshot::channel();let mut edit=incoming.params["edit"].clone();normalize_edit_versions(&mut edit,&state.wire,&state.opened)?;
                    let _=tx.send(Event::WorkspaceEdit{path:buffer.path.clone(),version:buffer.version,edit,reply}).await;
                    let applied=wait.await.unwrap_or(false);let _=incoming.reply.send(serde_json::json!({"applied":applied}));
                }}
            }
            Ok(None)
        }
        Request::Saved => {
            state.session.saved(&buffer.path).await?;
            Ok(None)
        }
        Request::Stop | Request::Refresh { .. } => Ok(None),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn applied_gate() -> Gate {
        let mut g = Gate::default();
        assert!(g.observe(1, 0));
        let t = g.begin(650).unwrap();
        assert!(g.finish(t, true));
        g
    }
    #[test]
    fn retained_model_diagnostics_waiting_and_warming_never_block_candidate() {
        let mut g = applied_gate();
        assert!(active_model_current(&g, Some(1), 1));
        g.observe(2, 700);
        assert!(
            !active_model_current(&g, Some(2), 1),
            "obsolete diagnostic work must not occupy the candidate-adoption loop during debounce"
        );
        let t = g.begin(1350).unwrap();
        assert!(
            !active_model_current(&g, Some(2), 1),
            "retained old K2 must not schedule pull or publish push diagnostics while a new model warms"
        );
        assert!(g.finish(t, true));
        assert!(!active_model_current(&g, Some(2), 1));
        assert!(active_model_current(&g, Some(2), 2));
    }
    #[test]
    fn retained_model_diagnostics_reply_rechecks_unprocessed_watcher_revision() {
        let g = applied_gate();
        assert!(active_model_current(&g, Some(1), 1));
        // The independent watcher can advance while an actual diagnostic RPC is
        // awaited, before the event loop has observed/updated its Gate.
        assert!(!active_model_current(&g, Some(2), 1));
        assert!(!active_model_current(&g, None, 1));
    }
    #[test]
    fn retained_model_diagnostics_force_retry_failure_keep_last_good_not_stale_errors() {
        let mut g = applied_gate();
        g.restart(true, 700);
        assert!(!active_model_current(&g, Some(1), 1));
        let t = g.begin(700).unwrap();
        assert!(!active_model_current(&g, Some(1), 1));
        assert!(g.finish(t, false));
        assert!(!active_model_current(&g, Some(1), 1));
        g.retry(false, 800);
        let t = g.begin(800).unwrap();
        assert!(g.finish(t, true));
        assert!(active_model_current(&g, Some(1), 1));
    }
    fn revision(id: u64) -> Option<crate::model_watch::Revision> {
        Some(crate::model_watch::Revision {
            id,
            fingerprint: format!("model-{id}"),
            error: None,
        })
    }
    #[tokio::test]
    async fn retained_model_diagnostics_cancel_inflight_pull_on_real_watch_change() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let (tx, rx) = watch::channel(revision(1));
        let (started, ready) = tokio::sync::oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = dropped.clone();
        let task = tokio::spawn(diagnostics_while_current(
            async move {
                let _drop = Dropped(flag);
                let _ = started.send(());
                std::future::pending::<&'static str>().await
            },
            rx,
            1,
        ));
        ready.await.unwrap();
        tx.send_replace(revision(2));
        let result = tokio::time::timeout(Duration::from_secs(1), task).await;
        assert!(
            result.is_ok(),
            "an obsolete in-flight diagnostic must release the coordinator when the independent model watcher changes"
        );
        assert_eq!(
            result.unwrap().unwrap(),
            None,
            "cancel is not a fabricated empty/success diagnostic report"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "the abandoned RPC future/RAII request guard must be dropped"
        );
    }
    #[tokio::test]
    async fn retained_model_diagnostics_keep_real_current_result_skip_already_stale_rpc() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (tx, rx) = watch::channel(revision(1));
        assert_eq!(
            diagnostics_while_current(async { "actual report" }, rx.clone(), 1).await,
            Some("actual report")
        );
        tx.send_replace(revision(2));
        let polled = AtomicBool::new(false);
        let result = diagnostics_while_current(
            async {
                polled.store(true, Ordering::SeqCst);
                "obsolete report"
            },
            rx,
            1,
        )
        .await;
        assert_eq!(result, None);
        assert!(!polled.load(Ordering::SeqCst));
    }
    #[tokio::test]
    #[ignore = "Requires an explicit disposable Android project, SDK and genuine pinned K2"]
    async fn genuine_k2_staged_cancel_and_followup_survive() {
        let root = PathBuf::from(
            std::env::var_os("WEX_K2_CANCEL_PROJECT").expect("explicit fixture required"),
        );
        assert!(root.join("CANCEL_FIXTURE").is_file());
        let output = PathBuf::from(std::env::var_os("WEX_K2_CANCEL_OUTPUT").unwrap());
        let server = PathBuf::from(std::env::var_os("WEX_KOTLIN_LSP").unwrap());
        let path = root.join("app/src/main/kotlin/dev/wex/android/MainActivity.kt");
        let original = std::fs::read_to_string(&path).unwrap();
        let model = crate::analysis::prepare(&root, "debug", true, false, &|_| {})
            .await
            .unwrap();
        let mut session = KotlinSession::start_isolated(&root, &model.graph, &server)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                let e = session
                    .client
                    .events
                    .recv()
                    .await
                    .expect("actual K2 import stream");
                if workspace_ready_event(&e) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        session.open(&path, &original, 1).await.unwrap();
        let first = session.pull_diagnostics(&path).await.unwrap();
        assert!(
            !first["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["severity"] == 1)
        );
        let mut rounds = vec![];
        let cancel_enabled = std::env::var_os("WEX_K2_CANCEL_CONTROL").is_none();
        let mut version = 1;
        for n in 0..3 {
            let mut dirty = original.clone();
            dirty.push_str(&format!(
                "\nprivate fun cancellationProbe{n}(): Int = \"genuine type error\"\n"
            ));
            if std::env::var_os("WEX_K2_CANCEL_DECLARATIONS").is_some() {
                for k in 0..2000 {
                    dirty.push_str(&format!("private val cancelWarm{n}_{k}: Int = {k}\n"));
                }
            } else {
                dirty.push_str(&format!("private fun cancelWarm{n}() = intArrayOf("));
                for k in 0..2000 {
                    dirty.push_str(&format!("{k},"));
                }
                dirty.push_str(")\n");
            }
            version += 1;
            session.change(&path, &dirty, version).await.unwrap();
            if cancel_enabled {
                let pending = session.begin_pull_diagnostics(&path).await.unwrap();
                drop(pending);
            }
            // The following real request is NOT cancelled by the previous id.
            let report = session.pull_diagnostics(&path).await;
            if report.is_err() {
                let base = PathBuf::from(std::env::var_os("WEX_K2_SYSTEM_DIR").unwrap());
                for dir in std::fs::read_dir(base).unwrap().flatten() {
                    let log = dir.path().join("system/log/intellij-server.log");
                    if log.is_file() {
                        let _ = std::fs::copy(log, output.with_extension("backend.log"));
                    }
                }
            }
            let report = report.unwrap();
            assert!(
                report["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["severity"] == 1
                        && d["message"]
                            .as_str()
                            .is_some_and(|m| m.contains("Int") && m.contains("String"))),
                "genuine dirty diagnostic missing: {report}"
            );
            version += 1;
            session.change(&path, &original, version).await.unwrap();
            let restored = session.pull_diagnostics(&path).await.unwrap();
            assert!(
                !restored["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["severity"] == 1)
            );
            rounds.push(json!({"round":n,"dirty":report,"restored":restored}));
        }
        let line = original
            .lines()
            .position(|l| l.contains("val greeter = Greeter()"))
            .unwrap();
        let col = original.lines().nth(line).unwrap().find("Greeter").unwrap() + 2;
        let locations = session
            .definition(&path, line as u32, col as u32)
            .await
            .unwrap();
        assert!(
            locations.as_array().unwrap().iter().any(|l| l["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with("/Greeter.kt"))
                || l["targetUri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("/Greeter.kt"))),
            "real definition missing: {locations:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(
            !root.join(".wex/native").exists(),
            "analysis probe never builds an APK"
        );
        let proof = json!({"result":if cancel_enabled {"GENUINE_K2_CANCEL_OK"} else {"GENUINE_K2_CANCEL_CONTROL_OK"},"cancel_enabled":cancel_enabled,"declaration_stress":std::env::var_os("WEX_K2_CANCEL_DECLARATIONS").is_some(),"server":session.capabilities["serverInfo"],"rounds":rounds,"definition":locations,"source_disk_unchanged":true,"no_apk_or_device":true});
        session.close(&path).await.unwrap();
        session.shutdown().await.unwrap();
        std::fs::write(output, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
        println!(
            "GENUINE_K2_CANCEL_OK: three staged cancellation notifications to the real server; fresh typed-error/restore reports and shared-source F12 remain valid; disk unchanged; no APK/device"
        );
    }
    #[test]
    fn warm_fence_includes_dirty_parked_buffers_not_only_visible_version() {
        let a = Some(Snapshot {
            path: "/src/A.kt".into(),
            text: "a".into(),
            version: 5,
            others: vec![Buffer {
                path: "/src/B.kt".into(),
                text: "dirty".into(),
                version: 2,
            }],
        });
        let mut b = a.clone();
        assert!(same(&a, &b));
        b.as_mut().unwrap().others[0].text = "new unsaved".into();
        assert!(!same(&a, &b));
        b = a.clone();
        b.as_mut().unwrap().version += 1;
        assert!(!same(&a, &b));
    }
    #[tokio::test]
    #[ignore = "Requires an explicit disposable Android project, SDK and genuine pinned K2"]
    async fn genuine_k2_profiles_reuse_and_refresh_sources() {
        let root = PathBuf::from(std::env::var_os("WEX_K2_INDEX_PROJECT").unwrap());
        assert!(root.join("INDEX_FIXTURE").is_file());
        let output = PathBuf::from(std::env::var_os("WEX_K2_INDEX_OUTPUT").unwrap());
        let server = PathBuf::from(std::env::var_os("WEX_KOTLIN_LSP").unwrap());
        let path = root.join("app/src/main/kotlin/dev/wex/android/MainActivity.kt");
        let original = std::fs::read_to_string(&path).unwrap();
        let shared = root.join("shared/src/main/kotlin/dev/wex/shared/Greeter.kt");
        let shared_original = std::fs::read_to_string(&shared).unwrap();
        let shared_meta = std::fs::metadata(&shared).unwrap();
        let added = root.join("shared/src/main/kotlin/dev/wex/shared/IndexEpoch.kt");
        assert!(!added.exists());
        let model = crate::analysis::prepare(&root, "debug", true, false, &|_| {})
            .await
            .unwrap();
        let mut rounds = vec![];
        let mut first_workspace = None;
        let mut damaged = None;
        for epoch in 0..3 {
            let start = std::time::Instant::now();
            let mut session = KotlinSession::start_isolated(&root, &model.graph, &server)
                .await
                .unwrap();
            assert_eq!(
                session.index_cache.status,
                if epoch == 1 { "hit" } else { "miss" },
                "actual profile: {:?}",
                session.index_cache
            );
            if epoch == 1 {
                assert_eq!(Some(&session.workspace), first_workspace.as_ref());
                assert!(session.index_cache.reused_files > 0);
            }
            if epoch == 2 {
                assert_ne!(Some(&session.workspace), first_workspace.as_ref());
            }
            tokio::time::timeout(Duration::from_secs(180), async {
                loop {
                    let event = session.client.events.recv().await.unwrap();
                    if workspace_ready_event(&event) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            session.open(&path, &original, 1).await.unwrap();
            let initial = session.pull_diagnostics(&path).await.unwrap();
            if epoch == 1 {
                assert!(
                    initial["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d["severity"] == 1
                            && d["message"].as_str().is_some_and(|m| m.contains("Greeter"))),
                    "same-size+mtime closed source was stale: {initial}"
                );
            } else {
                assert!(
                    !initial["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d["severity"] == 1),
                    "initial epoch {epoch}: {initial}"
                );
            }
            let working = if epoch == 1 {
                original
                    .replace("Greeter", "TreeAlt")
                    .replace("greeter.greeting(title)", "greeter.renamed1(title)")
            } else {
                original.clone()
            };
            let mut version = 1;
            if epoch == 1 {
                assert_ne!(working, original);
                version += 1;
                session.change(&path, &working, version).await.unwrap();
                let fresh = session.pull_diagnostics(&path).await.unwrap();
                assert!(
                    !fresh["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d["severity"] == 1),
                    "closed existing source not refreshed: {fresh}"
                );
            }
            let class = if epoch == 1 { "TreeAlt" } else { "Greeter" };
            let line = working
                .lines()
                .position(|s| s.contains(&format!("val greeter = {class}()")))
                .unwrap();
            let col = working.lines().nth(line).unwrap().find(class).unwrap() + 2;
            let definition = session
                .definition(&path, line as u32, col as u32)
                .await
                .unwrap();
            assert!(
                definition.to_string().contains("Greeter.kt"),
                "actual class F12: {definition}"
            );
            if epoch == 0 {
                first_workspace = Some(session.workspace.clone());
                let dirty = format!(
                    "{original}\nclass UnsavedOnly\nprivate val indexProof: Int = \"real error\"\n"
                );
                version += 1;
                session.change(&path, &dirty, version).await.unwrap();
                let bad = session.pull_diagnostics(&path).await.unwrap();
                assert!(
                    bad["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d["severity"] == 1)
                );
            } else if epoch == 1 {
                let fresh = format!(
                    "{working}\nprivate fun epochProof()=dev.wex.shared.IndexEpoch().message()\n"
                );
                version += 1;
                session.change(&path, &fresh, version).await.unwrap();
                let good = session.pull_diagnostics(&path).await.unwrap();
                assert!(
                    !good["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d["severity"] == 1),
                    "new closed source not found: {good}"
                );
                let line = fresh
                    .lines()
                    .position(|s| s.contains("private fun epochProof"))
                    .unwrap();
                let col = fresh.lines().nth(line).unwrap().find("IndexEpoch").unwrap() + 2;
                let location = session
                    .definition(&path, line as u32, col as u32)
                    .await
                    .unwrap();
                assert!(
                    location.to_string().contains("IndexEpoch.kt"),
                    "new source F12: {location}"
                );
                let absent = format!("{fresh}\nprivate fun staleUnsavedProof()=UnsavedOnly()\n");
                version += 1;
                session.change(&path, &absent, version).await.unwrap();
                let bad = session.pull_diagnostics(&path).await.unwrap();
                assert!(
                    bad["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d["severity"] == 1
                            && d["message"]
                                .as_str()
                                .is_some_and(|m| m.contains("UnsavedOnly"))),
                    "old unsaved declaration leaked: {bad}"
                );
                version += 1;
                session.change(&path, &working, version).await.unwrap();
                let restored = session.pull_diagnostics(&path).await.unwrap();
                assert!(
                    !restored["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|d| d["severity"] == 1)
                );
            }
            session.index_checkpoint();
            let record = serde_json::to_value(&session.index_cache).unwrap();
            let profile = session.index_cache.system.clone();
            session.shutdown().await.unwrap();
            let pointer = profile
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("current.json");
            assert!(
                pointer.is_file(),
                "genuine stopped profile did not seal: {:?}",
                session.index_cache
            );
            let receipt: Value = serde_json::from_slice(&std::fs::read(pointer).unwrap()).unwrap();
            assert_eq!(receipt["key"], record["key"]);
            rounds.push(json!({"epoch":epoch,"profile":record,"ready_and_clean_exit":true,"elapsed_ms":start.elapsed().as_millis(),"initial_diagnostics":initial,"definition":definition}));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            if epoch == 0 {
                let renamed = shared_original
                    .replace("class Greeter", "class TreeAlt")
                    .replace("fun greeting(", "fun renamed1(");
                assert_ne!(renamed, shared_original);
                assert_eq!(renamed.len(), shared_original.len());
                std::fs::write(&shared, renamed).unwrap();
                std::fs::File::open(&shared)
                    .unwrap()
                    .set_times(
                        std::fs::FileTimes::new()
                            .set_modified(shared_meta.modified().unwrap())
                            .set_accessed(shared_meta.accessed().unwrap()),
                    )
                    .unwrap();
                assert_eq!(
                    std::fs::metadata(&shared).unwrap().modified().unwrap(),
                    shared_meta.modified().unwrap()
                );
                assert_eq!(std::fs::metadata(&shared).unwrap().len(), shared_meta.len());
                std::fs::write(&added,"package dev.wex.shared\nclass IndexEpoch {fun message():String=\"fresh disk\"}\n").unwrap();
            }
            if epoch == 1 {
                std::fs::write(&shared, &shared_original).unwrap();
                std::fs::File::open(&shared)
                    .unwrap()
                    .set_times(
                        std::fs::FileTimes::new()
                            .set_modified(shared_meta.modified().unwrap())
                            .set_accessed(shared_meta.accessed().unwrap()),
                    )
                    .unwrap();
                std::fs::remove_file(&added).unwrap();
                let sst = walkdir::WalkDir::new(&profile)
                    .into_iter()
                    .map(Result::unwrap)
                    .find(|e| {
                        e.file_type().is_file() && e.path().extension().is_some_and(|v| v == "sst")
                    })
                    .unwrap()
                    .into_path();
                std::fs::write(&sst, b"damaged closed genuine index").unwrap();
                damaged = Some(sst);
            }
        }
        assert_eq!(rounds[0]["profile"]["key"], rounds[1]["profile"]["key"]);
        assert_eq!(rounds[1]["profile"]["key"], rounds[2]["profile"]["key"]);
        assert_eq!(
            std::fs::read(damaged.unwrap()).unwrap(),
            b"damaged closed genuine index"
        );
        assert_eq!(std::fs::read_to_string(&shared).unwrap(), shared_original);
        assert!(!added.exists());
        assert!(!root.join(".wex/native").exists());
        std::fs::write(output,serde_json::to_vec_pretty(&json!({"result":"GENUINE_K2_INDEX_OK","rounds":rounds,"same_workspace_on_hit":true,"same_context_key_all_rounds":true,"dirty_previous_session_did_not_leak":true,"fresh_closed_source_definition":true,"existing_closed_source_same_size_mtime_refreshed":true,"damaged_profile_quarantined":true,"final_source_hashes_restored":true,"main_disk_never_written":true,"no_apk_or_device":true})).unwrap()).unwrap();
        println!(
            "GENUINE_K2_INDEX_OK: actual cold/warm/tamper-recovery; same-size+mtime closed class/member refreshed without changing SDK key; new closed-source F12; old unsaved declaration rejected; final source bytes restored; no APK/device"
        );
    }
}
