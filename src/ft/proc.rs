//! Spawning and supervising FreeToken child processes.
//!
//! ft-man owns the `ft serve` process directly rather than going through `ft daemon`,
//! so it works against a plain `pip install freetoken` with nothing else running. Three
//! things make that safe:
//!
//! * the child gets its own process group, so a Ctrl-C aimed at ft-man does not race
//!   the engine's own shutdown;
//! * stdout and stderr are merged into a bounded ring the Logs view tails, and also
//!   appended to a file under the state directory so a crash is still diagnosable after
//!   ft-man exits;
//! * a state file records `{pid, starttime, args}` so a restarted ft-man can re-adopt a
//!   serve it started earlier instead of orphaning it. `starttime` from `/proc` makes
//!   re-adoption safe against PID reuse.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::locate::Freetoken;

// ---------------------------------------------------------------- log ring

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    /// Position in the stream of every line ever pushed, starting at 1. Never reused and
    /// never renumbered, so a client that fetches by sequence can tell a gap from a pause.
    pub seq: u64,
    pub text: String,
    /// True for lines that arrived on stderr.
    pub err: bool,
}

/// What a ring currently holds, for a reader deciding whether it has missed anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct RingStats {
    pub count: usize,
    /// Sequence of the oldest retained line; equal to `last_seq` when nothing is retained.
    pub first_seq: u64,
    /// Sequence of the newest line ever pushed; 0 before the first one.
    pub last_seq: u64,
    /// Lines evicted by the ring or discarded by a clear, since the process started.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct Ring {
    lines: VecDeque<LogLine>,
    /// Total pushed, which is also the newest sequence number.
    pushed: u64,
    dropped: u64,
}

/// A bounded, shared ring of child output. The reader task appends; the UI reads a
/// snapshot each frame.
#[derive(Clone)]
pub struct LogRing {
    inner: Arc<Mutex<Ring>>,
    capacity: usize,
}

impl LogRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Ring {
                lines: VecDeque::with_capacity(capacity.min(1024)),
                ..Default::default()
            })),
            capacity,
        }
    }

    pub fn push(&self, text: String, err: bool) {
        let mut ring = self.inner.lock().unwrap();
        if ring.lines.len() == self.capacity {
            ring.lines.pop_front();
            ring.dropped += 1;
        }
        ring.pushed += 1;
        let seq = ring.pushed;
        ring.lines.push_back(LogLine { seq, text, err });
    }

    /// All retained lines, oldest first.
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.inner.lock().unwrap().lines.iter().cloned().collect()
    }

    /// Retained lines after `after`, oldest first, at most `limit` of them.
    pub fn since(&self, after: u64, limit: usize) -> Vec<LogLine> {
        let ring = self.inner.lock().unwrap();
        ring.lines.iter().filter(|l| l.seq > after).take(limit).cloned().collect()
    }

    pub fn stats(&self) -> RingStats {
        let ring = self.inner.lock().unwrap();
        RingStats {
            count: ring.lines.len(),
            first_seq: ring.lines.front().map(|l| l.seq).unwrap_or(ring.pushed),
            last_seq: ring.pushed,
            dropped: ring.dropped,
        }
    }

    /// Discard everything retained. The sequence keeps counting and `dropped` absorbs
    /// what went, so a reader holding a sequence can still tell where it stands.
    pub fn clear(&self) {
        let mut ring = self.inner.lock().unwrap();
        ring.dropped += ring.lines.len() as u64;
        ring.lines.clear();
    }
}

// ---------------------------------------------------------------- serve state

/// Persisted so a restarted ft-man can re-adopt a running engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeState {
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`, which makes the record PID-reuse-safe.
    pub starttime: u64,
    pub model: String,
    pub port: u16,
    pub args: Vec<String>,
    pub log_path: PathBuf,
    /// Unix seconds when ft-man started it.
    pub started_at: i64,
}

impl ServeState {
    pub fn load() -> Option<Self> {
        let raw = std::fs::read_to_string(crate::config::serve_state_path()).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        crate::config::write_atomic(&crate::config::serve_state_path(), &json)
    }

    pub fn clear() {
        let _ = std::fs::remove_file(crate::config::serve_state_path());
    }

    /// True when the recorded process is still alive *and* is the same process — the
    /// start time guards against a recycled PID belonging to something else entirely.
    pub fn is_alive(&self) -> bool {
        proc_starttime(self.pid).is_some_and(|t| t == self.starttime)
    }
}

/// Read field 22 (`starttime`) from `/proc/<pid>/stat`. The comm field can contain
/// spaces and parentheses, so split after the final `)` rather than tokenizing blindly.
pub fn proc_starttime(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    // After the comm field, `state` is field 3, so starttime (field 22) is index 19 here.
    rest.split_whitespace().nth(19)?.parse().ok()
}

// ---------------------------------------------------------------- engine supervisor

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineState {
    /// No engine started by, or adopted by, this ft-man.
    Stopped,
    /// Process spawned; weights are loading. Readiness comes from `/health`.
    Starting,
    Running,
    Stopping,
    /// The process exited. Carries the exit status for display.
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// A process we did not spawn, adopted from the state file.
    Adopted,
}

impl EngineState {
    pub fn is_live(&self) -> bool {
        matches!(self, EngineState::Starting | EngineState::Running | EngineState::Adopted)
    }
}

/// Events the supervisor task emits back to the UI.
#[derive(Debug)]
pub enum EngineEvent {
    Started { pid: u32 },
    Exited { code: Option<i32>, signal: Option<i32> },
}

pub struct Engine {
    pub state: EngineState,
    pub pid: Option<u32>,
    pub log: LogRing,
    pub log_path: Option<PathBuf>,
    pub command_line: Option<String>,
    pub model: Option<String>,
    pub port: Option<u16>,
    child: Option<Child>,
    events: mpsc::UnboundedSender<EngineEvent>,
    /// When a stop was requested, driving the signal escalation in [`Engine::poll`].
    stop_at: Option<std::time::Instant>,
    /// How far the escalation has gone: 0 = SIGINT sent, 1 = SIGTERM, 2 = SIGKILL.
    stop_stage: u8,
    /// `starttime` of an adopted process, so a recycled PID is not mistaken for it.
    adopted_starttime: Option<u64>,
    /// When re-adoption last looked at the state file. Cheap is not free, and [`poll`]
    /// runs five times a second.
    last_adopt_check: Option<std::time::Instant>,
    /// A live engine recorded in the state file that this supervisor does not own — one a
    /// second ft-man on the same machine started. Refreshed with the adoption check.
    foreign: Option<ServeState>,
}

/// How long a stopping engine gets at each stage before the next signal. FreeToken
/// unloads weights and reaps its backend workers on SIGINT, which on a large MoE model
/// takes real time, so the first grace period is generous.
const STOP_GRACE: Duration = Duration::from_secs(25);
const TERM_GRACE: Duration = Duration::from_secs(15);

impl Engine {
    pub fn new(log_capacity: usize, events: mpsc::UnboundedSender<EngineEvent>) -> Self {
        Self {
            state: EngineState::Stopped,
            pid: None,
            log: LogRing::new(log_capacity),
            log_path: None,
            command_line: None,
            model: None,
            port: None,
            child: None,
            events,
            stop_at: None,
            stop_stage: 0,
            adopted_starttime: None,
            last_adopt_check: None,
            foreign: None,
        }
    }

    /// Re-attach to an engine a previous ft-man run started, if it is still alive.
    pub fn adopt(&mut self) -> Option<ServeState> {
        let state = ServeState::load()?;
        if !state.is_alive() {
            ServeState::clear();
            return None;
        }
        self.state = EngineState::Adopted;
        self.pid = Some(state.pid);
        self.adopted_starttime = Some(state.starttime);
        self.model = Some(state.model.clone());
        self.port = Some(state.port);
        self.log_path = Some(state.log_path.clone());
        self.command_line = Some(format!("ft serve {}", state.args.join(" ")));
        self.log.push(
            format!(
                "[ft-man] re-attached to engine pid {} serving {} on port {}",
                state.pid, state.model, state.port
            ),
            false,
        );
        self.log.push(format!("[ft-man] earlier output is in {}", state.log_path.display()), false);
        // It is ours now, so it is no longer somebody else's.
        self.foreign = None;
        Some(state)
    }

    /// The live engine another process started, as of the last check. `None` when the only
    /// engine around is this supervisor's own, or when there is none.
    pub fn foreign(&self) -> Option<&ServeState> {
        self.foreign.as_ref()
    }

    /// Re-read the state file now rather than waiting for the next adoption check.
    ///
    /// `poll` does this once a second, which is fine for a status line and not fine for a
    /// decision: an engine started in the last second is exactly the race a start has to
    /// lose rather than win twice.
    pub fn refresh_foreign(&mut self) -> Option<&ServeState> {
        self.foreign = ServeState::load().filter(|s| s.is_alive() && Some(s.pid) != self.pid);
        // Settled and idle: take it, rather than merely reporting it.
        if self.foreign.is_some()
            && self.pid.is_none()
            && matches!(self.state, EngineState::Stopped | EngineState::Exited { .. })
        {
            self.adopt();
        }
        self.foreign.as_ref()
    }

    pub fn is_live(&self) -> bool {
        self.state.is_live()
    }

    /// Spawn `ft serve` with `args`. Returns the log file the run is being teed into.
    pub fn start(
        &mut self,
        ft: &Freetoken,
        args: Vec<String>,
        env: &[(String, String)],
        model: String,
        port: u16,
    ) -> Result<PathBuf> {
        anyhow::ensure!(!self.is_live(), "an engine is already running");

        let log_path = new_log_path("serve");
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let argv = ft.argv("serve", &args);
        let mut cmd = Command::new(&ft.program);
        cmd.args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        for (k, v) in env {
            cmd.env(k, v);
        }
        // Unbuffered Python output, so the Logs view is live rather than arriving in
        // 8 KiB bursts once a buffer fills.
        cmd.env("PYTHONUNBUFFERED", "1");
        detach_process_group(&mut cmd);

        let mut child =
            cmd.spawn().with_context(|| format!("spawning {}", ft.program.display()))?;
        let pid = child.id().context("child exited before it could be identified")?;

        let display = shell_words::join(
            std::iter::once(ft.display_program().as_str()).chain(argv.iter().map(String::as_str)),
        );

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok()
            .map(|f| Arc::new(Mutex::new(f)));

        // The header goes into the file too. A log that does not say which command wrote
        // it is far less useful hours later, when the question is which model failed.
        for line in [
            format!("[ft-man] $ {display}"),
            format!("[ft-man] pid {pid}, logging to {}", log_path.display()),
        ] {
            write_line(&file, &line);
            self.log.push(line, false);
        }

        if let Some(out) = child.stdout.take() {
            spawn_reader(out, self.log.clone(), file.clone(), false);
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, self.log.clone(), file.clone(), true);
        }

        if let Some(starttime) = proc_starttime(pid) {
            let state = ServeState {
                pid,
                starttime,
                model: model.clone(),
                port,
                args: args.clone(),
                log_path: log_path.clone(),
                started_at: chrono::Utc::now().timestamp(),
            };
            state.save().ok();
        }

        self.state = EngineState::Starting;
        self.pid = Some(pid);
        self.model = Some(model);
        self.port = Some(port);
        self.log_path = Some(log_path.clone());
        self.command_line = Some(display);
        self.child = Some(child);
        let _ = self.events.send(EngineEvent::Started { pid });
        Ok(log_path)
    }

    /// Ask the engine to stop. SIGINT first, because FreeToken's own handler drains and
    /// reaps its backend workers on that signal; the escalation to SIGTERM and SIGKILL
    /// is driven by [`Engine::poll`] once the grace period elapses.
    pub fn stop(&mut self, force: bool) {
        let Some(pid) = self.pid else { return };
        if !self.is_live() {
            return;
        }
        let sig = if force { libc::SIGKILL } else { libc::SIGINT };
        signal_group(pid, sig);
        self.state = EngineState::Stopping;
        self.stop_at = Some(std::time::Instant::now());
        self.stop_stage = if force { 2 } else { 0 };
        self.log.push(
            format!(
                "[ft-man] sent {} to process group {pid}",
                if force { "SIGKILL" } else { "SIGINT" }
            ),
            false,
        );
    }

    /// Escalate SIGINT to SIGTERM and then SIGKILL when a stop does not take.
    ///
    /// Without this a wedged engine sits in `Stopping` forever, holding VRAM and the
    /// port, with no way out but killing it by hand.
    fn escalate_stop(&mut self) {
        if self.state != EngineState::Stopping {
            return;
        }
        let (Some(since), Some(pid)) = (self.stop_at, self.pid) else { return };
        let Some((stage, signal, why)) = next_stop_signal(self.stop_stage, since.elapsed()) else {
            return;
        };
        self.stop_stage = stage;
        signal_group(pid, signal);
        self.log.push(format!("[ft-man] {why}"), true);
    }

    /// Called every tick: reap the child if it exited, notice when an adopted engine goes
    /// away, and pick up an engine another process started.
    pub fn poll(&mut self) -> bool {
        let before = (self.state.clone(), self.pid, self.foreign.as_ref().map(|s| s.pid));
        self.escalate_stop();
        self.readopt();
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = status.code();
                    let signal = unix_signal(&status);
                    self.child = None;
                    self.pid = None;
                    self.stop_at = None;
                    self.stop_stage = 0;
                    self.state = EngineState::Exited { code, signal };
                    ServeState::clear();
                    self.log.push(
                        format!("[ft-man] engine exited: {}", describe_exit(code, signal)),
                        true,
                    );
                    let _ = self.events.send(EngineEvent::Exited { code, signal });
                }
                Ok(None) => {}
                Err(e) => {
                    self.log.push(format!("[ft-man] failed to reap the engine: {e}"), true);
                    self.child = None;
                }
            }
        } else if self.pid.is_some() {
            // An adopted engine is not our child, so there is no exit status to reap;
            // liveness is `/proc`. Compare the start time too, or a recycled PID would
            // read as the engine still running.
            if let Some(pid) = self.pid {
                let alive = match (proc_starttime(pid), self.adopted_starttime) {
                    (Some(now), Some(then)) => now == then,
                    (found, _) => found.is_some(),
                };
                if !alive {
                    self.pid = None;
                    self.stop_at = None;
                    self.stop_stage = 0;
                    self.state = EngineState::Exited { code: None, signal: None };
                    ServeState::clear();
                    self.log.push("[ft-man] the engine is gone".into(), true);
                }
            }
        }
        // Whether anything moved, so the web daemon can leave an idle machine alone
        // instead of publishing an identical snapshot five times a second.
        (self.state.clone(), self.pid, self.foreign.as_ref().map(|s| s.pid)) != before
    }

    /// Pick up an engine started elsewhere.
    ///
    /// A TUI and the web daemon can run side by side on one machine, and both are meant
    /// to drive the same engine rather than each seeing only what it started itself. The
    /// state file is the handoff: whichever process starts an engine writes it, and any
    /// other that has none adopts it on its next tick.
    ///
    /// Only from a settled idle state. `Stopping` is deliberately excluded — a stop this
    /// process just asked for has not cleared the file yet, and re-adopting the engine
    /// being killed would undo the request.
    fn readopt(&mut self) {
        // Once a second: reading a small file is cheap, but not five times a second
        // forever on an idle daemon.
        if self.last_adopt_check.is_some_and(|at| at.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.last_adopt_check = Some(std::time::Instant::now());
        // `refresh_foreign` both records and, from a settled idle state, adopts.
        // `Stopping` is deliberately excluded there — a stop this process just asked for
        // has not cleared the file yet, and re-adopting the engine being killed would undo
        // the request.
        if matches!(self.state, EngineState::Stopping) {
            return;
        }
        self.refresh_foreign();
    }

    /// Promote `Starting` to `Running` once `/health` reports readiness.
    pub fn mark_ready(&mut self) {
        if self.state == EngineState::Starting {
            self.state = EngineState::Running;
            self.log.push("[ft-man] engine is ready to serve".into(), false);
        }
    }

    /// Called on exit. The engine is a long-lived service and the state file lets a
    /// later run re-adopt it, so quitting ft-man deliberately leaves it running; only a
    /// stop the user actually asked for (which has already set `Stopping`) is carried
    /// through here.
    pub fn shutdown_blocking_if_requested(&mut self) {
        if self.state != EngineState::Stopping {
            return;
        }
        let Some(pid) = self.pid else { return };
        signal_group(pid, libc::SIGINT);
    }
}

/// The next signal in a stop escalation, given the stage already reached and how long
/// the engine has been asked to stop. `None` means keep waiting.
fn next_stop_signal(stage: u8, elapsed: Duration) -> Option<(u8, i32, String)> {
    match stage {
        0 if elapsed > STOP_GRACE => Some((
            1,
            libc::SIGTERM,
            format!("no exit after {}s; sent SIGTERM", STOP_GRACE.as_secs()),
        )),
        1 if elapsed > STOP_GRACE + TERM_GRACE => {
            Some((2, libc::SIGKILL, "still running; sent SIGKILL".to_string()))
        }
        _ => None,
    }
}

/// Append one line to an open log file. Failures are ignored: a log that cannot be
/// written must not take the run down with it.
fn write_line(file: &Option<Arc<Mutex<std::fs::File>>>, line: &str) {
    use std::io::Write;
    if let Some(f) = file {
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn spawn_reader<R>(reader: R, ring: LogRing, file: Option<Arc<Mutex<std::fs::File>>>, err: bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            write_line(&file, &line);
            ring.push(line, err);
        }
    });
}

/// Put the child in its own process group so signals sent to ft-man's group (a Ctrl-C
/// in the terminal that launched it) do not reach the engine, and so ft-man can signal
/// the whole engine tree — the API server plus its backend workers — at once.
fn detach_process_group(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                // Already a group leader: settle for a new process group.
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// Signal the child's whole process group, falling back to the bare PID if the group is
/// already gone.
pub fn signal_group(pid: u32, sig: i32) {
    unsafe {
        if libc::kill(-(pid as i32), sig) == -1 {
            libc::kill(pid as i32, sig);
        }
    }
}

fn unix_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

pub fn describe_exit(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(0), _) => "cleanly (status 0)".into(),
        (Some(c), _) => format!("with status {c}"),
        (_, Some(s)) => format!("on signal {s}"),
        _ => "for an unknown reason".into(),
    }
}

pub fn new_log_path(kind: &str) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    crate::config::log_dir().join(format!("{kind}-{stamp}.log"))
}

// ---------------------------------------------------------------- jobs

/// Which long-running FreeToken command a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// `ft checkpoint` — HF safetensors to FTW.
    Convert,
    /// `ft bench bw` — CPU vs PCIe bandwidth calibration.
    Bench,
    /// Pull the FreeToken checkout and reinstall it. Not an `ft` subcommand: the source
    /// tree is updated by git and the package manager that owns the venv.
    Update,
}

impl JobKind {
    pub fn label(self) -> &'static str {
        match self {
            JobKind::Convert => "convert",
            JobKind::Bench => "bench",
            JobKind::Update => "update",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Running,
    Done,
    Failed(String),
    Canceled,
}

impl JobStatus {
    /// The one word both front ends print in the status column.
    pub fn label(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed(_) => "failed",
            JobStatus::Canceled => "canceled",
        }
    }
}

// Written out rather than derived: serde cannot internally tag a newtype variant holding
// a plain String, and the wire format in docs/web-api.md names that payload `reason`.
impl Serialize for JobStatus {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        match self {
            JobStatus::Running => m.serialize_entry("kind", "running")?,
            JobStatus::Done => m.serialize_entry("kind", "done")?,
            JobStatus::Canceled => m.serialize_entry("kind", "canceled")?,
            JobStatus::Failed(reason) => {
                m.serialize_entry("kind", "failed")?;
                m.serialize_entry("reason", reason)?;
            }
        }
        m.end()
    }
}

/// Progress parsed out of a job's machine-readable output.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JobProgress {
    /// `dense`, `experts`, `finalize` for a convert; the current format for a bench.
    pub phase: String,
    pub done: u64,
    pub total: u64,
    /// Whether `done`/`total` are byte counts (convert) or step counts (bench).
    pub bytes: bool,
}

impl JobProgress {
    pub fn ratio(&self) -> Option<f64> {
        (self.total > 0).then(|| crate::util::ratio(self.done, self.total))
    }
}

/// Events a running job pushes to the UI.
#[derive(Debug)]
pub enum JobEvent {
    Progress(u64, JobProgress),
    /// The job produced a line of output; it is already in the job's own ring, so this
    /// exists only to wake the UI for a redraw.
    Line,
    /// A bench run wrote its profile here.
    Output(u64, PathBuf),
    Finished(u64, JobStatus),
}

pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    pub title: String,
    pub command_line: String,
    pub status: JobStatus,
    pub progress: JobProgress,
    pub log: LogRing,
    pub log_path: PathBuf,
    pub started_at: chrono::DateTime<chrono::Local>,
    pub finished_at: Option<chrono::DateTime<chrono::Local>>,
    /// Where a bench run wrote its profile, once it says so.
    pub output_path: Option<PathBuf>,
    /// Smoothed write rate, for phases that report no total.
    pub rate: crate::util::Ema,
    last_sample: Option<(std::time::Instant, u64)>,
    pid: Option<u32>,
}

impl Job {
    pub fn elapsed(&self) -> std::time::Duration {
        let end = self.finished_at.unwrap_or_else(chrono::Local::now);
        (end - self.started_at).to_std().unwrap_or_default()
    }

    pub fn is_running(&self) -> bool {
        self.status == JobStatus::Running
    }

    /// Fold a progress report in, updating the smoothed write rate.
    pub fn observe(&mut self, progress: JobProgress) {
        if progress.bytes {
            let now = std::time::Instant::now();
            match self.last_sample {
                Some((then, bytes)) => {
                    let dt = now.duration_since(then).as_secs_f64();
                    if dt >= 0.5 {
                        let delta = progress.done.saturating_sub(bytes) as f64;
                        self.rate.push(delta / dt);
                        self.last_sample = Some((now, progress.done));
                    }
                }
                None => self.last_sample = Some((now, progress.done)),
            }
        }
        self.progress = progress;
    }

    /// The most informative line the job printed before failing.
    ///
    /// `describe_exit` can only say "with status 1", which is true and useless. A Python
    /// traceback's last line is the thing worth reading, and leaving it buried in the
    /// output pane made a perfectly diagnosable failure look like a mystery.
    pub fn failure_reason(&self) -> Option<String> {
        let interesting = |l: &str| {
            !l.is_empty()
                && !l.starts_with("[ft-man]")
                && !l.starts_with("File \"")
                && !l.starts_with('^')
                && !l.starts_with('~')
                && !l.starts_with("Traceback")
                && !l.starts_with("During handling")
                && !l.starts_with("The above exception")
        };
        self.log
            .snapshot()
            .iter()
            .rev()
            .map(|l| l.text.trim().to_string())
            .find(|l| interesting(l))
            .map(|l| l.chars().take(300).collect())
    }

    pub fn cancel(&mut self) {
        if let Some(pid) = self.pid {
            if self.is_running() {
                signal_group(pid, libc::SIGINT);
                self.log.push("[ft-man] cancel requested (SIGINT)".into(), false);
            }
        }
    }
}

impl Job {
    /// A job in an arbitrary state, for tests that need populated UI state without
    /// spawning a real process.
    #[cfg(test)]
    pub fn fake(kind: JobKind, title: &str, status: JobStatus, progress: JobProgress) -> Self {
        let log = LogRing::new(64);
        log.push("[ft-man] $ ft ...".into(), false);
        log.push("loading weights".into(), false);
        Self {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            kind,
            title: title.into(),
            command_line: "ft ...".into(),
            status,
            progress,
            log,
            log_path: PathBuf::from("/tmp/ft-man-test.log"),
            started_at: chrono::Local::now(),
            finished_at: None,
            output_path: None,
            rate: crate::util::Ema::new(0.3),
            last_sample: None,
            pid: None,
        }
    }
}

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

/// What to run as a tracked job.
pub struct JobSpec<'a> {
    pub kind: JobKind,
    /// The `ft` subcommand, e.g. `["checkpoint"]` or `["bench", "bw"]`.
    pub subcommand: &'a [&'a str],
    pub args: Vec<String>,
    pub env: &'a [(String, String)],
    /// What to call this job in the UI.
    pub title: String,
    pub log_capacity: usize,
}

/// Spawn a FreeToken subcommand as a tracked job, streaming its progress protocol back
/// over `events`.
///
/// Both long commands speak a line protocol on stdout when their progress env var is
/// set: `FTCONVERT <phase> <done> <total>` and `FTBENCH <done> <total> <label>`, plus
/// `FTBENCH_OUT <path>` for the written profile. Parsing those is what lets ft-man show
/// a real progress bar instead of a spinner.
pub fn spawn_job(
    ft: &Freetoken,
    spec: JobSpec<'_>,
    events: mpsc::UnboundedSender<JobEvent>,
) -> Result<Job> {
    let JobSpec { kind, subcommand, args, env, title, log_capacity } = spec;
    let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    let log_path = new_log_path(kind.label());
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut argv: Vec<String> = ft.prefix.clone();
    argv.extend(subcommand.iter().map(|s| s.to_string()));
    argv.extend(args.iter().cloned());
    let display = shell_words::join(
        std::iter::once(ft.display_program().as_str()).chain(argv.iter().map(String::as_str)),
    );
    spawn_command(
        Run {
            program: ft.program.clone(),
            args: argv,
            cwd: None,
            display,
            kind,
            title,
            log_capacity,
        },
        env,
        id,
        log_path,
        events,
    )
}

/// A program to run as a tracked job, for work that is not an `ft` subcommand.
pub struct Run {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Working directory, when the command means something only inside one.
    pub cwd: Option<PathBuf>,
    /// The command as the log header should print it — which for a shell script wrapping
    /// two steps is what the operator would have typed, not the wrapper.
    pub display: String,
    pub kind: JobKind,
    pub title: String,
    pub log_capacity: usize,
}

/// Spawn `run` as a tracked job, streaming its output the way every job streams.
pub fn spawn_run(
    run: Run,
    env: &[(String, String)],
    events: mpsc::UnboundedSender<JobEvent>,
) -> Result<Job> {
    let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    let log_path = new_log_path(run.kind.label());
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    spawn_command(run, env, id, log_path, events)
}

fn spawn_command(
    run: Run,
    env: &[(String, String)],
    id: u64,
    log_path: PathBuf,
    events: mpsc::UnboundedSender<JobEvent>,
) -> Result<Job> {
    let Run { program, args, cwd, display, kind, title, log_capacity } = run;
    let argv = args;

    let mut cmd = Command::new(&program);
    cmd.args(&argv).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = &cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.env("PYTHONUNBUFFERED", "1");
    match kind {
        JobKind::Convert => {
            cmd.env("FREETOKEN_CONVERT_PROGRESS", "1");
        }
        JobKind::Bench => {
            cmd.env("FREETOKEN_BENCH_PROGRESS", "1");
        }
        // git and the package manager speak no progress protocol; their own output is the
        // progress, and it is already streamed to the Jobs tab line by line.
        JobKind::Update => {}
    }
    detach_process_group(&mut cmd);

    let mut child = cmd.spawn().with_context(|| format!("spawning {}", program.display()))?;
    let pid = child.id();

    let log = LogRing::new(log_capacity);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok()
        .map(|f| Arc::new(Mutex::new(f)));

    let header = format!("[ft-man] $ {display}");
    write_line(&file, &header);
    log.push(header, false);

    if let Some(out) = child.stdout.take() {
        spawn_job_reader(id, kind, out, log.clone(), file.clone(), false, events.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_job_reader(id, kind, err, log.clone(), file.clone(), true, events.clone());
    }

    let outcome_file = file.clone();
    tokio::spawn(async move {
        let status = match child.wait().await {
            Ok(s) => {
                if s.success() {
                    JobStatus::Done
                } else if unix_signal(&s) == Some(libc::SIGINT) {
                    JobStatus::Canceled
                } else {
                    JobStatus::Failed(describe_exit(s.code(), unix_signal(&s)))
                }
            }
            Err(e) => JobStatus::Failed(e.to_string()),
        };
        write_line(&outcome_file, &format!("[ft-man] finished: {status:?}"));
        let _ = events.send(JobEvent::Finished(id, status));
    });

    Ok(Job {
        id,
        kind,
        title,
        command_line: display,
        status: JobStatus::Running,
        progress: JobProgress::default(),
        log,
        log_path,
        started_at: chrono::Local::now(),
        finished_at: None,
        output_path: None,
        rate: crate::util::Ema::new(0.3),
        last_sample: None,
        pid,
    })
}

fn spawn_job_reader<R>(
    id: u64,
    kind: JobKind,
    reader: R,
    ring: LogRing,
    file: Option<Arc<Mutex<std::fs::File>>>,
    err: bool,
    events: mpsc::UnboundedSender<JobEvent>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            write_line(&file, &line);
            if let Some(path) = line.strip_prefix("FTBENCH_OUT ") {
                let _ = events.send(JobEvent::Output(id, PathBuf::from(path.trim())));
                continue;
            }
            if let Some(p) = parse_progress(kind, &line) {
                let _ = events.send(JobEvent::Progress(id, p));
                continue;
            }
            ring.push(line, err);
            let _ = events.send(JobEvent::Line);
        }
    });
}

/// `FTCONVERT <phase> <done> <total>` / `FTBENCH <done> <total> <label>`.
fn parse_progress(kind: JobKind, line: &str) -> Option<JobProgress> {
    match kind {
        JobKind::Convert => {
            let rest = line.strip_prefix("FTCONVERT ")?;
            let mut it = rest.split_whitespace();
            let phase = it.next()?.to_string();
            let done = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let total = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            Some(JobProgress { phase, done, total, bytes: true })
        }
        JobKind::Bench => {
            let rest = line.strip_prefix("FTBENCH ")?;
            let mut it = rest.splitn(3, char::is_whitespace);
            let done = it.next()?.parse().ok()?;
            let total = it.next()?.parse().ok()?;
            let phase = it.next().unwrap_or("").trim().to_string();
            Some(JobProgress { phase, done, total, bytes: false })
        }
        // No protocol to parse: git and uv report progress as ordinary output.
        JobKind::Update => None,
    }
}

/// The FTW marker file a converted checkpoint carries.
pub const FTW_INDEX: &str = "freetoken_weight.json";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_progress_parses_phase_and_bytes() {
        let p = parse_progress(JobKind::Convert, "FTCONVERT experts 1024 4096").unwrap();
        assert_eq!(p.phase, "experts");
        assert_eq!((p.done, p.total), (1024, 4096));
        assert_eq!(p.ratio(), Some(0.25));
    }

    #[test]
    fn convert_progress_tolerates_an_unknown_total() {
        let p = parse_progress(JobKind::Convert, "FTCONVERT dense 0 0").unwrap();
        assert_eq!(p.ratio(), None);
    }

    #[test]
    fn bench_progress_keeps_multiword_labels() {
        let p = parse_progress(JobKind::Bench, "FTBENCH 2 6 qwen3:NVFP4 kernels").unwrap();
        assert_eq!((p.done, p.total), (2, 6));
        assert_eq!(p.phase, "qwen3:NVFP4 kernels");
    }

    #[test]
    fn ordinary_output_is_not_mistaken_for_progress() {
        assert!(parse_progress(JobKind::Convert, "Converting dense weights: 12%").is_none());
        assert!(parse_progress(JobKind::Bench, "FTBENCHX 1 2 x").is_none());
    }

    #[test]
    fn a_failure_reason_is_the_error_not_the_traceback_scaffolding() {
        let job = Job::fake(JobKind::Convert, "t", JobStatus::Running, JobProgress::default());
        job.log.clear();
        for line in [
            "[ft-man] $ ft checkpoint --model /models/x",
            "Converting dense weights: 40%",
            "Traceback (most recent call last):",
            "  File \"/x/convert.py\", line 288, in load_moe_expert_sources",
            "    return stream_moe_expert_sources(",
            "    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^",
            "ValueError: Missing MoE expert source layers: {'gate_up': [0, 1]}",
        ] {
            job.log.push(line.into(), false);
        }
        assert_eq!(
            job.failure_reason().as_deref(),
            Some("ValueError: Missing MoE expert source layers: {'gate_up': [0, 1]}")
        );
    }

    #[test]
    fn a_job_that_printed_nothing_useful_has_no_reason_to_offer() {
        let job = Job::fake(JobKind::Bench, "t", JobStatus::Running, JobProgress::default());
        job.log.clear();
        job.log.push("[ft-man] $ ft bench bw".into(), false);
        job.log.push("   ".into(), false);
        assert_eq!(job.failure_reason(), None, "our own header is not a failure reason");
    }

    #[test]
    fn byte_progress_builds_a_rate_and_step_progress_does_not() {
        let mut job = Job::fake(JobKind::Convert, "t", JobStatus::Running, JobProgress::default());
        job.observe(JobProgress { phase: "dense".into(), done: 0, total: 0, bytes: true });
        // Two samples less than the sampling interval apart: not enough to rate yet.
        job.observe(JobProgress { phase: "dense".into(), done: 1 << 20, total: 0, bytes: true });
        assert_eq!(job.rate.get(), 0.0);
        assert_eq!(job.progress.done, 1 << 20);

        std::thread::sleep(Duration::from_millis(600));
        job.observe(JobProgress { phase: "dense".into(), done: 3 << 20, total: 0, bytes: true });
        assert!(job.rate.get() > 0.0, "a byte phase should report throughput");

        // A step-counted phase (the bench) has no bytes to rate.
        let mut bench = Job::fake(JobKind::Bench, "t", JobStatus::Running, JobProgress::default());
        bench.observe(JobProgress { phase: "nvfp4".into(), done: 1, total: 6, bytes: false });
        assert_eq!(bench.rate.get(), 0.0);
    }

    #[test]
    fn log_ring_drops_the_oldest_line_at_capacity() {
        let ring = LogRing::new(2);
        for i in 0..4 {
            ring.push(format!("line {i}"), false);
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].text, "line 2");
        assert_eq!(snap[1].text, "line 3");
    }

    /// A stand-in for the FreeToken CLI: `sh -c '<script>'`, with the subcommand and
    /// args landing in `$0`/`$@` where the script can ignore them.
    fn fake_cli(script: &str) -> Freetoken {
        Freetoken {
            program: PathBuf::from("/bin/sh"),
            prefix: vec!["-c".into(), script.into()],
            origin: "test".into(),
        }
    }

    /// Poll until `f` holds or the deadline passes.
    ///
    /// The wait must yield to the runtime rather than block it: the stdout and stderr
    /// readers are spawned tasks, so a blocking sleep here would starve them and the log
    /// would look empty no matter how long the test waited.
    async fn wait_until(engine: &mut Engine, secs: u64, f: impl Fn(&Engine) -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            engine.poll();
            if f(engine) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        engine.poll();
        f(engine)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_engine_starts_captures_output_and_stops() {
        crate::config::isolate_paths_for_tests();
        let _guard = crate::config::lock_serve_state().await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(64, tx);
        let ft = fake_cli("echo hello-from-engine; echo oops >&2; sleep 30");

        let log_path = engine
            .start(&ft, vec!["--model".into(), "x".into()], &[], "test-model".into(), 1919)
            .expect("the stand-in should spawn");
        assert_eq!(engine.state, EngineState::Starting);
        let pid = engine.pid.expect("a pid should be recorded");

        // The state file must describe a live process, so a later run can re-adopt it.
        let state = ServeState::load().expect("serve state should be written");
        assert_eq!(state.pid, pid);
        assert_eq!(state.model, "test-model");
        assert!(state.is_alive());

        // Both streams are captured, and teed to the log file.
        assert!(
            wait_until(&mut engine, 10, |e| {
                e.log.snapshot().iter().any(|l| l.text.contains("hello-from-engine"))
            })
            .await,
            "stdout was not captured"
        );
        assert!(
            wait_until(&mut engine, 10, |e| {
                e.log.snapshot().iter().any(|l| l.err && l.text.contains("oops"))
            })
            .await,
            "stderr was not captured"
        );
        let on_disk = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(on_disk.contains("hello-from-engine"), "log file was not written: {on_disk:?}");

        engine.stop(false);
        assert_eq!(engine.state, EngineState::Stopping);
        assert!(wait_until(&mut engine, 20, |e| !e.is_live() && e.pid.is_none()).await);
        assert!(matches!(engine.state, EngineState::Exited { .. }));
        // A stopped engine leaves nothing for a later run to re-adopt.
        assert!(ServeState::load().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_engine_that_exits_on_its_own_is_reaped_with_its_status() {
        crate::config::isolate_paths_for_tests();
        let _guard = crate::config::lock_serve_state().await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(16, tx);

        engine
            .start(&fake_cli("exit 3"), vec![], &[], "doomed".into(), 1919)
            .expect("the stand-in should spawn");
        assert!(
            wait_until(&mut engine, 10, |e| matches!(e.state, EngineState::Exited { .. })).await
        );
        assert_eq!(engine.state, EngineState::Exited { code: Some(3), signal: None });
        assert!(engine.log.snapshot().iter().any(|l| l.text.contains("with status 3")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_start_is_refused_while_one_is_running() {
        crate::config::isolate_paths_for_tests();
        let _guard = crate::config::lock_serve_state().await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(16, tx);
        let ft = fake_cli("sleep 30");

        engine.start(&ft, vec![], &[], "first".into(), 1919).unwrap();
        assert!(engine.start(&ft, vec![], &[], "second".into(), 1920).is_err());

        engine.stop(true);
        wait_until(&mut engine, 10, |e| !e.is_live()).await;
    }

    #[tokio::test]
    async fn a_stale_state_file_is_not_adopted() {
        crate::config::isolate_paths_for_tests();
        let _guard = crate::config::lock_serve_state().await;
        // PID 2^22 is above the default pid_max on Linux, so nothing can own it.
        ServeState {
            pid: 4_194_303,
            starttime: 1,
            model: "ghost".into(),
            port: 1919,
            args: vec![],
            log_path: PathBuf::from("/tmp/none.log"),
            started_at: 0,
        }
        .save()
        .unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(16, tx);
        assert!(engine.adopt().is_none(), "a dead pid must not be adopted");
        assert_eq!(engine.state, EngineState::Stopped);
        assert!(ServeState::load().is_none(), "the stale record should be cleared");
    }

    #[test]
    fn a_recorded_process_with_a_different_start_time_is_not_the_same_process() {
        // Our own pid, but a start time from another era: PID reuse, not our engine.
        let state = ServeState {
            pid: std::process::id(),
            starttime: 0,
            model: "x".into(),
            port: 1919,
            args: vec![],
            log_path: PathBuf::from("/tmp/none.log"),
            started_at: 0,
        };
        assert!(!state.is_alive());

        let real = ServeState { starttime: proc_starttime(std::process::id()).unwrap(), ..state };
        assert!(real.is_alive());
    }

    #[test]
    fn a_stop_escalates_from_sigint_to_sigterm_to_sigkill() {
        // Still inside the first grace period: no new signal.
        assert!(next_stop_signal(0, Duration::from_secs(1)).is_none());

        let (stage, sig, _) = next_stop_signal(0, STOP_GRACE + Duration::from_secs(1)).unwrap();
        assert_eq!((stage, sig), (1, libc::SIGTERM));

        // SIGTERM was just sent; it gets its own grace period before SIGKILL.
        assert!(next_stop_signal(1, STOP_GRACE + Duration::from_secs(1)).is_none());

        let late = STOP_GRACE + TERM_GRACE + Duration::from_secs(1);
        let (stage, sig, _) = next_stop_signal(1, late).unwrap();
        assert_eq!((stage, sig), (2, libc::SIGKILL));

        // Nothing follows SIGKILL.
        assert!(next_stop_signal(2, Duration::from_secs(3600)).is_none());
    }

    #[test]
    fn stopping_an_engine_that_is_not_running_does_nothing() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(16, tx);
        engine.stop(false);
        assert_eq!(engine.state, EngineState::Stopped);
        assert!(engine.stop_at.is_none());
        // And polling an engine with no process must not invent an exit.
        engine.poll();
        assert_eq!(engine.state, EngineState::Stopped);
    }

    #[test]
    fn our_own_starttime_is_readable_and_stable() {
        let pid = std::process::id();
        let a = proc_starttime(pid).expect("/proc/self/stat should parse");
        assert_eq!(proc_starttime(pid), Some(a));
    }
}
