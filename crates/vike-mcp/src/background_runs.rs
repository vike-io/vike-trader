//! Long runs (the CLI/MCP plan's G3): a run tool called with `background: true` answers a `run_id` at once,
//! `run_status` reports that run and, once it ended, its answer, and `cancel_run` stops it. A client's call limit
//! (Desktop and claude.ai cut a call at about 240 s) no longer loses a long sweep or walk-forward.
//!
//! ⚠ **A run lives in THIS PROCESS, and ends with it.** [`BackgroundRuns`] is a field of the `Server`, in memory, and
//! the backtesthub keeps no run whose client left: its peer-gone watch stops one whose connection closed
//! (`crates/vike-backtesthub/src/backtesthub_server/connection.rs`). So when the session ends, at EOF or otherwise,
//! every run still running is cancelled (`Drop`), and a run id means nothing to the next process: each id carries the
//! process's start instant, so an old id finds no new run. What a run's daemon SAVED (a sweep's `saved_run`)
//! outlives it, as it does a foreground call's.
//!
//! **No second runner.** A background run is the `DeferredCall` a foreground one is, run by the same
//! `super::protocol`'s `run_deferred` on a worker thread of its own, its daemon connection registered with a
//! [`CancelToken`] exactly as `notifications/cancelled` uses it. The worker touches no `Server` and writes no stdout:
//! it hands its outcome back over a channel that `run_status` reads on the loop's thread.
//!
//! **Progress is the state and the time running, nothing finer**: the backtesthub's run verbs send no progress frame
//! until their answer (`vike_hub_protocol::conn`'s module doc), so there is nothing finer to report.
//!
//! **Bounded**: at most [`MAX_RUNNING`] run at once and the newest [`MAX_ENDED`] ended runs are kept, so neither the
//! threads nor the kept answers grow with a session; a kept answer is the foreground tool's own, bounded as it is.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use vike_client::refusals::BackgroundRunRefusal;

use super::ToolError;
use super::backtest_tools::refused_before_the_dial;
use super::cancel::CancelToken;
use super::protocol::{DeferredCall, DeferredOutcome, run_deferred};
use super::tool_args::{RunIdArgs, parse_args};

/// At most this many background runs at once: each holds a thread and a daemon connection.
pub(super) const MAX_RUNNING: usize = 4;

/// How many ENDED runs `run_status` can still read, the newest: each holds its whole answer.
pub(super) const MAX_ENDED: usize = 16;

/// What a worker hands back: its outcome, and how long it ran.
type Handed = (DeferredOutcome, Duration);

/// How a run ended.
enum End {
    /// The tool's answer, exactly as the foreground call would have given it.
    Finished(Value),
    /// The tool's refusal or failure, in the foreground call's words.
    Failed(String),
    Cancelled,
}

impl End {
    fn of(tool: &str, outcome: DeferredOutcome) -> Self {
        match outcome {
            DeferredOutcome::Finished(Ok(Ok(answer))) => End::Finished(answer),
            DeferredOutcome::Finished(Ok(Err(e))) => End::Failed(e.message),
            // The foreground call's text for a panic (`super::protocol`'s `tool_response`).
            DeferredOutcome::Finished(Err(_)) => End::Failed(format!("tool panicked: {tool}")),
            DeferredOutcome::Cancelled => End::Cancelled,
        }
    }

    /// The `state` word `run_status` answers for it.
    fn word(&self) -> &'static str {
        match self {
            End::Finished(_) => "finished",
            End::Failed(_) => "failed",
            End::Cancelled => "cancelled",
        }
    }
}

enum State {
    Running(mpsc::Receiver<Handed>),
    Ended { end: End, took: Duration },
}

/// One background run.
struct Run {
    id: String,
    tool: String,
    token: CancelToken,
    started: Instant,
    state: State,
}

impl Run {
    fn is_running(&self) -> bool {
        matches!(self.state, State::Running(_))
    }

    /// What `run_status` answers for it: its id, its tool, its `state` and `elapsed_ms`, and once it ended its
    /// `answer` (finished) or `error` (failed).
    fn status(&self) -> Value {
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let mut status = RunStatusAnswer {
            run_id: self.id.clone(),
            tool: self.tool.clone(),
            state: "running",
            elapsed_ms: ms(self.started.elapsed()),
            answer: None,
            error: None,
        };
        if let State::Ended { end, took } = &self.state {
            status.state = end.word();
            status.elapsed_ms = ms(*took);
            match end {
                End::Finished(answer) => status.answer = Some(answer.clone()),
                End::Failed(error) => status.error = Some(error.clone()),
                End::Cancelled => {}
            }
        }
        json!(status)
    }
}

/// One run as it stands: `run_status`' and `cancel_run`'s answer, their `outputSchema` too, and a run tool's
/// `background: true` answer.
#[derive(Serialize, JsonSchema)]
pub(super) struct RunStatusAnswer {
    run_id: String,
    /// the run tool it runs
    tool: String,
    /// running | finished | failed | cancelled
    state: &'static str,
    /// how long it has run, or ran
    elapsed_ms: u64,
    /// finished: exactly what the tool would have answered in the foreground
    #[serde(skip_serializing_if = "Option::is_none")]
    answer: Option<Value>,
    /// failed: its refusal or failure, in the tool's words
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The background runs of one session, oldest first.
pub(super) struct BackgroundRuns {
    /// This process's start, in every id it mints: a run id of a process that ended finds no run here.
    session: u64,
    minted: u64,
    runs: Vec<Run>,
}

impl BackgroundRuns {
    pub(super) fn new() -> Self {
        Self { session: vike_model::now_ms_u64(), minted: 0, runs: Vec::new() }
    }

    /// Start `call` on a worker of its own and answer its run id. Refused before anything starts: what the tool
    /// refuses before it dials, in its own words, and a start while [`MAX_RUNNING`] runs are running.
    pub(super) fn start(&mut self, call: DeferredCall) -> Result<Value, ToolError> {
        refused_before_the_dial(&call.name, &call.args)?;
        self.poll();
        if self.runs.iter().filter(|run| run.is_running()).count() >= MAX_RUNNING {
            let refusal = BackgroundRunRefusal::TooManyRunning { limit: MAX_RUNNING };
            return Err(ToolError::refused(refusal.rendered()));
        }
        let (handed, outcome) = mpsc::channel::<Handed>();
        let token = CancelToken::new();
        let worker = token.clone();
        let tool = call.name.clone();
        let started = Instant::now();
        std::thread::Builder::new()
            .name("mcp-background".into())
            .spawn(move || {
                let outcome = run_deferred(&call, &worker);
                // A run cancelled or forgotten has dropped its receiver, and nobody waits for this.
                let _ = handed.send((outcome, started.elapsed()));
            })
            .map_err(|e| format!("cannot start a background run: no thread to be had: {e}"))?;
        self.minted += 1;
        let id = format!("run-{:x}-{}", self.session, self.minted);
        let run = Run { id, tool, token, started, state: State::Running(outcome) };
        let status = run.status();
        self.runs.push(run);
        Ok(status)
    }

    /// `run_status`: the run `args` names, as it stands now.
    pub(super) fn status(&mut self, args: &Value) -> Result<Value, String> {
        let at = self.find("run_status", args)?;
        Ok(self.runs[at].status())
    }

    /// `cancel_run`: stop the run `args` names and answer it, `state: cancelled`. A run that ended is refused.
    pub(super) fn cancel(&mut self, args: &Value) -> Result<Value, String> {
        let at = self.find("cancel_run", args)?;
        let run = &mut self.runs[at];
        if let State::Ended { end, .. } = &run.state {
            let refusal =
                BackgroundRunRefusal::AlreadyEnded { run_id: run.id.clone(), state: end.word() };
            return Err(refusal.rendered());
        }
        run.token.cancel();
        run.state = State::Ended { end: End::Cancelled, took: run.started.elapsed() };
        let status = run.status();
        self.forget_the_oldest();
        Ok(status)
    }

    /// The index of the run `args` names, every handed-back outcome taken in first.
    fn find(&mut self, tool: &'static str, args: &Value) -> Result<usize, String> {
        let run_id = parse_args::<RunIdArgs>(args, |_, _| String::new())
            .ok()
            .and_then(|args| args.run_id)
            .ok_or_else(|| BackgroundRunRefusal::NoRunId { tool }.rendered())?;
        self.poll();
        self.runs
            .iter()
            .position(|run| run.id == run_id)
            .ok_or_else(|| BackgroundRunRefusal::Unknown { run_id, kept: MAX_ENDED }.rendered())
    }

    /// Take in every outcome a worker handed back, then forget the oldest ended runs past [`MAX_ENDED`].
    fn poll(&mut self) {
        for run in &mut self.runs {
            let State::Running(outcome) = &run.state else { continue };
            let (end, took) = match outcome.try_recv() {
                Ok((outcome, took)) => (End::of(&run.tool, outcome), took),
                Err(mpsc::TryRecvError::Empty) => continue,
                // `run_deferred` catches a tool's panic, so only a worker killed outside it lands here.
                Err(mpsc::TryRecvError::Disconnected) => (
                    End::Failed("the run's worker ended without an answer".to_string()),
                    run.started.elapsed(),
                ),
            };
            run.state = State::Ended { end, took };
        }
        self.forget_the_oldest();
    }

    fn forget_the_oldest(&mut self) {
        let mut ended = self.runs.iter().filter(|run| !run.is_running()).count();
        self.runs.retain(|run| {
            let keep = run.is_running() || ended <= MAX_ENDED;
            ended -= usize::from(!keep);
            keep
        });
    }
}

impl Drop for BackgroundRuns {
    /// The session is over, so no client is left to read an answer: every run still running stops, its daemon
    /// connection closed as a cancel closes it.
    fn drop(&mut self) {
        for run in self.runs.iter().filter(|run| run.is_running()) {
            run.token.cancel();
        }
    }
}
