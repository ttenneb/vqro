//! Standalone tmux control-mode backend client, not connected to vqro's provider/core layers.
//!
//! One connection observes only panes linked into its attached session. Effects are never retried.
//! Slow subscribers and native pause-after loss are explicit gaps; capture is a labelled screen
//! repaint, NOT raw history and NOT a source of OSC status. See docs/research/tmux-client.md.
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
use super::parser::{self, Notification, PaneId};

use super::osc::Osc7501;
use super::parser::{Parser, Record};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot};

const MAX_COMMAND: usize = 64 * 1024;
const MAX_INPUT: usize = 16 * 1024;
/// Bytes per `send-keys -H` command (one argument each); well under tmux's parser limit.
const SEND_KEYS_BYTES: usize = 2048;
const CHUNK: usize = 16 * 1024;
const MAX_WATCHED: usize = 1024;
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// After a read-only caller times out, drain its FIFO barriers for at least the normal native
/// command budget. A short caller deadline must not turn healthy flood backlog into desync.
/// Longer configured deadlines retain their existing ten-times allowance; silent peers remain bounded.
const LATE_REPLY_FACTOR: u32 = 10;
/// Commands that only read tmux state: a timeout leaves nothing unknown, so it is not fatal.
const READ_ONLY: &[&str] = &[
    "list-panes", "list-windows", "list-sessions", "list-clients", "list-buffers", "list-keys", "list-commands",
    "display-message", "capture-pane", "show-options", "show-window-options", "show-environment", "show-hooks",
    "show-buffer", "has-session",
];

fn read_only(command: &str) -> bool {
    READ_ONLY.contains(&command)
}
type Reply = Result<Vec<Vec<u8>>, Error>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(String),
    Rejected(Vec<Vec<u8>>),
    Disconnected,
    /// The command was written, but its acknowledgement was lost. Never automatically resend.
    Indeterminate(String),
    /// A read-only command got no reply in time. It had no effect; the control client stays
    /// attached and absorbs the late reply, so later replies stay paired.
    TimedOut(String),
    Protocol(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tmux: {self:?}")
    }
}
impl std::error::Error for Error {}
impl From<parser::ParseError> for Error {
    fn from(e: parser::ParseError) -> Self {
        Self::Protocol(e.0)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// An explicit absolute private socket path; this client never uses the default tmux server.
    pub socket: PathBuf,
    pub session: String,
    pub binary: PathBuf,
    pub pause_after: u32,
    pub command_timeout: Duration,
    /// Bounded event slots per pane. Data is split into at most 16 KiB chunks.
    pub pane_capacity: usize,
}
impl Config {
    pub fn new(socket: PathBuf, session: impl Into<String>) -> Self {
        Self {
            socket,
            session: session.into(),
            binary: "tmux".into(),
            pause_after: 1,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            pane_capacity: 32,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GapReason {
    Paused,
    SubscriberLagged(u64),
    RecoveryRequested,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneEvent {
    /// Offset counts bytes actually observed on this attachment, not bytes lost by tmux.
    Data {
        offset: u64,
        bytes: Arc<[u8]>,
    },
    Osc7501(Arc<[u8]>),
    Gap(GapReason),
    /// Current visible rows from capture-pane -p -e, joined with CRLF. No scrollback/raw replay.
    Repaint {
        screen: Arc<[u8]>,
    },
    Continued,
    Closed,
}
pub struct PaneStream {
    rx: broadcast::Receiver<PaneEvent>,
}
impl PaneStream {
    pub async fn recv(&mut self) -> PaneEvent {
        match self.rx.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                self.rx = self.rx.resubscribe(); // Drop the stale tail; caller must request a repaint.
                PaneEvent::Gap(GapReason::SubscriberLagged(n))
            }
            Err(broadcast::error::RecvError::Closed) => PaneEvent::Closed,
        }
    }
}
struct Pane {
    tx: broadcast::Sender<PaneEvent>,
    offset: u64,
    osc: Osc7501,
    recovering: bool,
}
type Panes = Arc<Mutex<BTreeMap<PaneId, Pane>>>;
struct Request {
    command: String,
    marker: String,
    flow: Vec<(PaneId, bool)>,
    repaint: Option<PaneId>,
    /// Taken when answered, or when a read-only command times out (its late reply is absorbed).
    reply: Option<oneshot::Sender<Reply>>,
    /// Safe to abandon on timeout: tmux commands that only read state.
    read_only: bool,
}
enum Action {
    Command(Request),
    Close(oneshot::Sender<()>),
}

/// Cloneable command handle. Dropping every handle terminates only the control client, never server.
#[derive(Clone)]
pub struct Client {
    commands: mpsc::Sender<Action>,
    panes: Panes,
    notifications: broadcast::Sender<Notification>,
    pane_capacity: usize,
    session: String,
    instance: String,
    config: Arc<Config>,
    recovery: Arc<tokio::sync::Mutex<()>>,
}
impl Client {
    pub async fn connect(config: Config) -> Result<Self, Error> {
        if !config.socket.is_absolute()
            || config.socket.as_os_str().len() >= 103
            || config.session.is_empty()
            || config.pane_capacity == 0
            || config.pane_capacity > 1024
            || config.pause_after == 0
            || config.command_timeout.is_zero()
        {
            return Err(Error::Invalid(
                "absolute socket under 103 bytes, session, positive timeout/pause-after and pane capacity 1..1024 required".into(),
            ));
        }
        let mut cmd = tokio::process::Command::new(&config.binary);
        cmd.env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()));
        if let Some(home) = std::env::var_os("HOME") {
            cmd.env("HOME", home);
        }
        cmd.args(["-N", "-S"])
            .arg(&config.socket)
            .args(["-f", "/dev/null", "-C", "attach-session", "-t"])
            .arg(&config.session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| Error::Invalid(format!("cannot start control client: {e}")))?;
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (records_tx, records_rx) = mpsc::channel(16);
        let reader = tokio::spawn(read_records(stdout, records_tx));
        let (commands, requests) = mpsc::channel(32);
        let (ready_tx, ready_rx) = oneshot::channel();
        let panes = Arc::new(Mutex::new(BTreeMap::new()));
        let (notifications, _) = broadcast::channel(128);
        tokio::spawn(actor(
            child,
            stdin,
            records_rx,
            requests,
            ready_tx,
            panes.clone(),
            notifications.clone(),
            config.command_timeout,
            reader,
        ));
        ready_rx.await.map_err(|_| Error::Disconnected)??;
        let mut client = Self {
            commands,
            panes,
            notifications,
            pane_capacity: config.pane_capacity,
            session: config.session.clone(),
            instance: String::new(),
            config: Arc::new(config.clone()),
            recovery: Arc::new(tokio::sync::Mutex::new(())),
        };
        // A server-only nonce survives reconnects, but not server restart (unlike reused %N ids).
        // Native set-option -o performs the conditional assignment atomically on the server.
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| Error::Invalid(e.to_string()))?;
        let nonce: String = random.iter().map(|b| format!("{b:02x}")).collect();
        match client.command(&["set-option", "-s", "-o", "@vqro_control_instance", &nonce]).await {
            Ok(_) => {}
            Err(Error::Rejected(lines)) if lines == [b"already set: @vqro_control_instance".to_vec()] => {}
            Err(e) => return Err(e),
        }
        let identity = client.command(&["display-message", "-p", "#{pid}:#{start_time}:#{@vqro_control_instance}"]).await?;
        client.instance = String::from_utf8(identity.into_iter().next().ok_or_else(|| Error::Protocol("empty server identity".into()))?)
            .map_err(|_| Error::Protocol("invalid server identity".into()))?;
        if !client.instance.ends_with(&nonce)
            && client.instance.rsplit(':').next().is_none_or(|n| n.len() != 32 || !n.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(Error::Protocol("missing server-lifetime nonce".into()));
        }
        client.command(&["refresh-client", "-f", &format!("pause-after={}", config.pause_after)]).await?;
        Ok(client)
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }
    /// Lagged notifications require the caller to list/query current tmux facts; no event replay exists.
    pub fn notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }
    pub fn watch(&self, pane: PaneId) -> Result<PaneStream, Error> {
        if self.commands.is_closed() {
            return Err(Error::Disconnected);
        }
        let mut panes = self.panes.lock().unwrap();
        panes.retain(|_, p| p.tx.receiver_count() > 0);
        if !panes.contains_key(&pane) && panes.len() == MAX_WATCHED {
            return Err(Error::Invalid("too many watched panes".into()));
        }
        let p = panes.entry(pane).or_insert_with(|| {
            let (tx, _) = broadcast::channel(self.pane_capacity);
            Pane { tx, offset: 0, osc: Osc7501::default(), recovering: false }
        });
        Ok(PaneStream { rx: p.tx.subscribe() })
    }
    pub fn unwatch(&self, pane: &PaneId) {
        self.panes.lock().unwrap().remove(pane);
    }
    /// Arguments are quoted for tmux's parser, not a shell. Newlines/NUL and oversized commands fail locally.
    pub async fn command(&self, args: &[&str]) -> Reply {
        self.request(args, None).await
    }
    async fn request(&self, args: &[&str], repaint: Option<PaneId>) -> Reply {
        if args.is_empty() {
            return Err(Error::Invalid("empty command".into()));
        }
        let command = args.iter().map(|s| quote(s)).collect::<Result<Vec<_>, _>>()?.join(" ");
        if command.len() > MAX_COMMAND {
            return Err(Error::Invalid("command exceeds 64 KiB".into()));
        }
        let mut token = [0u8; 16];
        getrandom::fill(&mut token).map_err(|e| Error::Invalid(e.to_string()))?;
        let marker = format!("vqro_barrier_{}", token.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let (tx, rx) = oneshot::channel();
        let flow = if args[0] == "refresh-client" {
            args.windows(2)
                .filter(|pair| pair[0] == "-A")
                .filter_map(|pair| {
                    let (pane, action) = pair[1].split_once(':')?;
                    let pause = match action {
                        "pause" => true,
                        "continue" => false,
                        _ => return None,
                    };
                    Some((PaneId::parse(pane).ok()?, pause))
                })
                .collect()
        } else {
            Vec::new()
        };
        self.commands
            .send(Action::Command(Request { command, marker, flow, repaint, reply: Some(tx), read_only: read_only(args[0]) }))
            .await
            .map_err(|_| Error::Disconnected)?;
        rx.await.unwrap_or(Err(Error::Indeterminate("connection closed before command acknowledgement".into())))
    }
    /// New native window with one pane, linked into this client's session. tmux owns the process.
    pub async fn spawn(&self, argv: &[&str], cwd: Option<&Path>) -> Result<PaneId, Error> {
        if argv.is_empty() {
            return Err(Error::Invalid("spawn requires argv".into()));
        }
        let mut args = vec!["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", self.session.as_str()];
        let cwd = cwd.map(|p| p.to_str().ok_or_else(|| Error::Invalid("cwd is not UTF8".into()))).transpose()?;
        if let Some(cwd) = cwd {
            args.extend(["-c", cwd]);
        }
        args.push("--");
        args.extend_from_slice(argv);
        let lines = self.command(&args).await?;
        PaneId::parse(
            std::str::from_utf8(lines.first().ok_or_else(|| Error::Protocol("spawn returned no pane".into()))?)
                .map_err(|_| Error::Protocol("non-UTF8 pane id".into()))?,
        )
        .map_err(Into::into)
    }
    pub async fn input(&self, pane: &PaneId, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > MAX_INPUT {
            return Err(Error::Invalid("input exceeds 16 KiB; caller must bound/chunk explicitly".into()));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        // One `send-keys -H` argument per byte. tmux's command parser fails with "yacc stack overflow"
        // somewhere above 8 000 arguments (tmux 3.7b), so larger input is sent as several commands,
        // in order. If one fails, the error says how much was already delivered; nothing is resent.
        let mut sent = 0;
        for part in bytes.chunks(SEND_KEYS_BYTES) {
            let hex: Vec<_> = part.iter().map(|b| format!("{b:02x}")).collect();
            let mut args = vec!["send-keys", "-t", pane.as_str(), "-H"];
            args.extend(hex.iter().map(String::as_str));
            match self.command(&args).await {
                Ok(_) => sent += part.len(),
                Err(Error::Rejected(mut lines)) if sent > 0 => {
                    lines.insert(0, format!("input partly delivered: {sent} of {} bytes before tmux refused the rest", bytes.len()).into_bytes());
                    return Err(Error::Rejected(lines));
                }
                Err(Error::Indeterminate(why)) if sent > 0 => {
                    return Err(Error::Indeterminate(format!("{why} (after {sent} of {} bytes were delivered)", bytes.len())));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    /// Native pane layout resize; tmux may constrain it to the containing window.
    pub async fn resize_pane(&self, pane: &PaneId, cols: u16, rows: u16) -> Result<(), Error> {
        dimensions(cols, rows)?;
        self.command(&["resize-pane", "-t", pane.as_str(), "-x", &cols.to_string(), "-y", &rows.to_string()]).await.map(|_| ())
    }
    /// Native per-window control-client size; arbitration with other clients belongs to tmux.
    pub async fn resize_window(&self, window: &str, cols: u16, rows: u16) -> Result<(), Error> {
        dimensions(cols, rows)?;
        if !window.starts_with('@') || window.len() < 2 || !window[1..].bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Invalid("invalid window id".into()));
        }
        self.command(&["refresh-client", "-C", &format!("{window}:{cols}x{rows}")]).await.map(|_| ())
    }
    pub async fn kill(&self, pane: &PaneId) -> Result<(), Error> {
        self.command(&["kill-pane", "-t", pane.as_str()]).await?;
        self.unwatch(pane);
        Ok(())
    }
    /// `target` is tmux's native subscription selector (%*, @*, %N, etc.); None removes it.
    pub async fn subscribe_format(&self, name: &str, target: Option<(&str, &str)>) -> Result<(), Error> {
        if name.is_empty() || name.contains(':') {
            return Err(Error::Invalid("invalid subscription name".into()));
        }
        let value = match target {
            Some((target, format)) => format!("{name}:{target}:{format}"),
            None => name.into(),
        };
        self.command(&["refresh-client", "-B", &value]).await.map(|_| ())
    }
    /// Capture visible screen and emit Repaint in protocol order, before acknowledging this call.
    pub async fn repaint(&self, pane: &PaneId) -> Result<(), Error> {
        self.request(&["capture-pane", "-p", "-e", "-t", pane.as_str()], Some(pane.clone())).await.map(|_| ())
    }
    /// Explicit native-pause recovery. Fence stale raw output, capture through an independent
    /// scoped tmux client (paused control replies may not flush), repaint, then continue. Returns
    /// only after %continue, so subsequent input cannot race the stale pre-continue output tail.
    pub async fn resume(&self, pane: &PaneId) -> Result<(), Error> {
        let _serial = self.recovery.lock().await;
        if self.commands.is_closed() {
            return Err(Error::Disconnected);
        }
        let mut notes = self.notifications();
        if let Some(p) = self.panes.lock().unwrap().get_mut(pane) {
            p.recovering = true;
            p.osc.reset();
            let _ = p.tx.send(PaneEvent::Gap(GapReason::RecoveryRequested));
        }
        let screen = self.capture_paused(pane).await?;
        if let Some(p) = self.panes.lock().unwrap().get_mut(pane) {
            let _ = p.tx.send(PaneEvent::Repaint { screen: screen.into() });
        }
        self.command(&["refresh-client", "-A", &format!("{pane}:continue")]).await?;
        tokio::time::timeout(self.config.command_timeout, async {
            loop {
                match notes.recv().await {
                    Ok(Notification::Continue(p)) if p == *pane => return Ok(()),
                    Ok(Notification::Exit(_) | Notification::Disconnected { .. }) | Err(_) => {
                        return Err(Error::Indeterminate("lost continuation notification".into()));
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or(Err(Error::Indeterminate("continuation not observed; pane may not have been paused".into())))
    }
    async fn capture_paused(&self, pane: &PaneId) -> Result<Vec<u8>, Error> {
        let nonce = self.instance.rsplit(':').next().unwrap();
        let mismatch = format!("vqro_wrong_instance_{nonce}");
        let capture = format!("capture-pane -p -e -t {}", quote(pane.as_str())?);
        // Both identity check and capture run on the SAME server command queue. Never address a
        // reused %N on a replacement server merely because its socket path has been recreated.
        let mut cmd = tokio::process::Command::new(&self.config.binary);
        cmd.env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()));
        if let Some(home) = std::env::var_os("HOME") {
            cmd.env("HOME", home);
        }
        cmd.args(["-N", "-S"])
            .arg(&self.config.socket)
            .args([
                "-f",
                "/dev/null",
                "if-shell",
                "-F",
                &format!("#{{==:#{{@vqro_control_instance}},{nonce}}}"),
                &capture,
                &format!("display-message -p {mismatch}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| Error::Invalid(e.to_string()))?;
        let mut stdout = child.stdout.take().unwrap().take(4 * parser::MAX_LINE as u64 + 1);
        let mut stderr = child.stderr.take().unwrap().take(8193);
        let result = tokio::time::timeout(self.config.command_timeout, async {
            let mut output = Vec::new();
            let mut errors = Vec::new();
            let (out, err, status) = tokio::join!(stdout.read_to_end(&mut output), stderr.read_to_end(&mut errors), child.wait());
            out.map_err(|e| Error::Protocol(e.to_string()))?;
            err.map_err(|e| Error::Protocol(e.to_string()))?;
            if output.len() > 4 * parser::MAX_LINE || errors.len() > 8192 {
                return Err(Error::Protocol("capture exceeds bounds".into()));
            }
            if !status.map_err(|e| Error::Protocol(e.to_string()))?.success() {
                return Err(Error::Rejected(vec![errors]));
            }
            if output == format!("{mismatch}\n").as_bytes() {
                return Err(Error::Disconnected);
            }
            if output.last() == Some(&b'\n') {
                output.pop();
            }
            Ok(output.split(|b| *b == b'\n').collect::<Vec<_>>().join(&b"\r\n"[..]))
        })
        .await
        .unwrap_or(Err(Error::Indeterminate("capture timed out".into())));
        // Also reap on timeout/overflow. This is only our short-lived CLI client, not the server.
        let _ = child.start_kill();
        let _ = child.wait().await;
        result
    }
    /// Detach this connection (all clones), without killing the server or pane processes.
    pub async fn close(&self) {
        let (tx, rx) = oneshot::channel();
        if self.commands.send(Action::Close(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}
fn dimensions(cols: u16, rows: u16) -> Result<(), Error> {
    if cols == 0 || rows == 0 { Err(Error::Invalid("zero terminal size".into())) } else { Ok(()) }
}
fn quote(s: &str) -> Result<String, Error> {
    if s.bytes().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
        return Err(Error::Invalid("control argument contains newline or NUL".into()));
    }
    Ok(format!("'{}'", s.replace('\'', "'\\''")))
}
async fn read_records(mut stdout: tokio::process::ChildStdout, tx: mpsc::Sender<Result<Record, Error>>) {
    let mut parser = Parser::default();
    let mut buf = [0u8; 8192];
    loop {
        let result = match stdout.read(&mut buf).await {
            Ok(0) => {
                let _ = tx.send(Err(parser.finish().err().map(Error::from).unwrap_or(Error::Disconnected))).await;
                return;
            }
            Ok(n) => parser.feed(&buf[..n]).map_err(Into::into),
            Err(e) => Err(Error::Protocol(e.to_string())),
        };
        match result {
            Ok(records) => {
                for r in records {
                    if tx.send(Ok(r)).await.is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        }
    }
}

// The actor serializes commands. Dropped caller futures do not drop the pending block or shift
// another command's reply; mutating timeout, failed bounded drain or disconnect terminates this
// attachment. Never retry an effect.
#[allow(clippy::too_many_arguments)]
async fn actor(
    mut child: tokio::process::Child,
    mut stdin: tokio::process::ChildStdin,
    mut records: mpsc::Receiver<Result<Record, Error>>,
    mut requests: mpsc::Receiver<Action>,
    ready: oneshot::Sender<Result<(), Error>>,
    panes: Panes,
    notifications: broadcast::Sender<Notification>,
    timeout: Duration,
    reader: tokio::task::JoinHandle<()>,
) {
    let initial = tokio::time::timeout(timeout, async {
        loop {
            match records.recv().await.ok_or(Error::Disconnected)?? {
                Record::Block { error, lines, .. } => return if error { Err(Error::Rejected(lines)) } else { Ok(()) },
                record => dispatch(record, &panes, &notifications),
            }
        }
    })
    .await
    .unwrap_or(Err(Error::Disconnected));
    let initialized = initial.is_ok();
    let _ = ready.send(initial);
    let mut pending: Option<Request> = None;
    let mut started = false;
    let mut first: Option<Reply> = None;
    let mut deadline = tokio::time::Instant::now() + timeout;
    let mut close = None;
    let mut reason = "control connection closed".to_string();
    if initialized {
        loop {
            tokio::select! {
                action = requests.recv(), if pending.is_none() => {
                    match action {
                        Some(Action::Command(request)) => {
                            // Install pending before the write: even a partial failed write is indeterminate.
                            // Native commands and synchronous hooks may each produce extra blocks.
                            // Random begin/end barriers fence the whole request, including cancelled callers.
                            let command = format!("display-message -p {}_begin\n{}\ndisplay-message -p {}_end\n", request.marker, request.command, request.marker);
                            pending = Some(request);
                            started = false;
                            first = None;
                            let write = async { stdin.write_all(command.as_bytes()).await?; stdin.flush().await };
                            if !matches!(tokio::time::timeout(timeout, write).await, Ok(Ok(()))) { reason = "command write failed or timed out".into(); break; }
                            deadline = tokio::time::Instant::now() + timeout;
                        }
                        Some(Action::Close(reply)) => { close = Some(reply); break; }
                        None => break,
                    }
                }
                record = records.recv() => match record {
                    Some(Ok(Record::Block { error, lines, .. })) => {
                        let Some(request) = pending.as_ref() else { continue; };
                        if !error && lines == [format!("{}_begin", request.marker).into_bytes()] {
                            started = true;
                        } else if !error && lines == [format!("{}_end", request.marker).into_bytes()] {
                            let request = pending.take().unwrap();
                            let result = first.take().unwrap_or(Err(Error::Protocol("missing command block between barriers".into())));
                            if let Some(reply) = request.reply {
                                let _ = reply.send(result);
                            }
                        } else if started && first.is_none() {
                            // tmux 3.7b emits manual %pause/%continue *inside* refresh-client's reply
                            // block, despite the manual's general no-notifications-inside-block rule.
                            // Interpret ONLY exact replies to our matching -A action; arbitrary capture
                            // or command output that happens to start with '%' remains opaque.
                            if !error {
                                for (pane, pause) in &request.flow {
                                    let kind = if *pause { "%pause" } else { "%continue" };
                                    if lines.iter().any(|line| line == format!("{kind} {pane}").as_bytes()) {
                                        dispatch(Record::Notification(if *pause { Notification::Pause(pane.clone()) } else { Notification::Continue(pane.clone()) }), &panes, &notifications);
                                    }
                                }
                            }
                            // Insert repaint at the capture block itself, not at the later barrier:
                            // raw output after capture must remain after the repaint in subscriber order.
                            if !error {
                                if let Some(pane) = &request.repaint {
                                    if let Some(p) = panes.lock().unwrap().get_mut(pane) {
                                        p.osc.reset();
                                        let screen = lines.join(&b"\r\n"[..]);
                                        let _ = p.tx.send(PaneEvent::Repaint { screen: screen.into() });
                                    }
                                }
                            }
                            first = Some(if error { Err(Error::Rejected(lines)) } else { Ok(lines) });
                        }
                    }
                    Some(Ok(record)) => {
                        let exit = matches!(&record, Record::Notification(Notification::Exit(_)));
                        dispatch(record, &panes, &notifications);
                        if exit { break; }
                    }
                    Some(Err(e)) => { reason = e.to_string(); break; }
                    None => break,
                },
                _ = tokio::time::sleep_until(deadline), if pending.is_some() => {
                    let request = pending.as_mut().unwrap();
                    if request.read_only && request.reply.is_some() {
                        // No effect to be unsure about: tell the caller, keep the attachment, and
                        // keep this request at the head so its late blocks are absorbed, not paired
                        // with the next command. Bounded: no reply at all means desync.
                        let _ = request.reply.take().unwrap().send(Err(Error::TimedOut("read-only command timed out; no effect".into())));
                        deadline = tokio::time::Instant::now() + (timeout * LATE_REPLY_FACTOR).max(DEFAULT_COMMAND_TIMEOUT);
                        continue;
                    }
                    reason = if request.reply.is_none() { "late reply never arrived".into() } else { "command acknowledgement timed out".into() };
                    break;
                },
            }
        }
    }
    if let Some(reply) = pending.and_then(|r| r.reply) {
        let _ = reply.send(Err(Error::Indeterminate(reason.clone())));
    }
    reader.abort();
    drop(stdin);
    let _ = child.start_kill();
    let _ = child.wait().await;
    panes.lock().unwrap().clear();
    let _ = notifications.send(Notification::Disconnected { reason });
    if let Some(reply) = close {
        let _ = reply.send(());
    }
}
fn dispatch(record: Record, panes: &Panes, notifications: &broadcast::Sender<Notification>) {
    match record {
        Record::Output { pane, bytes, .. } => {
            let mut panes = panes.lock().unwrap();
            let Some(p) = panes.get_mut(&pane) else {
                return;
            };
            if p.tx.receiver_count() == 0 {
                panes.remove(&pane);
                return;
            }
            if p.recovering {
                p.offset += bytes.len() as u64;
                return;
            }
            for bytes in bytes.chunks(CHUNK) {
                let offset = p.offset;
                p.offset += bytes.len() as u64;
                let _ = p.tx.send(PaneEvent::Data { offset, bytes: bytes.into() });
                for status in p.osc.feed(bytes) {
                    let _ = p.tx.send(PaneEvent::Osc7501(status.into()));
                }
            }
        }
        Record::Notification(note) => {
            match &note {
                Notification::Pause(pane) | Notification::Continue(pane) => {
                    if let Some(p) = panes.lock().unwrap().get_mut(pane) {
                        p.osc.reset();
                        if matches!(note, Notification::Pause(_)) {
                            // A delayed pause note must not invalidate the already published recovery repaint.
                            if !p.recovering {
                                let _ = p.tx.send(PaneEvent::Gap(GapReason::Paused));
                            }
                        } else {
                            p.recovering = false;
                            let _ = p.tx.send(PaneEvent::Continued);
                        }
                    }
                }
                _ => {}
            }
            let _ = notifications.send(note);
        }
        Record::Block { .. } => {}
    }
}
