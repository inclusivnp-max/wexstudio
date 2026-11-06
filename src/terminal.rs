//! Lazy project PTY. All spawn, parsing, I/O, resize and reaping run off the UI thread.
use anyhow::{Result, ensure};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
static LIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);
pub fn live_workers() -> usize {
    LIVE_WORKERS.load(Ordering::Acquire)
}
pub fn drain_on_exit() -> Result<()> {
    let end = Instant::now() + Duration::from_secs(5);
    while live_workers() != 0 {
        ensure!(
            Instant::now() < end,
            "PTY shutdown still pending: {} workers",
            live_workers()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
pub const MAX_INPUT: usize = 64 * 1024;
pub const SCROLLBACK: usize = 2000;
pub const MAX_ROWS: u16 = 100;
pub const MAX_COLS: u16 = 240;
pub type Wake = Arc<dyn Fn() + Send + Sync>;
#[derive(Clone, Debug)]
pub struct Shell {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}
impl Shell {
    pub fn project(cwd: PathBuf) -> Self {
        let program = std::env::var_os("SHELL")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/bin/bash"));
        Self {
            program,
            args: vec!["-i".into()],
            cwd,
            env: vec![],
        }
    }
    pub fn fixture(cwd: PathBuf) -> Self {
        Self {
            program: PathBuf::from("/bin/bash"),
            args: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
            cwd,
            env: vec![
                ("PS1".into(), "wex$ ".into()),
                ("PROMPT_COMMAND".into(), String::new()),
                ("HISTFILE".into(), "/dev/null".into()),
            ],
        }
    }
}
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub enum State {
    Starting,
    Running,
    Stopping,
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Failed(String),
}
#[derive(Clone)]
pub struct Frame {
    pub state: State,
    pub pid: Option<u32>,
    pub rows: u16,
    pub cols: u16,
    pub cells: Vec<vt100::Cell>,
    pub wrapped: Vec<bool>,
    pub cursor: (u16, u16),
    pub hide_cursor: bool,
    pub scrollback: usize,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub alternate: bool,
    pub generation: u64,
    pub output_bytes: u64,
    pub input_bytes: u64,
    pub worker_thread: String,
    pub error: Option<String>,
}
impl Frame {
    pub fn cell(&self, row: u16, col: u16) -> Option<&vt100::Cell> {
        if row >= self.rows || col >= self.cols {
            None
        } else {
            self.cells
                .get(row as usize * self.cols as usize + col as usize)
        }
    }
    pub fn text(&self) -> String {
        self.selected((0, 0), (self.rows - 1, self.cols))
    }
    pub fn selected(&self, a: (u16, u16), b: (u16, u16)) -> String {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let mut result = String::new();
        for row in a.0..=b.0.min(self.rows - 1) {
            let start = if row == a.0 { a.1 } else { 0 };
            let end = if row == b.0 { b.1 } else { self.cols };
            let mut line = String::new();
            for col in start..end.min(self.cols) {
                if let Some(c) = self.cell(row, col) {
                    if c.is_wide_continuation() {
                        continue;
                    }
                    if c.has_contents() {
                        line.push_str(c.contents());
                    } else {
                        line.push(' ');
                    }
                }
            }
            if row != b.0 {
                if self.wrapped[row as usize] && end == self.cols {
                    result.push_str(&line);
                } else {
                    result.push_str(line.trim_end());
                }
                if !self.wrapped[row as usize] {
                    result.push('\n');
                }
            } else {
                result.push_str(line.trim_end());
            }
        }
        result
    }
}
fn dimensions(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(2, MAX_ROWS), cols.clamp(8, MAX_COLS))
}
#[derive(Clone)]
pub struct Cleanup {
    done: Arc<AtomicBool>,
}
impl Cleanup {
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}
enum Command {
    Input(Vec<u8>),
    Resize(u16, u16),
    Scroll(isize),
}
pub struct Handle {
    tx: mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    latest: Arc<Mutex<Arc<Frame>>>,
    pub visible: Arc<AtomicBool>,
    cleanup: Cleanup,
    initial: Arc<Frame>,
}
impl Handle {
    pub fn start(shell: Shell, rows: u16, cols: u16, wake: Wake) -> Result<Self> {
        let (rows, cols) = dimensions(rows, cols);
        let parser = vt100::Parser::new(rows, cols, 0);
        let frame = frame(&parser, State::Starting, None, 0, 0, 0, None);
        let initial = Arc::new(frame);
        let latest = Arc::new(Mutex::new(initial.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let visible = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::sync_channel(32);
        let h = Self {
            tx,
            stop: stop.clone(),
            latest: latest.clone(),
            visible: visible.clone(),
            cleanup: Cleanup { done: done.clone() },
            initial,
        };
        LIVE_WORKERS.fetch_add(1, Ordering::AcqRel);
        let spawned = std::thread::Builder::new()
            .name("wex-pty".into())
            .spawn(move || {
                struct Done(Arc<AtomicBool>);
                impl Drop for Done {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                        LIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                let _done = Done(done);
                let result = worker(
                    shell,
                    rows,
                    cols,
                    rx,
                    stop,
                    latest.clone(),
                    visible,
                    wake.clone(),
                );
                if let Err(e) = result {
                    if let Ok(mut slot) = latest.lock() {
                        let mut f = (**slot).clone();
                        f.state = State::Failed(format!("{e:#}"));
                        *slot = Arc::new(f);
                    }
                    wake();
                }
            });
        if let Err(e) = spawned {
            LIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
            return Err(e.into());
        }
        Ok(h)
    }
    pub fn initial(&self) -> Arc<Frame> {
        self.initial.clone()
    }
    pub fn snapshot(&self) -> Option<Arc<Frame>> {
        self.latest.try_lock().ok().map(|f| f.clone())
    }
    pub fn input(&self, bytes: Vec<u8>) -> Result<()> {
        ensure!(
            bytes.len() <= MAX_INPUT,
            "Terminal input exceeds 64KiB per action"
        );
        self.send(Command::Input(bytes))
    }
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let (r, c) = dimensions(rows, cols);
        self.send(Command::Resize(r, c))
    }
    pub fn scroll(&self, rows: isize) -> Result<()> {
        self.send(Command::Scroll(rows))
    }
    fn send(&self, c: Command) -> Result<()> {
        ensure!(!self.stop.load(Ordering::Acquire), "Terminal is closing");
        self.tx
            .try_send(c)
            .map_err(|e| anyhow::anyhow!("Terminal input queue: {e}"))
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn cleanup(&self) -> Cleanup {
        self.cleanup.clone()
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}
#[derive(Default)]
struct Replies {
    bytes: Vec<u8>,
}
impl Replies {
    fn reply(&mut self, b: &[u8]) {
        if self.bytes.len() + b.len() <= 4096 {
            self.bytes.extend_from_slice(b);
        }
    }
}
impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        s: &mut vt100::Screen,
        i: Option<u8>,
        _: Option<u8>,
        p: &[&[u16]],
        c: char,
    ) {
        let n = p.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i, n, c) {
            (None, 5, 'n') => self.reply(b"\x1b[0n"),
            (None, 6, 'n') => {
                let (r, c) = s.cursor_position();
                self.reply(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
            }
            (None, 0, 'c') => self.reply(b"\x1b[?1;2c"),
            (Some(b'>'), 0, 'c') => self.reply(b"\x1b[>0;100;0c"),
            _ => {}
        }
    }
}
// vte's std OSC accumulator is unbounded. Buffer a complete OSC ourselves before
// forwarding, with a fixed limit; oversized strings are discarded through BEL/ST.
// Normal printable bytes, fragmented UTF8/CSI and DCS are otherwise unchanged.
#[derive(Default)]
struct EscapeGate {
    esc: bool,
    osc: Option<Vec<u8>>,
    dropping: bool,
    osc_esc: bool,
}
impl EscapeGate {
    fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.osc.is_some() || self.dropping {
                let end = b == 7 || (self.osc_esc && b == b'\\');
                let cancel = b == 24 || b == 26;
                if let Some(buf) = &mut self.osc {
                    if buf.len() < 16384 {
                        buf.push(b);
                    } else {
                        self.osc = None;
                        self.dropping = true;
                    }
                }
                if end || cancel {
                    if !cancel && let Some(buf) = self.osc.take() {
                        out.extend(buf);
                    }
                    self.osc = None;
                    self.dropping = false;
                    self.osc_esc = false;
                } else {
                    self.osc_esc = b == 27;
                }
            } else if self.esc {
                self.esc = false;
                if b == b']' {
                    self.osc = Some(vec![27, b']']);
                } else {
                    out.push(27);
                    if b == 27 {
                        self.esc = true;
                    } else {
                        out.push(b);
                    }
                }
            } else if b == 27 {
                self.esc = true;
            } else {
                out.push(b);
            }
        }
        out
    }
}
fn frame<CB: vt100::Callbacks>(
    p: &vt100::Parser<CB>,
    state: State,
    pid: Option<u32>,
    generation: u64,
    output_bytes: u64,
    input_bytes: u64,
    error: Option<String>,
) -> Frame {
    let s = p.screen();
    let (rows, cols) = s.size();
    let mut cells = Vec::with_capacity(rows as usize * cols as usize);
    let mut wrapped = vec![];
    for row in 0..rows {
        wrapped.push(s.row_wrapped(row));
        for col in 0..cols {
            cells.push(s.cell(row, col).unwrap().clone());
        }
    }
    Frame {
        state,
        pid,
        rows,
        cols,
        cells,
        wrapped,
        cursor: s.cursor_position(),
        hide_cursor: s.hide_cursor(),
        scrollback: s.scrollback(),
        application_cursor: s.application_cursor(),
        bracketed_paste: s.bracketed_paste(),
        alternate: s.alternate_screen(),
        generation,
        output_bytes,
        input_bytes,
        worker_thread: format!("{:?}", std::thread::current().id()),
        error,
    }
}
#[cfg(not(target_os = "linux"))]
fn worker(
    _: Shell,
    _: u16,
    _: u16,
    _: mpsc::Receiver<Command>,
    _: Arc<AtomicBool>,
    _: Arc<Mutex<Arc<Frame>>>,
    _: Arc<AtomicBool>,
    _: Wake,
) -> Result<()> {
    anyhow::bail!("The project PTY backend currently targets Linux")
}
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn worker(
    shell: Shell,
    rows: u16,
    cols: u16,
    rx: mpsc::Receiver<Command>,
    stop: Arc<AtomicBool>,
    latest: Arc<Mutex<Arc<Frame>>>,
    visible: Arc<AtomicBool>,
    wake: Wake,
) -> Result<()> {
    use std::{
        fs::File,
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::process::{CommandExt, ExitStatusExt},
        },
        process::{Child, Command as Process, Stdio},
    };
    struct Pty {
        master: File,
        child: Child,
        closed: bool,
    }
    impl Pty {
        fn close(&mut self) -> Option<std::process::ExitStatus> {
            if self.closed {
                return None;
            }
            self.closed = true;
            // The child remains unreaped until its session has been signalled, so
            // its SID cannot be mistaken for a later reused process identity.
            let sid = self.child.id() as i32;
            signal_session(sid, libc::SIGHUP);
            let end = Instant::now() + Duration::from_millis(150);
            while Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
            signal_session(sid, libc::SIGKILL);
            self.child.wait().ok()
        }
    }
    impl Drop for Pty {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }
    ensure!(
        shell.program.is_absolute(),
        "Shell program must be absolute"
    );
    ensure!(shell.cwd.is_dir(), "Terminal project directory is missing");
    let (mut master, mut slave) = (-1, -1);
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors or fails; all become RAII-owned.
    ensure!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        } == 0,
        "openpty: {}",
        std::io::Error::last_os_error()
    );
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
            "PTY close-on-exec: {}",
            std::io::Error::last_os_error()
        );
    }
    ensure!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == 0,
        "PTY nonblocking: {}",
        std::io::Error::last_os_error()
    );
    let slave = File::from(slave);
    let mut cmd = Process::new(&shell.program);
    cmd.args(&shell.args)
        .current_dir(&shell.cwd)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("WEX_TERMINAL", "1")
        .envs(shell.env)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe syscalls run between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    drop(cmd);
    let pid = child.id();
    let mut pty = Pty {
        master: File::from(master),
        child,
        closed: false,
    };
    let mut parser = vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, Replies::default());
    let mut gate = EscapeGate::default();
    let mut outgoing = VecDeque::<u8>::new();
    let (mut generation, mut output_bytes, mut input_bytes) = (0, 0, 0);
    let mut dirty = true;
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut state = State::Running;
    let mut error = None;
    let mut exited = false;
    let mut eof = false;
    let mut session_signalled = false;
    let mut scratch = [0u8; 65536];
    loop {
        if stop.load(Ordering::Acquire) {
            state = State::Stopping;
            publish(
                &latest,
                frame(
                    &parser,
                    state.clone(),
                    Some(pid),
                    generation,
                    output_bytes,
                    input_bytes,
                    error.clone(),
                ),
            );
            wake();
            let status = pty.close();
            state = State::Exited {
                code: status.as_ref().and_then(|s| s.code()),
                signal: status.as_ref().and_then(|s| s.signal()),
            };
            dirty = true;
            exited = true;
        }
        for _ in 0..16 {
            if outgoing.len() > 128 * 1024 {
                break;
            }
            let c = match rx.try_recv() {
                Ok(c) => c,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    stop.store(true, Ordering::Release);
                    break;
                }
            };
            match c {
                Command::Input(bytes) => {
                    if !exited {
                        parser.screen_mut().set_scrollback(0);
                        outgoing.extend(bytes);
                        dirty = true;
                    }
                }
                Command::Resize(r, c) => {
                    if parser.screen().size() != (r, c) {
                        let s = libc::winsize {
                            ws_row: r,
                            ws_col: c,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                        if unsafe { libc::ioctl(pty.master.as_raw_fd(), libc::TIOCSWINSZ, &s) } < 0
                        {
                            error =
                                Some(format!("PTY resize: {}", std::io::Error::last_os_error()));
                        } else {
                            parser.screen_mut().set_size(r, c);
                        }
                        dirty = true;
                    }
                }
                Command::Scroll(delta) => {
                    let old = parser.screen().scrollback();
                    parser
                        .screen_mut()
                        .set_scrollback(old.saturating_add_signed(delta));
                    dirty = true;
                }
            }
        }
        if !exited && !outgoing.is_empty() {
            let (a, b) = outgoing.as_slices();
            let bytes = if a.is_empty() { b } else { a };
            match pty.master.write(bytes) {
                Ok(n) => {
                    outgoing.drain(..n);
                    input_bytes += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    error = Some(format!("PTY write: {e}"));
                    stop.store(true, Ordering::Release);
                }
            }
        }
        let mut read_output = false;
        if !eof && !exited {
            match pty.master.read(&mut scratch) {
                Ok(0) => eof = true,
                Ok(n) => {
                    read_output = true;
                    output_bytes += n as u64;
                    let bytes = gate.filter(&scratch[..n]);
                    parser.process(&bytes);
                    outgoing.extend(std::mem::take(&mut parser.callbacks_mut().bytes));
                    dirty = true;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.raw_os_error() == Some(libc::EIO) => eof = true,
                Err(e) => {
                    error = Some(format!("PTY read: {e}"));
                    stop.store(true, Ordering::Release);
                }
            }
        }
        // waitid WNOWAIT observes terminal state without reaping the SID owner
        // before same-session jobs have been terminated on the worker.
        if !exited && child_exited(pid)? {
            if !session_signalled {
                signal_session(pid as i32, libc::SIGHUP);
                signal_session(pid as i32, libc::SIGKILL);
                session_signalled = true;
            }
            if !eof && read_output {
                continue;
            }
            signal_session(pid as i32, libc::SIGHUP);
            signal_session(pid as i32, libc::SIGKILL);
            let status = pty.child.wait()?;
            pty.closed = true;
            state = State::Exited {
                code: status.code(),
                signal: status.signal(),
            };
            exited = true;
            dirty = true;
        }
        if dirty && (last.elapsed() >= Duration::from_millis(24) || exited) {
            generation += 1;
            publish(
                &latest,
                frame(
                    &parser,
                    state.clone(),
                    Some(pid),
                    generation,
                    output_bytes,
                    input_bytes,
                    error.clone(),
                ),
            );
            if visible.load(Ordering::Acquire) || exited {
                wake();
            }
            dirty = false;
            last = Instant::now();
        }
        if exited {
            break;
        }
        let mut fd = libc::pollfd {
            fd: pty.master.as_raw_fd(),
            events: libc::POLLIN
                | if outgoing.is_empty() {
                    0
                } else {
                    libc::POLLOUT
                },
            revents: 0,
        };
        unsafe {
            libc::poll(&mut fd, 1, 10);
        }
    }
    Ok(())
}
fn publish(slot: &Mutex<Arc<Frame>>, f: Frame) {
    if let Ok(mut s) = slot.lock() {
        *s = Arc::new(f);
    }
}
#[cfg(target_os = "linux")]
fn child_exited(pid: u32) -> Result<bool> {
    let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut status,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    ensure!(
        r == 0,
        "PTY child observation: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { status.si_pid() } != 0)
}
#[cfg(target_os = "linux")]
fn signal_session(sid: i32, signal: i32) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for e in entries.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let identity = || {
            let s = std::fs::read_to_string(e.path().join("stat")).ok()?;
            s.rsplit_once(") ")?
                .1
                .split_whitespace()
                .nth(3)?
                .parse::<i32>()
                .ok()
        };
        if identity() != Some(sid) {
            continue;
        }
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if fd < 0 {
            continue;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if identity() == Some(sid) {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
}
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    fn terminal() -> (tempfile::TempDir, Handle) {
        let d = tempfile::tempdir().unwrap();
        let h = Handle::start(
            Shell::fixture(d.path().to_owned()),
            12,
            100,
            Arc::new(|| {}),
        )
        .unwrap();
        (d, h)
    }
    fn until(h: &Handle, f: impl Fn(&Frame) -> bool) -> Arc<Frame> {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(s) = h.snapshot() {
                if f(&s) {
                    return s;
                }
                assert!(!matches!(s.state, State::Failed(_)), "{:?}", s.state);
            }
            assert!(Instant::now() < end, "actual PTY condition timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn genuine_controlling_pty_project_directory_color_utf8_resize_and_exit() {
        let (d, h) = terminal();
        until(&h, |f| f.text().contains("wex$"));
        h.input(b"test -t 0 && test -t 1 && test -t 2 && printf '\\033[31mPTY_REAL_%s_\xe6\x97\xa5\xe6\x9c\xac\\033[0m\\n' \"$PWD\"\r".to_vec()).unwrap();
        let f = until(&h, |f| {
            f.text()
                .contains(&format!("PTY_REAL_{}_日本", d.path().display()))
        });
        assert!(f.cells.iter().any(|c| c.fgcolor() == vt100::Color::Idx(1)));
        assert!(f.pid.is_some());
        assert_ne!(
            f.worker_thread,
            format!("{:?}", std::thread::current().id())
        );
        h.resize(20, 110).unwrap();
        until(&h, |f| f.rows == 20 && f.cols == 110);
        h.input(b"stty size\r".to_vec()).unwrap();
        until(&h, |f| f.text().contains("20 110"));
        h.input(b"exit 7\r".to_vec()).unwrap();
        let f = until(&h, |f| {
            matches!(f.state, State::Exited { code: Some(7), .. })
        });
        assert!(f.output_bytes > 0);
        until_done(h.cleanup());
    }
    fn until_done(c: Cleanup) {
        let end = Instant::now() + Duration::from_secs(5);
        while !c.done() {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn actual_foreground_sigint_does_not_kill_shell() {
        let (_d, h) = terminal();
        let initial = until(&h, |f| f.text().contains("wex$"));
        let shell = initial.pid.unwrap();
        let foreground = |pid: u32| -> Option<u32> {
            let value = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            value[value.rfind(')')? + 2..]
                .split_whitespace()
                .nth(5)?
                .parse()
                .ok()
        };
        h.input(b"sleep 30\r".to_vec()).unwrap();
        // Verify the REAL foreground sleep, not an arbitrary scheduling delay.
        let running = until(&h, |_| {
            foreground(shell)
                .filter(|p| *p != shell)
                .is_some_and(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/comm"))
                        .is_ok_and(|s| s.trim_end() == "sleep")
                })
        });
        h.input(vec![3]).unwrap();
        // Bash may flush typeahead while regaining its terminal. Wait for its
        // actual new prompt before issuing the command that verifies survival.
        until(&h, |f| {
            f.output_bytes > running.output_bytes
                && foreground(shell) == Some(shell)
                && f.text()
                    .lines()
                    .rfind(|s| !s.trim().is_empty())
                    .is_some_and(|s| s.trim_end() == "wex$")
        });
        h.input(b"printf 'INT_RETURNED\\n'\r".to_vec()).unwrap();
        until(&h, |f| f.text().lines().any(|l| l == "INT_RETURNED"));
        h.stop();
        until_done(h.cleanup());
    }
    #[test]
    fn closing_reaps_shell_and_same_session_background_jobs() {
        let (d, h) = terminal();
        until(&h, |f| f.text().contains("wex$"));
        h.input(b"(trap '' HUP; sleep 60) & echo $! > child.pid\r".to_vec())
            .unwrap();
        let f = until(&h, |f| f.text().contains("[1]"));
        let end = Instant::now() + Duration::from_secs(5);
        let path = d.path().join("child.pid");
        while !path.exists() {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: usize = std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        h.stop();
        until_done(h.cleanup());
        assert!(!PathBuf::from(format!("/proc/{}", f.pid.unwrap())).exists());
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert!(
            stat.is_empty()
                || stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, s)| s.starts_with('Z'))
        );
    }
    #[test]
    fn shell_failure_is_visible_and_worker_finishes() {
        let d = tempfile::tempdir().unwrap();
        let mut shell = Shell::fixture(d.path().to_owned());
        shell.program = d.path().join("missing");
        let h = Handle::start(shell, 12, 80, Arc::new(|| {})).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if h.snapshot()
                .is_some_and(|f| matches!(f.state, State::Failed(_)))
            {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        until_done(h.cleanup());
    }
    #[test]
    fn oversized_input_rejected_and_drop_requests_cleanup() {
        let (_d, h) = terminal();
        let f = until(&h, |f| f.pid.is_some());
        assert!(h.input(vec![b'a'; MAX_INPUT + 1]).is_err());
        let c = h.cleanup();
        drop(h);
        until_done(c);
        assert!(!PathBuf::from(format!("/proc/{}", f.pid.unwrap())).exists());
    }
    #[test]
    fn ansi_fragmented_unicode_alternate_and_bracketed_modes() {
        let mut p = vt100::Parser::new(4, 20, 20);
        let s = "\x1b[32m日本é\x1b[0m\r\nline2\x1b[?2004h\x1b[?1h";
        for b in s.as_bytes() {
            p.process(&[*b]);
        }
        assert!(p.screen().contents().contains("日本é"));
        assert!(p.screen().cell(0, 0).unwrap().is_wide());
        assert!(p.screen().bracketed_paste() && p.screen().application_cursor());
        p.process(b"\x1b[?1049hALT\x1b[?1049l");
        assert!(!p.screen().alternate_screen());
        assert!(p.screen().contents().contains("line2"));
    }
    #[test]
    fn bounded_osc_gate_preserves_fragmentation_and_resumes_printable_data() {
        let mut g = EscapeGate::default();
        assert_eq!(g.filter(b"before\x1b"), b"before");
        assert!(g.filter(b"]2;title").is_empty());
        assert_eq!(g.filter(b"\x1b\\after"), b"\x1b]2;title\x1b\\after");
        assert!(g.filter(b"\x1b]52;").is_empty());
        assert!(g.filter(&vec![b'x'; 100000]).is_empty());
        assert!(g.osc.is_none() && g.dropping);
        assert_eq!(g.filter(b"\x07OK"), b"OK");
        assert_eq!(g.filter("日本".as_bytes()), "日本".as_bytes());
    }
    #[test]
    fn selection_wide_cells_wrap_and_reverse_are_exact() {
        let mut p = vt100::Parser::new(4, 8, 0);
        p.process("abc日本xyz\r\nlast".as_bytes());
        let f = frame(&p, State::Running, None, 1, 0, 0, None);
        assert_eq!(f.selected((0, 0), (1, 3)), "abc日本xyz");
        assert_eq!(f.selected((1, 3), (0, 0)), "abc日本xyz");
        assert_eq!(f.selected((0, 3), (0, 7)), "日本");
    }
    #[test]
    fn cursor_device_queries_generate_bounded_actual_replies() {
        let mut p = vt100::Parser::new_with_callbacks(10, 40, 0, Replies::default());
        p.process(b"\x1b[3;8H\x1b[6n\x1b[5n\x1b[c");
        assert_eq!(p.callbacks().bytes, b"\x1b[3;8R\x1b[0n\x1b[?1;2c");
    }
    #[test]
    fn exiting_shell_with_hup_ignoring_output_flood_reaps_session() {
        let (_d, h) = terminal();
        until(&h, |f| f.text().contains("wex$"));
        h.input(b"(trap '' HUP; while :; do printf flood; done) & exit 3\r".to_vec())
            .unwrap();
        until(&h, |f| {
            matches!(f.state, State::Exited { code: Some(3), .. })
        });
        until_done(h.cleanup());
    }
    #[test]
    fn wrapped_selection_retains_real_spaces() {
        let mut p = vt100::Parser::new(3, 8, 0);
        p.process(b"abc     xyz");
        let f = frame(&p, State::Running, None, 1, 0, 0, None);
        assert_eq!(f.selected((0, 0), (1, 3)), "abc     xyz");
    }
}
