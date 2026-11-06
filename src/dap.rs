//! DAP request correlation and bounded events over a real debug-adapter child.
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncWrite, BufReader},
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
struct RequestGuard {
    id: u64,
    pending: Pending,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}
pub struct Client {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Pending,
    seq: AtomicU64,
    closed: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    child: std::sync::Mutex<Child>,
}
fn trace(direction: &str, value: &Value) {
    if let Some(p) = std::env::var_os("WEX_DAP_TRACE") {
        static TRACE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(p)
        {
            let _ = writeln!(f, "{direction} {value}");
        }
    }
}
impl Client {
    pub fn spawn(command: &mut Command) -> Result<(Arc<Self>, mpsc::Receiver<Value>)> {
        use std::process::Stdio;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let input = child.stdin.take().context("adapter stdin")?;
        let mut output = BufReader::new(child.stdout.take().context("adapter stdout")?);
        let pending: Pending = Default::default();
        let p = pending.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let ended = closed.clone();
        let (tx, rx) = mpsc::channel(256);
        let reader = tokio::spawn(async move {
            let result = async {
                while let Some(message) = crate::lsp::read_message(&mut output).await? {
                    trace("recv", &message);
                    match message["type"].as_str() {
                        Some("response") => {
                            let id = message["request_seq"]
                                .as_u64()
                                .context("invalid DAP response id")?;
                            if let Some(reply) = p.lock().unwrap().remove(&id) {
                                let result = if message["success"].as_bool() == Some(true) {
                                    Ok(message["body"].clone())
                                } else {
                                    Err(anyhow!(
                                        "DAP {}: {}",
                                        message["command"],
                                        message["message"]
                                    ))
                                };
                                let _ = reply.send(result);
                            }
                        }
                        Some("event") => {
                            if matches!(message["event"].as_str(), Some("output" | "thread")) {
                                let _ = tx.try_send(message);
                            } else {
                                // Do not block the only response reader behind a UI event queue.
                                // Critical state is never silently dropped: overflow ends the session
                                // and fails pending requests, rather than deadlocking attach/stepping.
                                match tx.try_send(message) {
                                    Ok(()) => {}
                                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                                    Err(mpsc::error::TrySendError::Full(_)) => {
                                        anyhow::bail!("DAP critical event queue overflow")
                                    }
                                }
                            }
                        }
                        Some("request") => anyhow::bail!("unsupported adapter reverse request"),
                        _ => anyhow::bail!("invalid DAP message type"),
                    }
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let error = result
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "debug adapter disconnected".into());
            ended.store(true, Ordering::Release);
            for (_, reply) in p.lock().unwrap().drain() {
                let _ = reply.send(Err(anyhow!(error.clone())));
            }
            let _ = tx
                .send(json!({"type":"event","event":"terminated","body":{"error":error}}))
                .await;
        });
        Ok((
            Arc::new(Self {
                writer: Mutex::new(Box::new(input)),
                pending,
                seq: AtomicU64::new(1),
                closed,
                reader,
                child: std::sync::Mutex::new(child),
            }),
            rx,
        ))
    }
    pub async fn request(&self, command: &str, arguments: Value) -> Result<Value> {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().unwrap();
            ensure!(
                !self.closed.load(Ordering::Acquire),
                "debug adapter is closed"
            );
            ensure!(p.len() < 128, "too many pending DAP requests");
            p.insert(id, tx);
        }
        let _guard = RequestGuard {
            id,
            pending: self.pending.clone(),
        };
        let message = json!({"seq":id,"type":"request","command":command,"arguments":arguments});
        trace("send", &message);
        crate::lsp::write_message(&mut **self.writer.lock().await, &message).await?;
        tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .context("DAP request timed out")?
            .context("DAP reader closed")?
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
        let _ = self.child.lock().unwrap().start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const IO: &str = r#"
import sys,json

def read():
 n=None
 while True:
  line=sys.stdin.buffer.readline()
  if not line: sys.exit(0)
  if line in (b'\r\n',b'\n'):break
  if line.lower().startswith(b'content-length:'):n=int(line.split(b':')[1])
 return json.loads(sys.stdin.buffer.read(n))
def send(x):
 b=json.dumps(x).encode();sys.stdout.buffer.write(b'Content-Length: '+str(len(b)).encode()+b'\r\n\r\n'+b);sys.stdout.buffer.flush()
"#;
    fn adapter(body: &str) -> (Arc<Client>, mpsc::Receiver<Value>) {
        let mut c = Command::new("python3");
        c.args(["-u", "-c", &(IO.to_string() + body)]);
        Client::spawn(&mut c).unwrap()
    }
    #[tokio::test]
    async fn critical_event_overflow_fails_requests_without_response_reader_deadlock() {
        let (client, mut events) = adapter(
            "\nr=read()\nfor i in range(300):send({'type':'event','event':'breakpoint','body':{'id':i}})\nsend({'type':'response','request_seq':r['seq'],'success':True,'body':{}})\nsys.stdin.buffer.read()\n",
        );
        let result =
            tokio::time::timeout(Duration::from_secs(3), client.request("attach", json!({})))
                .await
                .expect("event backlog must not stall the response reader");
        assert!(result.unwrap_err().to_string().contains("overflow"));
        loop {
            let event = tokio::time::timeout(Duration::from_secs(3), events.recv())
                .await
                .unwrap()
                .unwrap();
            if event["event"] == "terminated" {
                assert!(
                    event["body"]["error"]
                        .as_str()
                        .unwrap()
                        .contains("overflow")
                );
                break;
            }
        }
        assert!(client.pending.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn output_flood_does_not_block_responses_or_grow_the_event_queue() {
        let (client, mut events) = adapter(
            "\nr=read()\nfor i in range(1000):send({'type':'event','event':'output','body':{'output':'noise'}})\nsend({'type':'response','request_seq':r['seq'],'success':True,'body':{'ok':True}})\nsys.stdin.buffer.read()\n",
        );
        let result =
            tokio::time::timeout(Duration::from_secs(3), client.request("threads", json!({})))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(events.len(), 256);
        assert_eq!(events.recv().await.unwrap()["event"], "output");
    }
    #[tokio::test]
    async fn out_of_order_responses_and_events_are_correlated() {
        let (client, mut events) = adapter(
            "\na=read();b=read();send({'type':'event','event':'stopped','body':{'threadId':7}})\nfor r in [b,a]:send({'type':'response','request_seq':r['seq'],'success':True,'body':{'command':r['command']}})\n",
        );
        let (a, b) = tokio::join!(
            client.request("threads", json!({})),
            client.request("stackTrace", json!({}))
        );
        assert_eq!(a.unwrap()["command"], "threads");
        assert_eq!(b.unwrap()["command"], "stackTrace");
        assert_eq!(events.recv().await.unwrap()["body"]["threadId"], 7);
        assert!(client.pending.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn error_response_is_not_reported_as_success() {
        let (client, _) = adapter(
            "\nr=read();send({'type':'response','request_seq':r['seq'],'success':False,'command':r['command'],'message':'not suspended'})\n",
        );
        assert!(
            client
                .request("next", json!({}))
                .await
                .unwrap_err()
                .to_string()
                .contains("not suspended")
        );
    }
    #[tokio::test]
    async fn adapter_exit_fails_pending_request_and_emits_termination() {
        let (client, mut events) = adapter("\nread()\n");
        assert!(client.request("attach", json!({})).await.is_err());
        assert_eq!(events.recv().await.unwrap()["event"], "terminated");
        assert!(
            client
                .request("threads", json!({}))
                .await
                .unwrap_err()
                .to_string()
                .contains("closed")
        );
        assert!(client.pending.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn cancellation_releases_pending_request_slot() {
        let (client, _) = adapter("\nread();sys.stdin.buffer.read()\n");
        let c = client.clone();
        let task = tokio::spawn(async move { c.request("attach", json!({})).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        task.abort();
        let _ = task.await;
        assert!(client.pending.lock().unwrap().is_empty());
    }
}
