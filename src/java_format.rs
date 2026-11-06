//! Pinned external Java formatter. No project classpath, import fixups or user compilation.
use crate::{editor::Document, formatting::MAX_BYTES, kotlin::Range};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::watch,
};
#[derive(Deserialize)]
struct Pin {
    version: String,
    sha256: String,
}
fn pin() -> Result<Pin> {
    Ok(serde_json::from_str(include_str!(
        "../formatter/dependency.json"
    ))?)
}
fn cancelled(c: &watch::Receiver<bool>) -> bool {
    *c.borrow() || c.has_changed().is_err()
}
fn clean_command(c: &mut Command, root: &Path, secret: &[String]) {
    c.current_dir(root)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in secret.iter().map(String::as_str).chain([
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "CLASSPATH",
        "WEX_PRIVATE_STOREPASS",
        "WEX_PRIVATE_KEYPASS",
    ]) {
        c.env_remove(name);
    }
}
fn secret_names(root: &Path) -> Result<Vec<String>> {
    let bytes = match input_bytes(&root.join("wex.json"), 1024 * 1024) {
        Ok(v) => v,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(vec![]);
        }
        Err(e) => return Err(e),
    };
    let config: Value = serde_json::from_slice(&bytes)?;
    let signing = &config["build"]["release_signing"];
    if signing.is_null() {
        return Ok(vec![]);
    }
    let mut names = vec![];
    for field in ["store_password_env", "key_password_env"] {
        let name = signing[field]
            .as_str()
            .context("Invalid formatter credential environment reference")?;
        ensure!(
            !name.is_empty()
                && name.bytes().enumerate().all(|(i, c)| c == b'_'
                    || c.is_ascii_alphabetic()
                    || (i > 0 && c.is_ascii_digit())),
            "Invalid formatter credential environment name"
        );
        names.push(name.to_owned());
    }
    Ok(names)
}
async fn bounded(mut r: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    let mut b = [0; 8192];
    loop {
        let n = r.read(&mut b).await?;
        if n == 0 {
            return Ok(v);
        }
        ensure!(v.len() + n <= limit, "Java formatter output exceeded limit");
        v.extend_from_slice(&b[..n]);
    }
}
fn trace(value: Value) {
    use std::io::Write;
    if let Some(path) = std::env::var_os("WEX_JAVA_FORMAT_TRACE") {
        let _ = (|| -> Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            let mut data = serde_json::to_vec(&value)?;
            data.push(b'\n');
            f.write_all(&data)?;
            Ok(())
        })();
    }
}
/// Cancellation closes pipes, kills ONLY this Child and awaits its actual exit.
async fn run(
    mut command: Command,
    input: Vec<u8>,
    cancel: &mut watch::Receiver<bool>,
    limit: usize,
) -> Result<Vec<u8>> {
    ensure!(!cancelled(cancel), "Java formatting cancelled");
    let argv = command
        .as_std()
        .get_args()
        .map(|x| x.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let cwd = command.as_std().get_current_dir().map(|p| p.to_owned());
    let mut child = command.spawn().context("Starting Java formatter tool")?;
    let pid = child.id();
    trace(json!({"stage":"spawn","pid":pid,"program":program,"args":argv,"cwd":cwd}));
    let mut stdin = child.stdin.take().context("formatter stdin")?;
    let stdout = child.stdout.take().context("formatter stdout")?;
    let stderr = child.stderr.take().context("formatter stderr")?;
    enum Stop {
        Done(Result<Vec<u8>>),
        Cancel,
        Timeout,
    }
    let stop = {
        let work = async {
            let (_, out, err) = tokio::try_join!(
                async {
                    stdin.write_all(&input).await?;
                    drop(stdin);
                    Ok::<_, anyhow::Error>(())
                },
                bounded(stdout, limit),
                bounded(stderr, 65536)
            )?;
            let status = child.wait().await?;
            trace(
                json!({"stage":"exit","pid":pid,"code":status.code(),"success":status.success(),"reaped":true}),
            );
            ensure!(
                status.success(),
                "Java formatter failed ({status}): {}",
                String::from_utf8_lossy(&err)
                    .chars()
                    .take(4096)
                    .collect::<String>()
            );
            Ok(out)
        };
        tokio::select! {r=tokio::time::timeout(Duration::from_secs(60),work)=>match r{Ok(v)=>Stop::Done(v),Err(_)=>Stop::Timeout},_=cancel.changed()=>Stop::Cancel}
    };
    match stop {
        Stop::Done(Ok(v)) => {
            ensure!(!cancelled(cancel), "Java formatting cancelled");
            Ok(v)
        }
        other => {
            let _ = child.start_kill();
            let reaped = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            trace(
                json!({"stage":"stop","pid":pid,"cancelled":matches!(other,Stop::Cancel),"timed_out":matches!(other,Stop::Timeout),"reaped":matches!(reaped,Ok(Ok(_)))}),
            );
            match other {
                Stop::Done(Err(e)) => Err(e),
                Stop::Cancel => anyhow::bail!("Java formatting cancelled"),
                _ => anyhow::bail!("Java formatter timed out"),
            }
        }
    }
}
fn hash_bytes(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn input_bytes(path: &Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.len() <= max,
        "Invalid Java formatter input"
    );
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max,
        "Java formatter input grew past limit"
    );
    Ok(bytes)
}
fn fingerprint(path: &Path, max: u64) -> Result<String> {
    Ok(hash_bytes(&input_bytes(path, max)?))
}
pub async fn prepare(
    root: &Path,
    offline: bool,
    cancel: &mut watch::Receiver<bool>,
) -> Result<PathBuf> {
    let dir = root.to_owned();
    let config_root = dir.clone();
    let (support, tools, secret) = tokio::task::spawn_blocking(move || -> Result<_> {
        crate::runtime_paths::verify_support()?;
        Ok((
            crate::runtime_paths::paths().support.clone(),
            crate::runtime_paths::paths().tools.clone(),
            secret_names(&config_root)?,
        ))
    })
    .await??;
    let script = support.join("scripts/prepare-java-formatter.py");
    let check_script = script.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        ensure!(
            fingerprint(&check_script, 65536)?
                == hash_bytes(include_bytes!("../scripts/prepare-java-formatter.py")),
            "Java formatter preparation source changed; rebuild/restore support"
        );
        Ok(())
    })
    .await??;
    let mut c = Command::new("python3");
    c.arg(script).env("WEX_TOOLS_HOME", &tools);
    if offline {
        c.arg("--offline");
    }
    clean_command(&mut c, &dir, &secret);
    let bytes = run(c, vec![], cancel, 65536).await?;
    tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let p = pin()?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let jar = tools
            .canonicalize()?
            .join("java-formatter")
            .join(format!("google-java-format-{}-all-deps.jar", p.version));
        ensure!(
            value["schema"] == 1
                && value["version"] == p.version
                && value["sha256"] == p.sha256
                && value["jar"].as_str() == jar.to_str(),
            "Java formatter preparation receipt mismatch"
        );
        ensure!(
            fingerprint(&jar, 16 * 1024 * 1024)? == p.sha256,
            "Java formatter JAR hash mismatch"
        );
        Ok(jar)
    })
    .await?
}
struct Inputs {
    java: PathBuf,
    home: PathBuf,
    helper: PathBuf,
    helper_hash: String,
    secret: Vec<String>,
    scratch: tempfile::TempDir,
}
fn inputs(root: &Path) -> Result<Inputs> {
    crate::runtime_paths::verify_support()?;
    let home = std::env::var_os("WEX_FORMAT_JAVA_HOME")
        .or_else(|| std::env::var_os("JAVA_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/jvm/java-21-openjdk"))
        .canonicalize()
        .context("Java formatter requires a configured JDK21+")?;
    let release = String::from_utf8(input_bytes(&home.join("release"), 1024 * 1024)?)?;
    let version = release
        .lines()
        .find_map(|l| l.strip_prefix("JAVA_VERSION=\""))
        .and_then(|l| l.split(['.', '-', '+', '\"']).next())
        .and_then(|v| v.parse::<u32>().ok())
        .context("Invalid formatter JDK version")?;
    ensure!(
        version >= 21 && home.join("bin/javac").is_file(),
        "Java formatter needs JDK21+; set WEX_FORMAT_JAVA_HOME explicitly"
    );
    let java = home.join("bin/java").canonicalize()?;
    ensure!(
        java.starts_with(&home),
        "Java formatter launcher is outside configured JDK"
    );
    let helper = crate::runtime_paths::paths()
        .support
        .join("formatter/WexJavaFormat.java");
    let helper_hash = fingerprint(&helper, 65536)?;
    ensure!(
        helper_hash == hash_bytes(include_bytes!("../formatter/WexJavaFormat.java")),
        "Java formatter bridge source changed; restore verified support"
    );
    let scratch_root = crate::runtime_paths::paths()
        .tools
        .join("formatter-scratch");
    std::fs::create_dir_all(&scratch_root)?;
    Ok(Inputs {
        java,
        home,
        helper,
        helper_hash,
        secret: secret_names(root)?,
        scratch: tempfile::Builder::new()
            .prefix("java-")
            .tempdir_in(scratch_root)?,
    })
}
/// Shared production Java route, including source/disk fences and backend identity.
pub async fn review(
    capture: crate::formatting::Capture,
    range: Option<Range>,
    cancel: watch::Receiver<bool>,
) -> Result<crate::formatting::Review> {
    ensure!(
        crate::formatting::Backend::for_path(&capture.path)?
            == crate::formatting::Backend::GoogleJava,
        "Java formatter needs a Java source"
    );
    let value = edits(
        &capture.root,
        capture.text.clone(),
        range.clone(),
        cancel.clone(),
    )
    .await?;
    let mut result = tokio::task::spawn_blocking(move || capture.review(&value, range)).await??;
    ensure!(!cancelled(&cancel), "Java formatting cancelled");
    result.backend = crate::formatting::Backend::GoogleJava;
    Ok(result)
}
/// Same reviewed TextEdit contract as K2; UTF16 offsets are never scalar indices.
pub async fn edits(
    root: &Path,
    text: String,
    range: Option<Range>,
    mut cancel: watch::Receiver<bool>,
) -> Result<Value> {
    ensure!(
        text.len() <= MAX_BYTES && !cancelled(&cancel),
        "Java formatting cancelled or exceeds8MiB"
    );
    let dir = root.to_owned();
    let captured = text.clone();
    let requested = range.clone();
    let (tools, start, end) = tokio::task::spawn_blocking(move || -> Result<_> {
        let tools = inputs(&dir)?;
        let doc = Document::new(&captured)?;
        let (a, b) = if let Some(r) = requested {
            let a = doc.char_at_lsp(r.start.line, r.start.character)?;
            let b = doc.char_at_lsp(r.end.line, r.end.character)?;
            ensure!(a <= b, "reversed Java format selection");
            (a, b)
        } else {
            (0, doc.rope().len_chars())
        };
        let a = doc
            .rope()
            .slice(..a)
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();
        let b = doc
            .rope()
            .slice(..b)
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();
        Ok((tools, a, b))
    })
    .await??;
    let jar = prepare(
        root,
        std::env::var_os("WEX_FORMATTER_OFFLINE").is_some(),
        &mut cancel,
    )
    .await?;
    let mut c = Command::new(&tools.java);
    c.args(["-Xmx384m", "-XX:ActiveProcessorCount=2"])
        .arg(format!(
            "-Djava.io.tmpdir={}",
            tools.scratch.path().display()
        ));
    for package in ["api", "code", "file", "parser", "tree", "util"] {
        c.arg(format!(
            "--add-exports=jdk.compiler/com.sun.tools.javac.{package}=ALL-UNNAMED"
        ));
    }
    c.arg("--class-path")
        .arg(&jar)
        .arg(&tools.helper)
        .arg(start.to_string())
        .arg(end.to_string())
        .env("JAVA_HOME", &tools.home);
    clean_command(&mut c, root, &tools.secret);
    let bytes = run(c, text.as_bytes().to_vec(), &mut cancel, 32 * 1024 * 1024).await?;
    let value = tokio::task::spawn_blocking(move || -> Result<_> {
        ensure!(
            fingerprint(&jar, 16 * 1024 * 1024)? == pin()?.sha256
                && fingerprint(&tools.helper, 65536)? == tools.helper_hash,
            "Java formatter inputs changed during formatting"
        );
        decode(&text, &bytes)
    })
    .await??;
    ensure!(!cancelled(&cancel), "Java formatting cancelled");
    Ok(value)
}
fn decode(text: &str, bytes: &[u8]) -> Result<Value> {
    use std::collections::{BTreeMap, BTreeSet};
    ensure!(
        bytes.starts_with(b"WEXJF001"),
        "Invalid Java formatter protocol"
    );
    let mut at = 8;
    fn word(bytes: &[u8], at: &mut usize) -> Result<usize> {
        let end = at
            .checked_add(4)
            .context("Java formatter offset overflow")?;
        let b = bytes
            .get(*at..end)
            .context("Truncated Java formatter protocol")?;
        *at = end;
        Ok(u32::from_be_bytes(b.try_into()?) as usize)
    }
    let count = word(bytes, &mut at)?;
    ensure!(count <= 100000, "Too many Java formatting edits");
    let mut raw = Vec::with_capacity(count);
    let mut positions = BTreeSet::new();
    let mut total = 0;
    let mut previous: Option<(usize, usize)> = None;
    for _ in 0..count {
        let a = word(bytes, &mut at)?;
        let b = word(bytes, &mut at)?;
        ensure!(a <= b, "Reversed Java formatter edit");
        if let Some((start, end)) = previous {
            ensure!(
                a >= end && a != start,
                "Overlapping/ambiguous Java formatter edits"
            );
        }
        previous = Some((a, b));
        let len = word(bytes, &mut at)?;
        total += len;
        ensure!(total <= MAX_BYTES, "Java formatter expansion exceeds8MiB");
        let end = at
            .checked_add(len)
            .context("Java formatter text overflow")?;
        let s = std::str::from_utf8(
            bytes
                .get(at..end)
                .context("Truncated Java formatter text")?,
        )?
        .to_owned();
        at = end;
        positions.insert(a);
        positions.insert(b);
        raw.push((a, b, s));
    }
    ensure!(at == bytes.len(), "Trailing Java formatter output");
    let mut mapped = BTreeMap::new();
    let (mut offset, mut line, mut column) = (0usize, 0u32, 0u32);
    if positions.contains(&0) {
        mapped.insert(0, (0usize, 0u32, 0u32));
    }
    for (byte, ch) in text.char_indices() {
        offset += ch.len_utf16();
        advance(&mut line, &mut column, ch);
        if positions.contains(&offset) {
            mapped.insert(offset, (byte + ch.len_utf8(), line, column));
        }
    }
    let mut edits = vec![];
    let raw_count = raw.len();
    for (index, (a, b, replacement)) in raw.into_iter().enumerate() {
        let start = mapped
            .get(&a)
            .context("Java formatter UTF16 start is invalid")?;
        let end = mapped
            .get(&b)
            .context("Java formatter UTF16 end is invalid")?;
        let budget = 100000 - edits.len() - (raw_count - index - 1);
        if let Some(fine) = whitespace_edits(
            &text[start.0..end.0],
            &replacement,
            (start.1, start.2),
            budget,
        ) {
            edits.extend(fine);
        } else {
            edits.push(json!({"range":{"start":{"line":start.1,"character":start.2},"end":{"line":end.1,"character":end.2}},"newText":replacement}));
        }
    }
    Ok(Value::Array(edits))
}
fn advance(line: &mut u32, column: &mut u32, ch: char) {
    if ch == '\n' {
        *line += 1;
        *column = 0;
    } else {
        *column += ch.len_utf16() as u32;
    }
}
/// Refine ONLY an unchanged non-whitespace sequence, copying the formatter's
/// exact whitespace runs. No style decisions, token rewrite or output filtering.
/// Wide replacements would otherwise move a token caret to a raw relative offset.
fn whitespace_edits(
    before: &str,
    after: &str,
    (mut line, mut column): (u32, u32),
    budget: usize,
) -> Option<Vec<Value>> {
    if !before
        .chars()
        .filter(|c| !c.is_whitespace())
        .eq(after.chars().filter(|c| !c.is_whitespace()))
    {
        return None;
    }
    let (mut a, mut b) = (before.chars().peekable(), after.chars().peekable());
    let mut edits = vec![];
    loop {
        let start = (line, column);
        let (mut old, mut new) = (String::new(), String::new());
        while a.peek().is_some_and(|c| c.is_whitespace()) {
            let ch = a.next()?;
            old.push(ch);
            advance(&mut line, &mut column, ch);
        }
        while b.peek().is_some_and(|c| c.is_whitespace()) {
            new.push(b.next()?);
        }
        if old != new {
            edits.push(json!({"range":{"start":{"line":start.0,"character":start.1},"end":{"line":line,"character":column}},"newText":new}));
            if edits.len() > budget {
                return None;
            }
        }
        match (a.next(), b.next()) {
            (None, None) => break,
            (Some(x), Some(y)) if x == y => advance(&mut line, &mut column, x),
            _ => return None,
        }
    }
    Some(edits)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn packet(a: u32, b: u32, text: &[u8]) -> Vec<u8> {
        let mut p = b"WEXJF001".to_vec();
        for x in [1, a, b, text.len() as u32] {
            p.extend_from_slice(&x.to_be_bytes());
        }
        p.extend_from_slice(text);
        p
    }
    #[cfg(unix)]
    #[test]
    fn tool_inputs_reject_fifo_symlink_and_oversize_without_blocking() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("file");
        std::fs::write(&p, b"12345").unwrap();
        assert!(input_bytes(&p, 4).is_err());
        let alias = d.path().join("alias");
        symlink(&p, &alias).unwrap();
        assert!(input_bytes(&alias, 10).is_err());
        let fifo = d.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(input_bytes(&fifo, 10).is_err());
    }
    #[test]
    fn exact_whitespace_refinement_preserves_token_caret_with_newlines_and_emoji() {
        let before = "class A{String text=\"😀 x\";int value=1;}\n";
        let after = "class A {\n    String text = \"😀 x\";\n    int value = 1;\n}\n";
        let data = decode(
            before,
            &packet(0, before.encode_utf16().count() as u32, after.as_bytes()),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("A.java");
        std::fs::write(&path, before).unwrap();
        let doc = Document::open(&path).unwrap();
        let review =
            crate::formatting::Capture::read(dir.path(), &path, before.into(), doc.version)
                .unwrap()
                .review(&data, None)
                .unwrap();
        assert_eq!(review.after, after);
        let old = before[..before.find("value").unwrap()].chars().count();
        let expected = after[..after.find("value").unwrap()].chars().count();
        assert_eq!(review.map_position(old, true), expected);
        assert!(whitespace_edits("x", "y", (0, 0), 10).is_none());
        assert!(whitespace_edits("x=1", "x = 1", (0, 0), 1).is_none());
    }
    #[test]
    fn protocol_maps_utf16_and_rejects_surrogates_trailing_and_bounds() {
        let p = packet(3, 4, b"y");
        assert_eq!(
            decode("\n😀x", &p).unwrap()[0]["range"]["start"],
            json!({"line":1,"character":2})
        );
        assert!(decode("😀", &packet(1, 2, b"y")).is_err());
        assert!(decode("x", &packet(2, 2, b"y")).is_err());
        let mut p = packet(0, 1, b"x");
        p.push(0);
        assert!(decode("x", &p).is_err());
        assert!(decode("x", &packet(0, 1, &[255])).is_err());
        let mut bad = packet(0, 1, b"x");
        bad[8..12].copy_from_slice(&2u32.to_be_bytes());
        bad.extend_from_slice(&packet(0, 1, b"x")[12..]);
        assert!(decode("x", &bad).is_err());
    }
    #[test]
    fn protocol_rejects_truncation_and_large_counts() {
        for p in [
            vec![],
            b"WEXJF001".to_vec(),
            packet(0, 1, b"x")[..15].to_vec(),
        ] {
            assert!(decode("x", &p).is_err());
        }
        let mut p = b"WEXJF001".to_vec();
        p.extend_from_slice(&100001u32.to_be_bytes());
        assert!(decode("", &p).is_err());
    }
    #[tokio::test]
    async fn real_child_does_not_receive_configured_credentials_or_jvm_injection() {
        let d = tempfile::tempdir().unwrap();
        let mut c = Command::new("python3");
        c.args(["-c","import os;assert all(k not in os.environ for k in ['STORE_CANARY','KEY_CANARY','JAVA_TOOL_OPTIONS','CLASSPATH']);print('ENV_STRIPPED')"]);
        for name in [
            "STORE_CANARY",
            "KEY_CANARY",
            "JAVA_TOOL_OPTIONS",
            "CLASSPATH",
        ] {
            c.env(name, "must-not-reach-child");
        }
        clean_command(
            &mut c,
            d.path(),
            &["STORE_CANARY".into(), "KEY_CANARY".into()],
        );
        let (_tx, mut rx) = watch::channel(false);
        assert_eq!(
            run(c, vec![], &mut rx, 1024).await.unwrap(),
            b"ENV_STRIPPED\n"
        );
    }
    #[tokio::test]
    async fn process_cancel_reaps_only_its_child() {
        let d = tempfile::tempdir().unwrap();
        let ready = d.path().join("ready");
        let mut c = Command::new("python3");
        // Publish a complete PID atomically. Existence alone can observe the
        // empty file between open(O_CREAT) and write, making join("") == /proc.
        c.args(["-c","import os,pathlib,sys,time;p=pathlib.Path(sys.argv[1]);q=p.with_suffix('.stage');q.write_text(str(os.getpid()));os.replace(q,p);time.sleep(30)"]).arg(&ready);
        clean_command(&mut c, d.path(), &[]);
        let mut unrelated = Command::new("python3")
            .args(["-c", "import time;time.sleep(30)"])
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (tx, mut rx) = watch::channel(false);
        let job = tokio::spawn(async move { run(c, vec![], &mut rx, 1024).await });
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(std::time::Instant::now() < end);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(&ready)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert!(pid > 1);
        let proc = Path::new("/proc").join(pid.to_string());
        let info = std::fs::read_to_string(proc.join("stat")).unwrap();
        assert_eq!(
            info.rsplit(')')
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u32>()
                .unwrap(),
            std::process::id()
        );
        tx.send_replace(true);
        assert!(
            job.await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("Java formatting cancelled")
        );
        assert!(!proc.exists());
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().await.unwrap();
        unrelated.wait().await.unwrap();
    }
}
