//! ADB smart-socket protocol. No adb subprocesses are spawned here.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

const MAX_REPLY: usize = 0xffff;
#[derive(Clone, Debug)]
pub struct AdbClient {
    address: String,
    deadline: Duration,
    operation_deadline: Duration,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
    pub transport_id: Option<String>,
}
impl Device {
    pub fn usable(&self) -> bool {
        self.state == "device"
    }
}

impl Default for AdbClient {
    fn default() -> Self {
        Self::new("127.0.0.1:5037")
    }
}
impl AdbClient {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            deadline: Duration::from_secs(5),
            operation_deadline: Duration::from_secs(120),
        }
    }
    pub fn with_timeout(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self.operation_deadline = deadline;
        self
    }
    async fn connect(&self) -> Result<TcpStream> {
        let stream = timeout(self.deadline, TcpStream::connect(&self.address))
            .await
            .context("ADB connection timed out")?
            .with_context(|| {
                format!(
                    "ADB server at {}: start the server once with adb start-server",
                    self.address
                )
            })?;
        // Smart-socket and JDWP are small request/response exchanges. Avoid
        // Nagle + delayed ACK postponing the claim until a DDM monitor owns ART.
        stream.set_nodelay(true)?;
        Ok(stream)
    }
    pub async fn devices(&self) -> Result<Vec<Device>> {
        timeout(self.deadline, async {
            let mut stream = self.connect().await?;
            request(&mut stream, "host:devices-l").await?;
            Ok(parse_devices(&String::from_utf8(
                read_frame(&mut stream).await?,
            )?))
        })
        .await
        .context("ADB devices timed out")?
    }
    /// Returns raw binary bytes, not shell-v2 frames or a PTY stream.
    pub async fn logcat(&self, serial: &str) -> Result<TcpStream> {
        self.service(serial, "exec:logcat -B -b main -b system -b crash")
            .await
    }
    /// Preselect the exact transport before a latency-sensitive JDWP notification.
    pub(crate) async fn transport(&self, serial: &str) -> Result<TcpStream> {
        ensure!(
            !serial.is_empty() && !serial.contains(['\n', '\r', '\0']),
            "invalid device serial"
        );
        timeout(self.deadline, async {
            let mut stream = self.connect().await?;
            request(&mut stream, &format!("host:transport:{serial}")).await?;
            Ok(stream)
        })
        .await
        .context("ADB transport negotiation timed out")?
    }
    pub async fn service(&self, serial: &str, service: &str) -> Result<TcpStream> {
        timeout(self.deadline, async {
            let mut stream = self.transport(serial).await?;
            request(&mut stream, service).await?;
            Ok(stream)
        })
        .await
        .context("ADB service negotiation timed out")?
    }
    /// shell-v2 carries separate stdout/stderr and a required remote exit status.
    pub async fn shell(&self, serial: &str, args: &[&str]) -> Result<ShellOutput> {
        ensure!(!args.is_empty(), "empty device command");
        let command = args
            .iter()
            .map(|s| quote_arg(s))
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        timeout(self.operation_deadline, async {
            let mut stream = self
                .service(serial, &format!("shell,v2,raw:{command}"))
                .await?;
            shell_packet(&mut stream, 4, &[]).await?;
            read_shell(&mut stream).await
        })
        .await
        .context("ADB shell operation timed out")?
    }
    /// Stream a base/universal APK directly to PackageManager; no remote temp file, no adb CLI.
    pub async fn install_apk(&self, serial: &str, path: &std::path::Path) -> Result<InstallResult> {
        let mut file = tokio::fs::File::open(path).await?;
        let metadata = file.metadata().await?;
        let size = metadata.len();
        ensure!(
            metadata.is_file() && (1..=1024 * 1024 * 1024).contains(&size),
            "invalid APK file size"
        );
        let command = format!("shell,v2,raw:cmd package install -r -t --user current -S {size}");
        timeout(self.operation_deadline, async {
            let stream = self.service(serial, &command).await?;
            let (mut read, mut write) = stream.into_split();
            let sender = async {
                let mut remaining = size;
                let mut buffer = [0u8; 65536];
                let mut hash = blake3::Hasher::new();
                while remaining > 0 {
                    let max = remaining.min(buffer.len() as u64) as usize;
                    let n = file.read(&mut buffer[..max]).await?;
                    ensure!(n > 0, "APK changed/truncated during install");
                    hash.update(&buffer[..n]);
                    shell_packet(&mut write, 0, &buffer[..n]).await?;
                    remaining -= n as u64;
                }
                shell_packet(&mut write, 4, &[]).await?;
                Ok::<_, anyhow::Error>(hash.finalize().to_hex().to_string())
            };
            let (hash, output) = {
                let receiver = read_shell(&mut read);
                tokio::pin!(sender, receiver);
                tokio::select! {
                    biased;
                    output = &mut receiver => {
                        let output = output?;
                        check_install(&output)?;
                        (sender.await?, output)
                    }
                    hash = &mut sender => {
                        let hash = hash?;
                        let output = receiver.await?;
                        check_install(&output)?;
                        (hash, output)
                    }
                }
            };
            let after = file.metadata().await?;
            ensure!(
                after.len() == size && after.modified()? == metadata.modified()?,
                "APK changed while streaming"
            );
            Ok(InstallResult {
                bytes: size,
                blake3: hash,
                output,
            })
        })
        .await
        .context("APK install timed out")?
    }
    pub async fn logcat_since(&self, serial: &str, device_seconds: u64) -> Result<TcpStream> {
        ensure!(device_seconds > 0, "invalid device timestamp");
        self.service(
            serial,
            &format!("exec:logcat -B -b main -b system -b crash -T {device_seconds}.0"),
        )
        .await
    }
    pub async fn forward_jdwp(&self, serial: &str, port: u16, pid: u32) -> Result<()> {
        ensure!(
            port != 0
                && pid != 0
                && !serial.is_empty()
                && !serial.contains([';', '\n', '\r', '\0']),
            "invalid JDWP endpoint"
        );
        timeout(self.deadline, async {
            let mut stream = self.connect().await?;
            // Forward has two statuses: smart socket and service result.
            request(
                &mut stream,
                &format!("host-serial:{serial}:forward:tcp:{port};jdwp:{pid}"),
            )
            .await?;
            read_status(&mut stream).await
        })
        .await
        .context("ADB forward timed out")?
    }
    /// SYNC v1 push streams 64 KiB chunks; APK installation is a separate operation.
    pub async fn push(&self, serial: &str, local: &std::path::Path, remote: &str) -> Result<u64> {
        ensure!(
            remote.starts_with('/') && remote.len() <= 1010 && !remote.contains(['\0', ',']),
            "invalid remote path"
        );
        let mut file = tokio::fs::File::open(local).await?;
        let mut stream = self.service(serial, "sync:").await?;
        timeout(self.deadline, async {
            let path = format!("{remote},33206"); // regular file, 0666 subject to device umask
            sync_packet(&mut stream, b"SEND", path.as_bytes()).await?;
            let mut buffer = [0u8; 65536];
            let mut total = 0;
            loop {
                let count = file.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                sync_packet(&mut stream, b"DATA", &buffer[..count]).await?;
                total += count as u64;
            }
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as u32;
            stream.write_all(b"DONE").await?;
            stream.write_all(&secs.to_le_bytes()).await?;
            let mut response = [0; 8];
            stream.read_exact(&mut response).await?;
            let len = u32::from_le_bytes(response[4..8].try_into()?) as usize;
            match &response[..4] {
                b"OKAY" => {
                    ensure!(len == 0, "malformed SYNC OKAY");
                    Ok(total)
                }
                b"FAIL" => {
                    ensure!(len <= MAX_REPLY, "oversized SYNC failure");
                    let mut msg = vec![0; len];
                    stream.read_exact(&mut msg).await?;
                    bail!("SYNC: {}", String::from_utf8_lossy(&msg))
                }
                _ => bail!("invalid SYNC response"),
            }
        })
        .await
        .context("ADB push timed out")?
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u32,
}
impl ShellOutput {
    pub fn check(self, operation: &str) -> Result<Self> {
        ensure!(
            self.exit_code == 0,
            "{operation} failed (exit {}): {}{}",
            self.exit_code,
            self.stdout,
            self.stderr
        );
        Ok(self)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallResult {
    pub bytes: u64,
    pub blake3: String,
    pub output: ShellOutput,
}
fn check_install(output: &ShellOutput) -> Result<()> {
    ensure!(
        output.exit_code == 0 && output.stdout.lines().any(|line| line.trim() == "Success"),
        "APK install failed (exit {}): {}{}",
        output.exit_code,
        output.stdout,
        output.stderr
    );
    Ok(())
}
fn quote_arg(value: &str) -> Result<String> {
    ensure!(!value.contains('\0'), "NUL in device argument");
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
}
async fn shell_packet<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    id: u8,
    bytes: &[u8],
) -> Result<()> {
    stream.write_u8(id).await?;
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await?;
    stream.write_all(bytes).await?;
    Ok(())
}
async fn read_shell<R: tokio::io::AsyncRead + Unpin>(stream: &mut R) -> Result<ShellOutput> {
    const LIMIT: usize = 4 * 1024 * 1024;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let mut header = [0; 5];
        stream
            .read_exact(&mut header)
            .await
            .context("shell stream ended without exit status")?;
        let size = u32::from_le_bytes(header[1..].try_into()?) as usize;
        ensure!(size <= LIMIT, "oversized shell packet");
        ensure!(
            stdout.len() + stderr.len() + size <= LIMIT,
            "shell output exceeds 4 MiB"
        );
        let mut bytes = vec![0; size];
        stream.read_exact(&mut bytes).await?;
        match header[0] {
            1 => stdout.extend(bytes),
            2 => stderr.extend(bytes),
            3 => {
                let exit = match bytes.len() {
                    1 => bytes[0] as u32,
                    4 => u32::from_le_bytes(bytes.try_into().unwrap()),
                    _ => anyhow::bail!("invalid shell exit packet"),
                };
                return Ok(ShellOutput {
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    exit_code: exit,
                });
            }
            _ => anyhow::bail!("unexpected shell packet id {}", header[0]),
        }
    }
}
async fn sync_packet(stream: &mut TcpStream, id: &[u8; 4], data: &[u8]) -> Result<()> {
    stream.write_all(id).await?;
    stream.write_all(&(data.len() as u32).to_le_bytes()).await?;
    stream.write_all(data).await?;
    Ok(())
}
pub(crate) async fn request(stream: &mut TcpStream, command: &str) -> Result<()> {
    ensure!(
        command.len() <= MAX_REPLY,
        "ADB request exceeds 65535 bytes"
    );
    stream
        .write_all(format!("{:04X}", command.len()).as_bytes())
        .await?;
    stream.write_all(command.as_bytes()).await?;
    read_status(stream).await
}
async fn read_status(stream: &mut TcpStream) -> Result<()> {
    let mut status = [0u8; 4];
    stream.read_exact(&mut status).await?;
    match &status {
        b"OKAY" => Ok(()),
        b"FAIL" => bail!(
            "ADB: {}",
            String::from_utf8_lossy(&read_frame(stream).await?)
        ),
        _ => bail!("invalid ADB status: {status:?}"),
    }
}
pub(crate) async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).await?;
    let len = usize::from_str_radix(std::str::from_utf8(&header)?, 16)
        .context("invalid ADB frame length")?;
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}
pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?.to_owned();
            let state = fields.next()?.to_owned();
            let mut model = None;
            let mut transport_id = None;
            for field in fields {
                if let Some(v) = field.strip_prefix("model:") {
                    model = Some(v.replace('_', " "));
                }
                if let Some(v) = field.strip_prefix("transport_id:") {
                    transport_id = Some(v.to_owned());
                }
            }
            Some(Device {
                serial,
                state,
                model,
                transport_id,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    async fn command(stream: &mut TcpStream) -> String {
        String::from_utf8(read_frame(stream).await.unwrap()).unwrap()
    }
    #[test]
    fn preserves_offline_and_unauthorized() {
        let d = parse_devices("a device model:Pixel_9 transport_id:2\nb unauthorized\nc offline\n");
        assert_eq!(d.len(), 3);
        assert!(d[0].usable());
        assert!(!d[1].usable());
        assert_eq!(d[0].model.as_deref(), Some("Pixel 9"));
    }
    #[tokio::test]
    async fn devices_fragmented_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = AdbClient::new(listener.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            assert_eq!(command(&mut s).await, "host:devices-l");
            for byte in b"OKAY0011abc device model:X" {
                s.write_all(&[*byte]).await.unwrap();
            }
        });
        assert_eq!(client.devices().await.unwrap()[0].serial, "abc");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn propagates_adb_failure() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let s = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            command(&mut s).await;
            s.write_all(b"FAIL0007offline").await.unwrap();
        });
        assert!(
            c.devices()
                .await
                .unwrap_err()
                .to_string()
                .contains("offline")
        );
        s.await.unwrap();
    }
    #[tokio::test]
    async fn service_selects_transport_then_raw_exec() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let s = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            assert_eq!(command(&mut s).await, "host:transport:abc");
            s.write_all(b"OKAY").await.unwrap();
            assert_eq!(
                command(&mut s).await,
                "exec:logcat -B -b main -b system -b crash"
            );
            s.write_all(b"OKAYraw").await.unwrap();
        });
        let mut stream = c.logcat("abc").await.unwrap();
        assert!(
            stream.nodelay().unwrap(),
            "latency-sensitive ADB/JDWP must disable Nagle"
        );
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        assert_eq!(raw, b"raw");
        s.await.unwrap();
    }
    #[tokio::test]
    async fn stalled_server_times_out() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string())
            .with_timeout(Duration::from_millis(20));
        let s = tokio::spawn(async move {
            let (_s, _) = l.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        assert!(
            c.devices()
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        s.abort();
    }
    #[tokio::test]
    async fn sync_push_streams_chunks_and_done() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.apk");
        let expected = vec![42u8; 70000];
        std::fs::write(&path, &expected).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            assert_eq!(command(&mut s).await, "host:transport:test");
            s.write_all(b"OKAY").await.unwrap();
            assert_eq!(command(&mut s).await, "sync:");
            s.write_all(b"OKAY").await.unwrap();
            let mut actual = Vec::new();
            let mut first = true;
            loop {
                let mut header = [0; 8];
                s.read_exact(&mut header).await.unwrap();
                let n = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
                if &header[..4] == b"DONE" {
                    assert!(n > 0);
                    break;
                }
                assert!(n <= 65536);
                let mut data = vec![0; n];
                s.read_exact(&mut data).await.unwrap();
                if first {
                    assert_eq!(&header[..4], b"SEND");
                    assert_eq!(data, b"/data/local/tmp/sample.apk,33206");
                    first = false;
                } else {
                    assert_eq!(&header[..4], b"DATA");
                    actual.extend(data);
                }
            }
            assert_eq!(actual, expected);
            s.write_all(b"OKAY\0\0\0\0").await.unwrap();
        });
        assert_eq!(
            c.push("test", &path, "/data/local/tmp/sample.apk")
                .await
                .unwrap(),
            70000
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn jdwp_forward_reads_both_statuses() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            assert_eq!(
                command(&mut s).await,
                "host-serial:test:forward:tcp:8700;jdwp:123"
            );
            s.write_all(b"OKAYOKAY").await.unwrap();
        });
        c.forward_jdwp("test", 8700, 123).await.unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn jdwp_forward_propagates_second_failure() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            command(&mut s).await;
            s.write_all(b"OKAYFAIL0004busy").await.unwrap();
        });
        assert!(
            c.forward_jdwp("test", 8700, 123)
                .await
                .unwrap_err()
                .to_string()
                .contains("busy")
        );
        server.await.unwrap();
    }
    async fn negotiate(s: &mut TcpStream, service: &str) {
        assert_eq!(command(s).await, "host:transport:chosen");
        s.write_all(b"OKAY").await.unwrap();
        assert_eq!(command(s).await, service);
        s.write_all(b"OKAY").await.unwrap();
    }
    async fn packet(s: &mut TcpStream) -> (u8, Vec<u8>) {
        let id = s.read_u8().await.unwrap();
        let n = s.read_u32_le().await.unwrap() as usize;
        assert!(n <= 65536);
        let mut b = vec![0; n];
        s.read_exact(&mut b).await.unwrap();
        (id, b)
    }
    #[test]
    fn shell_quoting_and_install_receipt_validation() {
        assert_eq!(quote_arg("a'; echo bad").unwrap(), "'a'\"'\"'; echo bad'");
        assert!(quote_arg("x\0y").is_err());
        for (stdout, exit_code) in [
            ("Success-ish", 0),
            ("Success\n", 1),
            ("Failure [REJECTED]", 0),
        ] {
            assert!(
                check_install(&ShellOutput {
                    stdout: stdout.into(),
                    stderr: String::new(),
                    exit_code
                })
                .is_err()
            );
        }
    }
    #[tokio::test]
    async fn shell_v2_fragmented_stdout_stderr_exit_and_argv() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            negotiate(&mut s, "shell,v2,raw:'echo' 'hello world' 'a'\"'\"'b'").await;
            assert_eq!(packet(&mut s).await, (4, vec![]));
            for bytes in [
                vec![1, 3, 0, 0, 0, b'o', b'u', b't'],
                vec![2, 3, 0, 0, 0, b'e', b'r', b'r'],
                vec![3, 1, 0, 0, 0, 7],
            ] {
                for b in bytes {
                    s.write_all(&[b]).await.unwrap();
                }
            }
        });
        let out = c
            .shell("chosen", &["echo", "hello world", "a'b"])
            .await
            .unwrap();
        assert_eq!(out.stdout, "out");
        assert_eq!(out.stderr, "err");
        assert_eq!(out.exit_code, 7);
        assert!(out.check("test").is_err());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn shell_rejects_truncation_missing_exit_oversize_and_invalid_ids() {
        for b in [
            vec![1, 2, 0, 0, 0, b'x'],
            vec![1, 0, 0, 0, 0],
            vec![1, 0, 0, 0, 1],
            vec![8, 0, 0, 0, 0],
            vec![3, 2, 0, 0, 0, 0, 0],
        ] {
            let mut input = b.as_slice();
            assert!(read_shell(&mut input).await.is_err());
        }
        let mut input: &[u8] = &[3, 4, 0, 0, 0, 42, 0, 0, 0];
        assert_eq!(read_shell(&mut input).await.unwrap().exit_code, 42);
    }
    #[tokio::test]
    async fn apk_install_exact_bytes_streaming_hash_and_remote_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.apk");
        let bytes: Vec<u8> = (0..170003).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let expected = bytes.clone();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            negotiate(
                &mut s,
                "shell,v2,raw:cmd package install -r -t --user current -S 170003",
            )
            .await;
            let mut actual = vec![];
            loop {
                let (id, b) = packet(&mut s).await;
                if id == 4 {
                    assert!(b.is_empty());
                    break;
                }
                assert_eq!(id, 0);
                actual.extend(b);
            }
            assert_eq!(actual, expected);
            shell_packet(&mut s, 1, b"Success\n").await.unwrap();
            shell_packet(&mut s, 3, &[0]).await.unwrap();
        });
        let installed = c.install_apk("chosen", &path).await.unwrap();
        assert_eq!(installed.bytes, bytes.len() as u64);
        assert_eq!(installed.blake3, blake3::hash(&bytes).to_hex().to_string());
        assert_eq!(installed.output.stdout, "Success\n");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn early_package_manager_failure_is_not_a_timeout_or_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.apk");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(32 * 1024 * 1024)
            .unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string())
            .with_timeout(Duration::from_secs(2));
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            negotiate(
                &mut s,
                "shell,v2,raw:cmd package install -r -t --user current -S 33554432",
            )
            .await;
            shell_packet(&mut s, 2, b"Failure [INSTALL_FAILED_TEST_ONLY]")
                .await
                .unwrap();
            shell_packet(&mut s, 3, &[1]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let err = c.install_apk("chosen", &path).await.unwrap_err();
        assert!(
            err.to_string().contains("INSTALL_FAILED_TEST_ONLY"),
            "{err:#}"
        );
        server.abort();
    }
    #[tokio::test]
    async fn shell_command_operation_has_a_deadline() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string())
            .with_timeout(Duration::from_millis(40));
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            negotiate(&mut s, "shell,v2,raw:'sleep' '999'").await;
            tokio::time::sleep(Duration::from_secs(3)).await;
        });
        assert!(
            c.shell("chosen", &["sleep", "999"])
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        server.abort();
    }
    #[tokio::test]
    async fn startup_log_subscription_replays_from_device_timestamp() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = AdbClient::new(l.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            negotiate(
                &mut s,
                "exec:logcat -B -b main -b system -b crash -T 1792000000.0",
            )
            .await;
        });
        let _ = c.logcat_since("chosen", 1792000000).await.unwrap();
        server.await.unwrap();
    }
}
