//! Asynchronous JSON-RPC/LSP framing and multiplexing over a child process.
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};

fn trace(direction: &str, value: &Value) {
    if let Some(path) = std::env::var_os("WEX_LSP_TRACE") {
        static TRACE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{direction} {value}");
        }
    }
}
const MAX_BODY: usize = 16 * 1024 * 1024;
type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
struct PendingRequest {
    id: u64,
    pending: Pending,
    cancel: Option<Writer>,
}
impl Drop for PendingRequest {
    fn drop(&mut self) {
        let pending = self.pending.lock().unwrap().remove(&self.id).is_some();
        if pending
            && let Some(writer) = &self.cancel
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let writer = writer.clone();
            let id = self.id;
            // If the stream is idle, reserve its next whole frame synchronously.
            // No blocking write, runtime creation or waiting occurs in Drop.
            let reserved = writer.clone().try_lock_owned().ok();
            runtime.spawn(async move {
                let value = json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}});
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    if let Some(mut output) = reserved {
                        output.send(&value).await
                    } else {
                        write_rpc(&writer, &value).await
                    }
                })
                .await;
            });
        }
    }
}
/// A fully written request frame; cancelling only the response wait cannot leave
/// half a JSON-RPC frame in the retained session's output stream.
pub(crate) struct PendingResponse {
    reply: oneshot::Receiver<Result<Value>>,
    cleanup: PendingRequest,
    deadline: tokio::time::Instant,
}
impl PendingResponse {
    pub(crate) async fn wait(self) -> Result<Value> {
        let Self {
            reply,
            cleanup,
            deadline,
        } = self;
        let result = tokio::time::timeout_at(deadline, reply)
            .await
            .context("LSP request timed out")?
            .context("LSP response channel closed")?;
        drop(cleanup);
        result
    }
}
struct FrameWriter {
    stream: Box<dyn AsyncWrite + Send + Unpin>,
    incomplete: bool,
    closed: bool,
}
impl FrameWriter {
    async fn send(&mut self, value: &Value) -> Result<()> {
        ensure!(!self.closed, "LSP output is closed");
        ensure!(
            !self.incomplete,
            "LSP output has an interrupted frame; reconnect required"
        );
        let body = encode_body(value)?;
        trace("send", value);
        // Cancellation/error after a partial header or body poisons the stream
        // BEFORE its mutex is released. Never append another frame to those bytes.
        self.incomplete = true;
        let result = write_body(&mut *self.stream, &body).await;
        self.incomplete = result.is_err();
        result
    }
}
type Writer = Arc<Mutex<FrameWriter>>;
async fn write_rpc(writer: &Writer, value: &Value) -> Result<()> {
    writer.lock().await.send(value).await
}
pub async fn read_message<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Option<Value>> {
    let mut total = 0;
    let mut length = None;
    loop {
        let mut line = Vec::new();
        loop {
            let buf = r.fill_buf().await?;
            if buf.is_empty() {
                ensure!(total == 0 && line.is_empty(), "truncated LSP header");
                return Ok(None);
            }
            let n = buf
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |p| p + 1);
            ensure!(total + line.len() + n <= 8192, "LSP header too large");
            line.extend_from_slice(&buf[..n]);
            r.consume(n);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        total += line.len();
        let s = std::str::from_utf8(&line)?.trim_end_matches(['\n', '\r']);
        if s.is_empty() {
            break;
        }
        let (name, value) = s.split_once(':').context("invalid LSP header")?;
        if name.eq_ignore_ascii_case("Content-Length") {
            ensure!(length.is_none(), "duplicate Content-Length");
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length.context("missing Content-Length")?;
    ensure!(length <= MAX_BODY, "LSP message too large");
    let mut body = vec![0; length];
    r.read_exact(&mut body).await?;
    let value = serde_json::from_slice(&body)?;
    trace("recv", &value);
    Ok(Some(value))
}
pub async fn write_message<W: AsyncWrite + Unpin + ?Sized>(w: &mut W, value: &Value) -> Result<()> {
    trace("send", value);
    let body = encode_body(value)?;
    write_body(w, &body).await
}
fn encode_body(value: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value)?;
    ensure!(body.len() <= MAX_BODY, "LSP message too large");
    Ok(body)
}
async fn write_body<W: AsyncWrite + Unpin + ?Sized>(w: &mut W, body: &[u8]) -> Result<()> {
    w.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

pub struct ServerRequest {
    pub method: String,
    pub params: Value,
    pub reply: oneshot::Sender<Value>,
}
pub struct LspClient {
    server_requests: Mutex<mpsc::Receiver<ServerRequest>>,
    request_timeout: Duration,
    writer: Writer,
    pending: Pending,
    next: AtomicU64,
    reader: JoinHandle<()>,
    child: Option<Child>,
    pub events: mpsc::Receiver<Value>,
    pub dropped_events: Arc<AtomicU64>,
}
impl LspClient {
    pub fn spawn(program: &Path, args: &[String], cwd: &Path) -> Result<Self> {
        Self::spawn_filtered(program, args, cwd, &[])
    }
    pub(crate) fn spawn_filtered(
        program: &Path,
        args: &[String],
        cwd: &Path,
        remove: &[String],
    ) -> Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        for name in remove {
            command.env_remove(name);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("starting LSP {}", program.display()))?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let mut client = Self::from_io(BufReader::new(output), input);
        client.child = Some(child);
        Ok(client)
    }
    fn from_io<R, W>(mut read: R, write: W) -> Self
    where
        R: AsyncBufRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let writer: Writer = Arc::new(Mutex::new(FrameWriter {
            stream: Box::new(write),
            incomplete: false,
            closed: false,
        }));
        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (sender, events) = mpsc::channel(64);
        let (requests_tx, requests_rx) = mpsc::channel::<ServerRequest>(32);
        let dropped_events = Arc::new(AtomicU64::new(0));
        let (p, w, d) = (pending.clone(), writer.clone(), dropped_events.clone());
        let reader = tokio::spawn(async move {
            let result:Result<()>=async {
                while let Some(value)=read_message(&mut read).await? {
                    if value.get("method").is_some() {
                        if let Some(id)=value.get("id") {
                            let method=value["method"].as_str().unwrap_or("");
                            if method=="workspace/applyEdit" {
                                let (reply,wait)=oneshot::channel();
                                let request=ServerRequest{method:method.to_owned(),params:value["params"].clone(),reply};
                                if requests_tx.try_send(request).is_ok(){let id=id.clone();let writer=w.clone();tokio::spawn(async move{let result=tokio::time::timeout(Duration::from_secs(10),wait).await.ok().and_then(Result::ok).unwrap_or_else(||json!({"applied":false,"failureReason":"editor did not acknowledge the edit"}));let _=write_rpc(&writer,&json!({"jsonrpc":"2.0","id":id,"result":result})).await;});continue;}
                            }
                            let response=match method {
                                "workspace/configuration"=>json!({"jsonrpc":"2.0","id":id,"result":value["params"]["items"].as_array().map(|a|vec![Value::Null;a.len()]).unwrap_or_default()}),
                                "client/registerCapability"|"client/unregisterCapability"|"window/workDoneProgress/create"=>json!({"jsonrpc":"2.0","id":id,"result":null}),
                                _=>json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unsupported client method"}}),
                            };
                            write_rpc(&w,&response).await?;
                        } else if sender.try_send(value).is_err(){d.fetch_add(1,Ordering::Relaxed);}
                    } else if let Some(id)=value["id"].as_u64() && let Some(send)=p.lock().unwrap().remove(&id){let reply=if let Some(error)=value.get("error"){Err(anyhow!("LSP error: {error}"))}else{Ok(value["result"].clone())};let _=send.send(reply);}
                }Ok(())
            }.await;
            let reason = result
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "LSP stream closed".into());
            for (_, send) in p.lock().unwrap().drain() {
                let _ = send.send(Err(anyhow!(reason.clone())));
            }
        });
        Self {
            server_requests: Mutex::new(requests_rx),
            request_timeout: Duration::from_secs(15),
            writer,
            pending,
            next: AtomicU64::new(1),
            reader,
            child: None,
            events,
            dropped_events,
        }
    }
    pub async fn next_server_request(&self) -> Option<ServerRequest> {
        self.server_requests.lock().await.recv().await
    }
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }
    pub(crate) async fn begin_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<PendingResponse> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, reply) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut response = PendingResponse {
            reply,
            cleanup: PendingRequest {
                id,
                pending: self.pending.clone(),
                cancel: None,
            },
            deadline: tokio::time::Instant::now() + self.request_timeout,
        };
        // Callers await this complete send before selecting on cancellation.
        // The original whole-request deadline still includes the write time.
        tokio::time::timeout_at(response.deadline, async {
            write_rpc(
                &self.writer,
                &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            )
            .await
        })
        .await
        .context("LSP request timed out")??;
        response.cleanup.cancel = Some(self.writer.clone());
        Ok(response)
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _cleanup = PendingRequest {
            id,
            pending: self.pending.clone(),
            cancel: None,
        };
        let result = tokio::time::timeout(self.request_timeout, async {
            write_rpc(
                &self.writer,
                &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            )
            .await?;
            rx.await.context("LSP response channel closed")?
        })
        .await;
        self.pending.lock().unwrap().remove(&id);
        result.context("LSP request timed out")?
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            write_rpc(
                &self.writer,
                &json!({"jsonrpc":"2.0","method":method,"params":params}),
            ),
        )
        .await
        .context("LSP write timed out")?
    }
    pub async fn initialize(&self, root_uri: &str) -> Result<Value> {
        let result=self.request("initialize",json!({"processId":std::process::id(),"clientInfo":{"name":"WexAndroidStudio","version":env!("CARGO_PKG_VERSION")},"rootUri":root_uri,"workspaceFolders":[{"uri":root_uri,"name":"WexAndroidStudio"}],"capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"formatting":{"dynamicRegistration":false},"rangeFormatting":{"dynamicRegistration":false},"rename":{"prepareSupport":true,"honorsChangeAnnotations":true},"synchronization":{"didSave":true},"completion":{"completionItem":{"snippetSupport":false}},"publishDiagnostics":{}},"workspace":{"configuration":true,"applyEdit":true,"workspaceEdit":{"documentChanges":true,"failureHandling":"transactional"}}}})).await?;
        if let Some(encoding) = result["capabilities"]["positionEncoding"].as_str() {
            ensure!(
                encoding == "utf-16",
                "unsupported server position encoding {encoding}"
            );
        }
        self.notify("initialized", json!({})).await?;
        Ok(result)
    }
    pub async fn shutdown(&mut self) -> Result<()> {
        let response = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        // Exit is a notification, not proof that the process has stopped. Close
        // its input after the final complete frame so a stdio server can observe
        // EOF, finish its index flush and exit normally instead of being killed.
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            let mut writer = self.writer.lock().await;
            writer.closed = true;
            let mut stream = std::mem::replace(&mut writer.stream, Box::new(tokio::io::sink()));
            drop(writer);
            let _ = stream.shutdown().await;
            // Unix ChildStdin::poll_shutdown is a no-op; dropping the actual
            // owned pipe is what delivers EOF. Closed writers reject later RPCs.
            drop(stream);
        })
        .await;
        if let Some(child) = self.child.as_mut()
            && tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_err()
        {
            child.kill().await?;
            child.wait().await?;
        }
        response.map(|_| ())
    }
    pub(crate) fn child_exited_successfully(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .is_some_and(|status| status.success())
    }
}
impl Drop for LspClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_filters_only_child_environment_without_changing_parent() {
        let path = std::env::var_os("PATH");
        let home_present = std::env::var_os("HOME").is_some();
        let script = "import os,json,sys; b=json.dumps({'jsonrpc':'2.0','method':'environment-check','params':{'removed':'PATH' not in os.environ,'home_present':'HOME' in os.environ}}).encode(); sys.stdout.buffer.write(('Content-Length: %d\\r\\n\\r\\n'%len(b)).encode()+b); sys.stdout.buffer.flush()";
        let mut client = LspClient::spawn_filtered(
            Path::new("/usr/bin/python3"),
            &["-c".into(), script.into()],
            Path::new("/tmp"),
            &["PATH".into()],
        )
        .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), client.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event["method"], "environment-check");
        assert_eq!(event["params"]["removed"], true);
        assert_eq!(event["params"]["home_present"], home_present);
        assert_eq!(std::env::var_os("PATH"), path);
    }
    #[tokio::test]
    async fn unicode_content_length_is_bytes() {
        let (mut a, b) = tokio::io::duplex(1024);
        let value = json!({"x":"👋привет"});
        write_message(&mut a, &value).await.unwrap();
        drop(a);
        assert_eq!(
            read_message(&mut BufReader::new(b)).await.unwrap().unwrap(),
            value
        );
    }
    #[tokio::test]
    async fn rejects_huge_frame_before_allocating() {
        let mut r = BufReader::new(&b"Content-Length: 999999999\r\n\r\n"[..]);
        assert!(
            read_message(&mut r)
                .await
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
    }
    #[tokio::test]
    async fn duplicate_length_is_error() {
        let mut r = BufReader::new(&b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}"[..]);
        assert!(read_message(&mut r).await.is_err());
    }
    #[tokio::test]
    async fn disconnect_wakes_pending_request() {
        let (a, b) = tokio::io::duplex(4096);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        let server = tokio::spawn(async move {
            let mut r = BufReader::new(b);
            read_message(&mut r).await.unwrap();
        });
        let result = c.request("example", json!({})).await;
        assert!(result.is_err());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn routes_reordered_responses_and_notifications() {
        let (a, b) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(a);
        let mut c = LspClient::from_io(BufReader::new(r), w);
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(b);
            let mut r = BufReader::new(r);
            let one = read_message(&mut r).await.unwrap().unwrap();
            let two = read_message(&mut r).await.unwrap().unwrap();
            write_message(&mut w,&json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"diagnostics":[]}})).await.unwrap();
            for req in [two, one] {
                write_message(
                    &mut w,
                    &json!({"jsonrpc":"2.0","id":req["id"],"result":req["method"]}),
                )
                .await
                .unwrap();
            }
        });
        let (a, b) = tokio::join!(c.request("one", json!({})), c.request("two", json!({})));
        assert_eq!(a.unwrap(), "one");
        assert_eq!(b.unwrap(), "two");
        assert_eq!(
            c.events.recv().await.unwrap()["method"],
            "textDocument/publishDiagnostics"
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_request_releases_pending_slot() {
        let (a, _b) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        let result =
            tokio::time::timeout(Duration::from_millis(10), c.request("waiting", json!({}))).await;
        assert!(result.is_err());
        assert!(c.pending.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn staged_cancel_after_complete_frame_keeps_late_reply_and_next_request_independent() {
        // Deliberately smaller than either frame: actual async writes fragment.
        let (a, b) = tokio::io::duplex(32);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        let (seen, read) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(b);
            let mut r = BufReader::new(r);
            let first = read_message(&mut r).await.unwrap().unwrap();
            assert_eq!(first["method"], "diagnostic");
            let _ = seen.send(());
            let cancel = read_message(&mut r).await.unwrap().unwrap();
            assert_eq!(cancel["method"], "$/cancelRequest");
            assert!(
                cancel.get("id").is_none(),
                "cancellation is a notification, not a new request"
            );
            assert_eq!(cancel["params"]["id"], first["id"]);
            let second = read_message(&mut r).await.unwrap().unwrap();
            assert_eq!(second["method"], "next");
            for (request, result) in [(first, "obsolete"), (second, "actual next result")] {
                write_message(
                    &mut w,
                    &json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                )
                .await
                .unwrap();
            }
        });
        let first = c
            .begin_request("diagnostic", json!({"source":"😀 source"}))
            .await
            .unwrap();
        read.await.unwrap();
        assert_eq!(c.pending.lock().unwrap().len(), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), first.wait())
                .await
                .is_err()
        );
        assert!(c.pending.lock().unwrap().is_empty());
        let result = tokio::time::timeout(Duration::from_secs(3), c.request("next", json!({})))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, "actual next result");
        assert!(c.pending.lock().unwrap().is_empty());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn staged_drop_without_wait_sends_cancel_once_before_following_frame() {
        let (a, b) = tokio::io::duplex(128);
        let (r, w) = tokio::io::split(a);
        let mut c = LspClient::from_io(BufReader::new(r), w);
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(b);
            let mut r = BufReader::new(r);
            let first = read_message(&mut r).await.unwrap().unwrap();
            let cancel = read_message(&mut r).await.unwrap().unwrap();
            assert_eq!(
                cancel,
                json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":first["id"]}})
            );
            assert_eq!(
                read_message(&mut r).await.unwrap().unwrap()["method"],
                "barrier"
            );
            write_message(&mut w, &json!({"jsonrpc":"2.0","method":"done"}))
                .await
                .unwrap();
        });
        let ticket = c.begin_request("pending", json!({})).await.unwrap();
        drop(ticket);
        assert!(c.pending.lock().unwrap().is_empty());
        c.notify("barrier", json!({})).await.unwrap();
        assert_eq!(c.events.recv().await.unwrap()["method"], "done");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn staged_completed_response_never_sends_cancel() {
        let (a, b) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(b);
            let mut r = BufReader::new(r);
            let first = read_message(&mut r).await.unwrap().unwrap();
            write_message(
                &mut w,
                &json!({"jsonrpc":"2.0","id":first["id"],"result":47}),
            )
            .await
            .unwrap();
            assert_eq!(
                read_message(&mut r).await.unwrap().unwrap()["method"],
                "barrier"
            );
        });
        assert_eq!(
            c.begin_request("complete", json!({}))
                .await
                .unwrap()
                .wait()
                .await
                .unwrap(),
            47
        );
        c.notify("barrier", json!({})).await.unwrap();
        server.await.unwrap();
    }
    #[test]
    fn staged_drop_after_runtime_shutdown_only_cleans_the_slot() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (c, ticket, _peer) = runtime.block_on(async {
            let (a, b) = tokio::io::duplex(8192);
            let (r, w) = tokio::io::split(a);
            let c = LspClient::from_io(BufReader::new(r), w);
            let ticket = c.begin_request("pending", json!({})).await.unwrap();
            (c, ticket, b)
        });
        drop(runtime);
        drop(ticket);
        assert!(c.pending.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn interrupted_frame_is_poisoned_before_another_write_can_acquire_it() {
        let (a, _peer) = tokio::io::duplex(1);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                write_rpc(&c.writer, &json!({"method":"large","body":"partial"}))
            )
            .await
            .is_err()
        );
        assert!(c.writer.lock().await.incomplete);
        let error = write_rpc(&c.writer, &json!({"method":"next"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("interrupted frame"));
    }
    #[tokio::test]
    async fn cancelled_mutex_wait_does_not_poison_an_unwritten_frame() {
        let (a, b) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        let held = c.writer.lock().await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                write_rpc(&c.writer, &json!({"method":"never written"}))
            )
            .await
            .is_err()
        );
        assert!(!held.incomplete);
        drop(held);
        c.notify("actual", json!({})).await.unwrap();
        assert_eq!(
            read_message(&mut BufReader::new(b)).await.unwrap().unwrap()["method"],
            "actual"
        );
    }
    #[tokio::test]
    async fn failed_staged_send_never_arms_cancel_and_cleans_pending_slot() {
        let (a, peer) = tokio::io::duplex(1024);
        drop(peer);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        assert!(c.begin_request("broken", json!({})).await.is_err());
        assert!(c.pending.lock().unwrap().is_empty());
        assert!(c.writer.lock().await.incomplete);
    }
    #[tokio::test]
    async fn cancellation_id_is_scoped_to_one_client_not_an_equal_id_in_another() {
        let (a, pa) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(a);
        let a = LspClient::from_io(BufReader::new(r), w);
        let (b, pb) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(b);
        let b = LspClient::from_io(BufReader::new(r), w);
        let first = a.begin_request("a", json!({})).await.unwrap();
        let second = b.begin_request("b", json!({})).await.unwrap();
        let (seen, ready) = oneshot::channel();
        let server_a = tokio::spawn(async move {
            let mut r = BufReader::new(pa);
            let request = read_message(&mut r).await.unwrap().unwrap();
            let cancel = read_message(&mut r).await.unwrap().unwrap();
            assert_eq!(request["id"], 1);
            assert_eq!(cancel["params"]["id"], 1);
            assert_eq!(cancel["method"], "$/cancelRequest");
            let _ = seen.send(());
        });
        let server_b = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(pb);
            let mut r = BufReader::new(r);
            let request = read_message(&mut r).await.unwrap().unwrap();
            ready.await.unwrap();
            assert_eq!(request["id"], 1);
            write_message(
                &mut w,
                &json!({"jsonrpc":"2.0","id":request["id"],"result":"B remains current"}),
            )
            .await
            .unwrap();
        });
        drop(first);
        assert_eq!(second.wait().await.unwrap(), "B remains current");
        server_a.await.unwrap();
        server_b.await.unwrap();
    }
    #[tokio::test]
    async fn rejected_oversized_frame_does_not_poison_the_stream() {
        let (a, b) = tokio::io::duplex(1024);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        assert!(
            write_rpc(&c.writer, &json!({"too_big":"x".repeat(MAX_BODY+1)}))
                .await
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
        assert!(!c.writer.lock().await.incomplete);
        c.notify("small", json!({})).await.unwrap();
        assert_eq!(
            read_message(&mut BufReader::new(b)).await.unwrap().unwrap()["method"],
            "small"
        );
    }
    #[tokio::test]
    async fn responds_to_server_configuration_request() {
        let (a, b) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(a);
        let c = LspClient::from_io(BufReader::new(r), w);
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(b);
            let mut r = BufReader::new(r);
            let request = read_message(&mut r).await.unwrap().unwrap();
            write_message(&mut w,&json!({"jsonrpc":"2.0","id":"config","method":"workspace/configuration","params":{"items":[{"section":"kotlin"}]}})).await.unwrap();
            let response = read_message(&mut r).await.unwrap().unwrap();
            assert_eq!(response["id"], "config");
            assert_eq!(response["result"], json!([null]));
            write_message(
                &mut w,
                &json!({"jsonrpc":"2.0","id":request["id"],"result":true}),
            )
            .await
            .unwrap();
        });
        assert_eq!(c.request("test", json!({})).await.unwrap(), true);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn overlong_header_is_bounded() {
        let data = vec![b'a'; 9000];
        let mut r = BufReader::new(&data[..]);
        assert!(
            read_message(&mut r)
                .await
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
    }
    #[tokio::test]
    async fn execute_command_can_wait_for_editor_apply_edit_ack_without_deadlock() {
        let (a, b) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(a);
        let client = LspClient::from_io(BufReader::new(r), w);
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(b);
            let mut r = BufReader::new(r);
            let request = read_message(&mut r).await.unwrap().unwrap();
            write_message(&mut w,&json!({"jsonrpc":"2.0","id":"edit1","method":"workspace/applyEdit","params":{"edit":{"changes":{}}}})).await.unwrap();
            let ack = read_message(&mut r).await.unwrap().unwrap();
            assert_eq!(ack["id"], "edit1");
            assert_eq!(ack["result"]["applied"], true);
            write_message(
                &mut w,
                &json!({"jsonrpc":"2.0","id":request["id"],"result":true}),
            )
            .await
            .unwrap();
        });
        let (response, ()) = tokio::join!(
            client.request("workspace/executeCommand", json!({"command":"test"})),
            async {
                let edit = client.next_server_request().await.unwrap();
                assert_eq!(edit.method, "workspace/applyEdit");
                edit.reply.send(json!({"applied":true})).unwrap();
            }
        );
        assert_eq!(response.unwrap(), true);
        server.await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_child_exit_proof_requires_an_actual_reaped_success_status() {
        for code in [0, 7] {
            let mut client = LspClient::spawn(
                Path::new("/usr/bin/python3"),
                &[
                    "-c".into(),
                    format!("import time;time.sleep(.05);raise SystemExit({code})"),
                ],
                Path::new("/tmp"),
            )
            .unwrap();
            assert!(!client.child_exited_successfully());
            let status = client.child.as_mut().unwrap().wait().await.unwrap();
            assert_eq!(status.success(), code == 0);
            assert_eq!(client.child_exited_successfully(), code == 0);
        }
        let (a, _b) = tokio::io::duplex(64);
        let (r, w) = tokio::io::split(a);
        let mut transport = LspClient::from_io(BufReader::new(r), w);
        assert!(!transport.child_exited_successfully());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_closes_stdin_and_waits_for_actual_normal_exit() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("normal-eof-exit");
        let script = r#"import sys,json,pathlib
while True:
 line=sys.stdin.buffer.readline()
 if not line: break
 length=int(line.split(b':',1)[1]); assert sys.stdin.buffer.readline()==b'\r\n'
 message=json.loads(sys.stdin.buffer.read(length))
 if message.get('method')=='shutdown':
  body=json.dumps({'jsonrpc':'2.0','id':message['id'],'result':None}).encode();sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body);sys.stdout.buffer.flush()
pathlib.Path(sys.argv[1]).write_text('EOF then normal exit')
"#;
        let mut client = LspClient::spawn(
            Path::new("/usr/bin/python3"),
            &[
                "-c".into(),
                script.into(),
                marker.to_string_lossy().into_owned(),
            ],
            root.path(),
        )
        .unwrap();
        client.set_request_timeout(Duration::from_secs(2));
        client.shutdown().await.unwrap();
        assert!(
            client.child_exited_successfully(),
            "a completed shutdown RPC is not proof of a clean child exit"
        );
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "EOF then normal exit"
        );
        assert!(
            client
                .notify("after-exit", Value::Null)
                .await
                .unwrap_err()
                .to_string()
                .contains("closed")
        );
    }
}
