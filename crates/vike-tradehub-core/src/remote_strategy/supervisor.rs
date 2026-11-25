//! The supervisor of one sandboxed mount: ONE thread that owns the child for its whole life (the kernel
//! sends a child's `PR_SET_PDEATHSIG` when the THREAD that forked it ends, so the child is started here and
//! nowhere else), sends it one event at a time, waits for the reply up to [`HOOK_DEADLINE`], hands the reply
//! to the core, and restarts the child by the module root's failure table.
//!
//! Two helper threads per child, both ending with it: the READER (frames in, decoded, into the shared
//! inbox under the child's generation) and the TAIL (the child's stderr, the last [`REASON_TAIL`] bytes
//! kept for the reason a crash is reported in, and each line logged under the mount's id: the child's
//! log, the strategy file's own warnings included, has no other way out of the jail).

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use vike_hub_protocol::child_pipe;
use vike_marketdata::Bar;
use vike_utility::errors::CarriedRefusal;

use super::protocol::{
    self, BrokerCall, Event, FromChild, Open, Opened, SeriesDelta, ToChild, View,
};
use super::proxy::{FoldView, Inbound, Pending, RemoteSpec, RemoteStatus, Shared, TARGET};

/// How long a hook may take in the child before it is killed as hung (plan 2b §4: live 1 s).
pub const HOOK_DEADLINE: Duration = Duration::from_secs(1);
/// How long the child may take to answer `Open` (jail, load, build the strategy).
pub const OPEN_DEADLINE: Duration = Duration::from_secs(10);
/// How many crashes in [`CRASH_WINDOW`] stop the mount.
pub const CRASH_LIMIT: usize = 5;
/// The window [`CRASH_LIMIT`] counts crashes in.
pub const CRASH_WINDOW: Duration = Duration::from_secs(600);
/// How many events may wait for the child before it is restarted as stuck.
pub const QUEUE_CAP: usize = 4096;
/// How much of the child's fd 1 and 2 a crash's reason keeps (the end of it).
const REASON_TAIL: usize = 2048;
/// How many of the child's stderr lines reach the daemon's log in a [`LOG_WINDOW`]; the rest are
/// counted and the count said once the window ends, so a user file printing per tick cannot flood the
/// box's journal.
const LOG_LINES_PER_WINDOW: u32 = 60;
/// The window [`LOG_LINES_PER_WINDOW`] counts in.
const LOG_WINDOW: Duration = Duration::from_secs(60);
/// The longest child stderr line logged whole; a longer one is logged in pieces this long.
const LOG_LINE_CAP: usize = 4096;
/// The wait before restart `n` (1-based) after a crash: at once, then 1, 2, 4 s (decision 0273).
const BACKOFF: [Duration; 4] =
    [Duration::ZERO, Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(4)];

/// A started child: the process, and the two ends of the protocol.
pub struct Launched {
    pub child: Box<dyn SandboxChild>,
    pub to_child: Box<dyn Write + Send>,
    pub from_child: Box<dyn Read + Send>,
    /// The child's stderr, when the launcher captures it.
    pub reasons: Option<Box<dyn Read + Send>>,
}

/// The process half of a started child.
pub trait SandboxChild: Send {
    /// SIGKILL it (a no-op once it is dead).
    fn kill(&mut self);
    /// Wait for it to end, and say how it ended ("exit code 3", "killed by signal 9").
    fn wait(&mut self) -> String;
}

/// Starts one child: called once per open and once per restart, on the supervisor thread.
pub type Launcher = Box<dyn FnMut() -> io::Result<Launched> + Send>;

/// The production child: `program args`, started by `vike_sandbox::spawn_strategy_process` (an empty
/// environment, working directory `/`; stdin and stdout the protocol, stderr the reason).
#[derive(Debug, Clone)]
pub struct SandboxCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl SandboxCommand {
    /// This binary's `strategy-sandbox live <builds> <artifact>` verb: `artifact` is the builder's answer
    /// RELATIVE to `builds` (`sdk-N/<folder>/<sha>.so`), which the child opens beneath `builds` with no
    /// link followed (`crates/vike-strategy-sandbox/src/lib.rs`'s module doc, "The artifact grant").
    ///
    /// # Errors
    /// The path of this binary cannot be read.
    pub fn multicall(builds: &Path, artifact: &Path) -> io::Result<SandboxCommand> {
        Ok(SandboxCommand {
            program: std::env::current_exe()?,
            args: vec![
                "strategy-sandbox".into(),
                "live".into(),
                builds.as_os_str().to_owned(),
                artifact.as_os_str().to_owned(),
            ],
        })
    }

    /// The launcher that starts this command.
    pub fn launcher(self) -> Launcher {
        Box::new(move || launch(&self))
    }
}

/// Start the child the one way a strategy process is started (`vike_sandbox::spawn_strategy_process`:
/// an empty environment, working directory `/`, three pipes). Off Linux the child refuses its jail by
/// name, which the open reports.
fn launch(cmd: &SandboxCommand) -> io::Result<Launched> {
    let mut child = vike_sandbox::spawn_strategy_process(&cmd.program, &cmd.args)?;
    let piped = |what: &str| io::Error::other(format!("the child's {what} was not piped"));
    let to_child = child.stdin.take().ok_or_else(|| piped("stdin"))?;
    let from_child = child.stdout.take().ok_or_else(|| piped("stdout"))?;
    let reasons = child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
    Ok(Launched {
        child: Box::new(ProcessChild(child)),
        to_child: Box::new(to_child),
        from_child: Box::new(from_child),
        reasons,
    })
}

struct ProcessChild(std::process::Child);

impl SandboxChild for ProcessChild {
    fn kill(&mut self) {
        let _ = self.0.kill();
    }
    fn wait(&mut self) -> String {
        match self.0.wait() {
            Ok(status) => exit_words(status),
            Err(e) => format!("could not be waited for: {e}"),
        }
    }
}

#[cfg(unix)]
fn exit_words(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(sig)) => format!("killed by signal {sig}"),
        (None, None) => status.to_string(),
    }
}

#[cfg(not(unix))]
fn exit_words(status: std::process::ExitStatus) -> String {
    status.to_string()
}

/// Start the supervisor thread and wait for the first open's answer (a child's refusal with its id kept).
pub(super) fn start(
    shared: Arc<Shared>,
    spec: RemoteSpec,
    launcher: Launcher,
) -> Result<Opened, CarriedRefusal> {
    let (tx, rx) = mpsc::channel();
    let name = format!("vt-remote-{}", shared.mount_id);
    std::thread::Builder::new()
        .name(name)
        .spawn(move || Supervisor::new(shared, spec, launcher).run(tx))
        .map_err(|e| format!("the supervisor thread did not start: {e}"))?;
    rx.recv().unwrap_or_else(|_| Err("the supervisor ended before the strategy opened".into()))
}

/// The child the supervisor runs now.
struct Live {
    child: Box<dyn SandboxChild>,
    to_child: Box<dyn Write + Send>,
    generation: u64,
    tail: Arc<Mutex<VecDeque<u8>>>,
    /// What the child holds of each history, so only what is new is sent.
    sent_bars: HashMap<String, (usize, Bar)>,
    sent_inputs: HashMap<String, (usize, Bar)>,
    sent_closed: usize,
}

impl Live {
    /// Kill the child and say how it ended, with the end of what it printed.
    fn end(mut self) -> String {
        self.child.kill();
        drop(self.to_child);
        let exit = self.child.wait();
        let tail = self.tail.lock().unwrap_or_else(PoisonError::into_inner);
        let (a, b) = tail.as_slices();
        let text = String::from_utf8_lossy(&[a, b].concat()).trim().to_string();
        match text.lines().last() {
            Some(last) => format!("{exit}, last words: {last}"),
            None => exit,
        }
    }
}

/// How serving a child (or waiting on it) ended.
enum End {
    /// The proxy went away: stop.
    Closing,
    /// The child died or misbehaved: restart it.
    Crash(String),
    /// A wait passed its deadline with no answer.
    Deadline,
    /// The host asked for a fee re-arm at this schedule (`RemoteHandle::fee_rearm`).
    Rearm(Option<vike_model::FeeSchedule>),
    /// The child sent what no restart can mend (a state that is not msgpack): stop the mount, once.
    Stop(String),
}

struct Supervisor {
    shared: Arc<Shared>,
    spec: RemoteSpec,
    launcher: Launcher,
    generation: u64,
    /// The symbols an intent may name: `""`, the mount's own, and what the strategy declared.
    allowed: Vec<String>,
    inputs: Vec<String>,
    crashes: VecDeque<Instant>,
}

impl Supervisor {
    fn new(shared: Arc<Shared>, spec: RemoteSpec, launcher: Launcher) -> Supervisor {
        Supervisor {
            shared,
            spec,
            launcher,
            generation: 0,
            allowed: Vec::new(),
            inputs: Vec::new(),
            crashes: VecDeque::new(),
        }
    }

    fn run(mut self, opened_tx: mpsc::Sender<Result<Opened, CarriedRefusal>>) {
        let mut live = match self.open(None) {
            Ok((live, opened)) => {
                tracing::info!(
                    target: TARGET,
                    mount_id = %self.shared.mount_id,
                    file = %self.spec.file,
                    sha256 = %self.spec.sha256,
                    "sandboxed strategy opened"
                );
                let _ = opened_tx.send(Ok(opened));
                live
            }
            Err(reason) => {
                let _ = opened_tx.send(Err(reason));
                return;
            }
        };
        loop {
            let reason = match self.serve(&mut live) {
                End::Closing => {
                    live.end();
                    return;
                }
                End::Crash(why) => format!("{why} ({})", live.end()),
                End::Deadline => format!("no answer ({})", live.end()),
                End::Stop(why) => {
                    let reason = format!("{why} ({})", live.end());
                    self.stop(&reason);
                    return;
                }
                End::Rearm(schedule) => {
                    let Some(facts) = self.rearmed_facts(schedule) else {
                        self.answer_rearm("kept: the account's rate moves no floor".to_string());
                        continue;
                    };
                    self.spec.facts = facts;
                    // Not a crash: the old child goes, a fresh one opens on the new facts.
                    live.end();
                    let state = self.shared.cache().state.clone();
                    match self.open(state) {
                        Ok((next, _)) => {
                            live = next;
                            self.answer_rearm("rebuilt at the account's own fee rate".to_string());
                            continue;
                        }
                        Err(why) => {
                            self.answer_rearm(format!("the rebuild failed: {why}"));
                            why.to_string()
                        }
                    }
                }
            };
            match self.restart(reason) {
                Some(next) => live = next,
                None => return,
            }
        }
    }

    /// The facts a fee re-arm at `schedule` rebuilds the strategy with, or `None` when it rebuilds
    /// nothing. The law is the in-process maker's, called as it was (`vike_market_making::rearm_fee_floor`, its
    /// logs included) over the floor the file holds now: the account rate it was handed, else the
    /// static lane's.
    fn rearmed_facts(
        &self,
        schedule: Option<vike_model::FeeSchedule>,
    ) -> Option<vike_model::params::MountFacts> {
        let facts = &self.spec.facts;
        let mut held = vike_model::params::Params::default();
        if let Some(rate) = facts.account_round_trip_fee_rate.or(facts.static_round_trip_fee_rate) {
            held.insert("round_trip_fee_rate", rate);
        }
        let armed =
            vike_market_making::rearm_fee_floor(&held, schedule, &facts.venue, &facts.symbol)?;
        let rate =
            armed.get("round_trip_fee_rate").and_then(vike_model::params::ParamValue::as_f64);
        Some(vike_model::params::MountFacts { account_round_trip_fee_rate: rate, ..facts.clone() })
    }

    /// Hand the host waiting in `RemoteHandle::fee_rearm` its answer.
    fn answer_rearm(&self, answer: String) {
        self.shared.outbox().rearmed = Some(answer);
        self.shared.wake.notify_all();
    }

    /// Stop the mount for good: events are dropped until an operator restarts it, and its resting orders
    /// rest. `reason` is what the operator is told.
    fn stop(&self, reason: &str) {
        tracing::error!(target: TARGET, mount_id = %self.shared.mount_id, %reason,
            "sandboxed strategy STOPPED; its resting orders rest, nothing is cancelled");
        let mut out = self.shared.outbox();
        out.refusing = true;
        out.dropped += out.events.len() as u64;
        out.events.clear();
        drop(out);
        self.shared.set_status(RemoteStatus::Stopped { reason: reason.to_string() });
    }

    /// Count a crash and start a fresh child, or stop the mount; `None` when the supervisor ends.
    fn restart(&mut self, mut reason: String) -> Option<Live> {
        loop {
            // A failed open that stopped the mount ([`End::Stop`]) is not a crash to count.
            if self.shared.outbox().refusing {
                return None;
            }
            self.shared.set_last_reason(&reason);
            let now = Instant::now();
            self.crashes.push_back(now);
            while self.crashes.front().is_some_and(|t| now.duration_since(*t) > CRASH_WINDOW) {
                self.crashes.pop_front();
            }
            let crashes = self.crashes.len();
            if crashes >= CRASH_LIMIT {
                self.stop(&format!("crash loop: {reason}"));
                return None;
            }
            let status = RemoteStatus::Restarting { crashes, reason: reason.clone() };
            tracing::warn!(target: TARGET, mount_id = %self.shared.mount_id, status = %status,
                "sandboxed strategy died; its resting orders rest, nothing is cancelled");
            self.shared.set_status(status);
            if self.pause(BACKOFF[(crashes - 1).min(BACKOFF.len() - 1)]) {
                return None;
            }
            let state = self.shared.cache().state.clone();
            match self.open(state) {
                Ok((live, _)) => {
                    tracing::info!(target: TARGET, mount_id = %self.shared.mount_id,
                        "sandboxed strategy restarted");
                    self.shared.set_status(RemoteStatus::Running);
                    return Some(live);
                }
                Err(why) => reason = why.to_string(),
            }
        }
    }

    /// Wait `pause`, or less if the proxy goes away: `true` when it did.
    fn pause(&self, pause: Duration) -> bool {
        let deadline = Instant::now() + pause;
        let mut out = self.shared.outbox();
        loop {
            if out.closing {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { return false };
            out =
                self.shared.wake.wait_timeout(out, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Start a child and open the strategy in it (`state`: the bytes the last child sent, if any). A refused open
    /// keeps the child's id.
    fn open(
        &mut self,
        state: Option<vike_model::StrategyState>,
    ) -> Result<(Live, Opened), CarriedRefusal> {
        self.generation += 1;
        let generation = self.generation;
        let Launched { child, to_child, from_child, reasons } =
            (self.launcher)().map_err(|e| format!("the sandbox process did not start: {e}"))?;
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(REASON_TAIL)));
        if let Some(reasons) = reasons {
            spawn_tail(reasons, Arc::clone(&tail), self.shared.mount_id.clone());
        }
        spawn_reader(Arc::clone(&self.shared), generation, from_child, self.spec.build.clone());
        let mut live = Live {
            child,
            to_child,
            generation,
            tail,
            sent_bars: HashMap::new(),
            sent_inputs: HashMap::new(),
            sent_closed: 0,
        };
        let open = Open {
            file: self.spec.file.clone(),
            sha256: self.spec.sha256.clone(),
            params: self.spec.params.clone(),
            facts: self.spec.facts.clone(),
            state,
        };
        // This build first ([`child_pipe::check_build`]: the child refuses another one), then the open.
        let sent = child_pipe::send_build(&mut live.to_child, &self.spec.build).and_then(|()| {
            child_pipe::write_message(&mut live.to_child, &ToChild::Open(Box::new(open)))
        });
        if let Err(e) = sent {
            return Err(format!("the strategy could not be opened: {e} ({})", live.end()).into());
        }
        let answer = self.await_inbound(&live, Instant::now() + OPEN_DEADLINE, |m| match m {
            FromChild::Opened(o) => Ok(Ok(o)),
            FromChild::Refused { reason, id } => Ok(Err((reason, id))),
            other => Err(other),
        });
        match answer {
            Ok(Ok(opened)) => {
                self.allowed = ["".to_string(), self.spec.facts.symbol.clone()]
                    .into_iter()
                    .chain(opened.symbols.iter().cloned())
                    .collect();
                self.inputs.clone_from(&opened.inputs);
                let mut cache = self.shared.cache();
                cache.state.clone_from(&opened.state);
                cache.params = opened.params.as_deref().cloned();
                drop(cache);
                Ok((live, opened))
            }
            Ok(Err((refused, id))) => {
                Err(CarriedRefusal::new(id, format!("refused: {refused} ({})", live.end())))
            }
            // `await_inbound` never answers a re-arm: that request is read by `next_event` alone.
            Err(End::Closing | End::Rearm(_)) => {
                Err(format!("the mount went away ({})", live.end()).into())
            }
            Err(End::Crash(why)) => Err(format!("{why} ({})", live.end()).into()),
            Err(End::Deadline) => Err(format!(
                "no answer to open within {} s ({})",
                OPEN_DEADLINE.as_secs(),
                live.end()
            )
            .into()),
            // Stopped once here, so a restart that opens on it ends instead of counting a crash.
            Err(End::Stop(why)) => {
                let reason = format!("{why} ({})", live.end());
                self.stop(&reason);
                Err(reason.into())
            }
        }
    }

    /// Wait for the message of the current child `want` picks, until `deadline`. A state frame updates
    /// the cache on the way; any other message, the child's death and the deadline end the wait.
    fn await_inbound<T>(
        &self,
        live: &Live,
        deadline: Instant,
        mut want: impl FnMut(FromChild) -> Result<T, FromChild>,
    ) -> Result<T, End> {
        let mut out = self.shared.outbox();
        loop {
            while let Some((generation, inbound)) = out.inbox.pop_front() {
                if generation != live.generation {
                    continue;
                }
                match inbound {
                    Inbound::Gone(why) => return Err(End::Crash(why)),
                    Inbound::Unusable(why) => return Err(End::Stop(why)),
                    Inbound::Message(FromChild::State { state, params }) => {
                        let mut cache = self.shared.cache();
                        (cache.state, cache.params) = (state, params.map(|p| *p));
                    }
                    Inbound::Message(m) => {
                        return want(m)
                            .map_err(|m| End::Crash(format!("garbage: unexpected {m:?}")));
                    }
                }
            }
            if out.closing {
                return Err(End::Closing);
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Err(End::Deadline);
            };
            out =
                self.shared.wake.wait_timeout(out, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Serve the current child until it dies, misbehaves, or the proxy goes away.
    fn serve(&mut self, live: &mut Live) -> End {
        let mut seq = 0u64;
        loop {
            let Some(pending) = self.next_event(live) else { return End::Closing };
            let pending = match pending {
                Ok(p) => p,
                Err(end) => return end,
            };
            seq += 1;
            let hook = pending.hook.name();
            let on_bar = matches!(pending.hook, protocol::Hook::Bar(_));
            // An order's end (a fill, a re-tune, ...) the child may die inside goes to the next child.
            let retry = pending.hook.is_lossless().then(|| pending.clone());
            let now = pending.view.as_ref().map_or(0, |v| v.now);
            let view = pending.view.map(|v| self.wire_view(live, v));
            let event = Event { sequence: seq, hook: pending.hook, view };
            let crashed = |why: String| {
                if let Some(p) = retry.clone() {
                    self.shared.outbox().events.push_front(p);
                }
                End::Crash(why)
            };
            if let Err(e) =
                child_pipe::write_message(&mut live.to_child, &ToChild::Event(Box::new(event)))
            {
                return crashed(format!("the pipe to the strategy broke: {e}"));
            }
            let deadline = Instant::now() + HOOK_DEADLINE;
            let calls = match self.await_inbound(live, deadline, |m| match m {
                FromChild::Reply { sequence: got, calls } if got == seq => Ok(calls),
                other => Err(other),
            }) {
                Ok(calls) => calls,
                Err(End::Crash(why)) => return crashed(why),
                Err(End::Stop(why)) => return End::Stop(why),
                Err(End::Deadline) => {
                    let ms = HOOK_DEADLINE.as_millis();
                    return crashed(format!("hang: no reply to {hook} within {ms} ms"));
                }
                // `await_inbound` never answers a re-arm (see `open`).
                Err(End::Closing | End::Rearm(_)) => return End::Closing,
            };
            if let Some(why) = self.refuse_calls(&calls) {
                return crashed(why);
            }
            if !calls.is_empty() && !self.hand_to_core(calls, now, on_bar) {
                return End::Closing;
            }
        }
    }

    /// Wait until the core has applied the reply just handed to it, so the next event sent is one the
    /// strategy would hear AFTER that reply's consequences in-process (`Outbox::applying`). `false` when
    /// the proxy went away meanwhile.
    fn await_applied(&self) -> bool {
        let mut out = self.shared.outbox();
        while !out.acked {
            if out.closing {
                return false;
            }
            out = self.shared.wake.wait(out).unwrap_or_else(PoisonError::into_inner);
        }
        out.acked = false;
        true
    }

    /// The next event to send, parking idle until there is one. `None` when the proxy is gone; an `Err`
    /// when the child must be restarted first (it died while idle, or the queue overflowed).
    fn next_event(&self, live: &Live) -> Option<Result<Pending, End>> {
        let mut out = self.shared.outbox();
        loop {
            while let Some((generation, inbound)) = out.inbox.pop_front() {
                if generation != live.generation {
                    continue;
                }
                match inbound {
                    Inbound::Gone(why) => return Some(Err(End::Crash(why))),
                    Inbound::Unusable(why) => return Some(Err(End::Stop(why))),
                    Inbound::Message(FromChild::State { state, params }) => {
                        let mut cache = self.shared.cache();
                        (cache.state, cache.params) = (state, params.map(|p| *p));
                    }
                    Inbound::Message(m) => {
                        return Some(Err(End::Crash(format!("garbage: unasked {m:?}"))));
                    }
                }
            }
            if out.closing {
                return None;
            }
            if let Some(schedule) = out.rearm.take() {
                return Some(Err(End::Rearm(schedule)));
            }
            if out.dropped > 0 {
                tracing::warn!(target: TARGET, mount_id = %self.shared.mount_id, dropped = out.dropped,
                    "sandboxed strategy: market events dropped over the queue cap");
                out.dropped = 0;
            }
            if out.overflow {
                out.overflow = false;
                let waiting = out.events.len();
                out.events.retain(|p| p.hook.is_lossless());
                return Some(Err(End::Crash(format!(
                    "event queue full: {waiting} events waiting (the market ones dropped)"
                ))));
            }
            if let Some(p) = out.events.pop_front() {
                return Some(Ok(p));
            }
            out.idle = true;
            out = self.shared.wake.wait(out).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The view the child is sent: the histories as changes to what it holds.
    fn wire_view(&self, live: &mut Live, v: FoldView) -> View {
        let mut bars = Vec::new();
        for (name, series) in &v.bars {
            if let Some(d) = SeriesDelta::between(name, live.sent_bars.get(name), series) {
                bars.push(d);
            }
            note_sent(&mut live.sent_bars, name, series);
        }
        let mut inputs = Vec::new();
        if let Some(at) = &v.inputs {
            for name in &self.inputs {
                let held = at.closed_bars(name);
                if let Some(d) = SeriesDelta::between(name, live.sent_inputs.get(name), held) {
                    inputs.push(d);
                }
                note_sent(&mut live.sent_inputs, name, held);
            }
        }
        let rows = v.closed.as_deref().map_or(&[][..], |ledger| ledger.rows());
        let closed_from = if rows.len() >= live.sent_closed { live.sent_closed } else { 0 };
        let closed = rows[closed_from..].to_vec();
        live.sent_closed = rows.len();
        View {
            rows: v.rows,
            declared: v.declared,
            equity: v.equity,
            index: v.index,
            now: v.now,
            bars,
            inputs,
            closed_from,
            closed,
        }
    }

    /// Why a reply is garbage: an intent naming a symbol the mount does not trade.
    fn refuse_calls(&self, calls: &[BrokerCall]) -> Option<String> {
        calls.iter().find_map(|c| {
            let symbol = c.symbol()?;
            (!self.allowed.iter().any(|a| a == symbol)).then(|| {
                format!("garbage: an intent for `{symbol}`, a symbol this mount does not trade")
            })
        })
    }

    /// Hand a reply's intents to the core, then wait until it applied them ([`Self::await_applied`]);
    /// `false` when the core or the proxy is gone.
    /// `on_bar` says the reply answers a bar (`vike_execution::MountReply::on_bar`).
    fn hand_to_core(&self, calls: Vec<BrokerCall>, now: i64, on_bar: bool) -> bool {
        let Some(lane) = self.shared.lane.get() else {
            tracing::error!(target: TARGET, mount_id = %self.shared.mount_id,
                "sandboxed strategy replied before the core's lane was attached: intents dropped");
            return true;
        };
        // While the core applies them, what it delivers to this mount goes to the head of the queue.
        let (opening, closing) = (Arc::clone(&self.shared), Arc::clone(&self.shared));
        let apply = vike_core::MountReplyIntents {
            write: Box::new(move |broker: &mut vike_core::LiveBroker| {
                opening.outbox().applying = Some(0);
                for call in calls {
                    call.apply(broker);
                }
            }),
            applied: Box::new(move || {
                let mut out = closing.outbox();
                (out.applying, out.acked) = (None, true);
                drop(out);
                closing.wake.notify_all();
            }),
        };
        let reply = vike_execution::MountReply {
            mount_id: self.shared.mount_id.clone(),
            now,
            on_bar,
            apply: Box::new(apply),
        };
        lane.mount_reply(reply).is_ok() && self.await_applied()
    }
}

/// Record that the child now holds `series` under `name`.
fn note_sent(sent: &mut HashMap<String, (usize, Bar)>, name: &str, series: &[Bar]) {
    match series.last() {
        Some(last) => {
            sent.insert(name.to_string(), (series.len(), last.clone()));
        }
        None => {
            sent.remove(name);
        }
    }
}

/// The reader: frames from the child into the shared inbox, until the child is gone. The first frame is
/// the child's build ([`child_pipe::check_build`] against `ours`, `RemoteSpec::build`): another build is gone
/// before any message is decoded.
fn spawn_reader(
    shared: Arc<Shared>,
    generation: u64,
    mut from_child: Box<dyn Read + Send>,
    ours: String,
) {
    let spawned = std::thread::Builder::new().name("vt-remote-read".into()).spawn(move || {
        let checked = child_pipe::check_build(&mut from_child, &ours, "the strategy process");
        let mut inbound = match checked {
            Ok(()) => read_inbound(&mut from_child),
            Err(e) => gone(&e),
        };
        loop {
            let ended = matches!(inbound, Inbound::Gone(_) | Inbound::Unusable(_));
            let mut out = shared.outbox();
            out.inbox.push_back((generation, inbound));
            out.idle = false;
            drop(out);
            shared.wake.notify_all();
            if ended {
                return;
            }
            inbound = read_inbound(&mut from_child);
        }
    });
    if let Err(e) = spawned {
        tracing::error!(target: TARGET, error = %e, "sandboxed strategy: the reader did not start");
    }
}

/// The child's next message, or why it is gone.
fn read_inbound(from_child: &mut impl Read) -> Inbound {
    match child_pipe::read_message::<_, FromChild>(from_child) {
        Ok(m) => unusable_state(&m).map_or(Inbound::Message(m), Inbound::Unusable),
        Err(e) => gone(&e),
    }
}

/// Why a state the child sent cannot be kept: its bytes are not one msgpack value
/// (`vike_model::StrategyState::check`). Checked ONCE, as it arrives (the child sends a state only when its
/// bytes changed, so an unchanged state stays a byte compare there), on this reader thread, off the child's
/// hook path and off the fold. A scan, no document built: what the core saves (`RemoteStrategy`'s
/// `save_state`, the sidecar) and a restarted child loads is then msgpack.
fn unusable_state(message: &FromChild) -> Option<String> {
    let (FromChild::State { state: Some(state), .. }
    | FromChild::Opened(Opened { state: Some(state), .. })) = message
    else {
        return None;
    };
    let scanned = vike_model::StrategyState::check(state.bytes());
    scanned.err().map(|e| format!("the strategy's save_state is not valid msgpack: {e}"))
}

/// Why the child is gone, from the read that failed.
fn gone(e: &io::Error) -> Inbound {
    Inbound::Gone(match e.kind() {
        io::ErrorKind::UnexpectedEof => "the strategy process ended".into(),
        // Another build ([`child_pipe::check_build`]), in its own words.
        io::ErrorKind::Unsupported => e.to_string(),
        io::ErrorKind::InvalidData => format!("garbage: an undecodable frame: {e}"),
        _ => format!("the pipe from the strategy broke: {e}"),
    })
}

/// The tail: [`read_reasons`] on a thread of its own.
fn spawn_tail(reasons: Box<dyn Read + Send>, tail: Arc<Mutex<VecDeque<u8>>>, mount_id: String) {
    let _ = std::thread::Builder::new()
        .name("vt-remote-tail".into())
        .spawn(move || read_reasons(reasons, &tail, &mount_id));
}

/// Read the child's stderr to its end: the last [`REASON_TAIL`] bytes kept in `tail` for a crash's
/// reason, and each line logged under `mount_id` (at most [`LOG_LINES_PER_WINDOW`] a window).
///
/// ⚠ The child's stderr is the ONLY way its log leaves the jail (no log file: a write channel past
/// it), the strategy file's own `tracing` events included. Kept only as a crash's tail, the spread
/// maker's edge-triggered `maker HOLDING` lines (its break-even refusal, with the numbers) never
/// reached the daemon's log, and a maker refusing by design read as a maker that was broken.
pub(super) fn read_reasons(mut reasons: impl Read, tail: &Mutex<VecDeque<u8>>, mount_id: &str) {
    let mut log =
        ChildLog { mount_id, line: Vec::new(), window: Instant::now(), logged: 0, dropped: 0 };
    let mut buffer = [0u8; 512];
    while let Ok(n) = reasons.read(&mut buffer) {
        if n == 0 {
            break;
        }
        {
            let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
            tail.extend(&buffer[..n]);
            let over = tail.len().saturating_sub(REASON_TAIL);
            tail.drain(..over);
        }
        log.push(&buffer[..n]);
    }
    log.flush();
    log.say_dropped();
}

/// The child's stderr, cut into lines and logged under its mount's id within the line budget.
struct ChildLog<'a> {
    mount_id: &'a str,
    line: Vec<u8>,
    window: Instant,
    logged: u32,
    dropped: u64,
}

impl ChildLog<'_> {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.flush();
                continue;
            }
            if self.line.len() >= LOG_LINE_CAP {
                self.flush();
            }
            self.line.push(byte);
        }
    }

    fn flush(&mut self) {
        let text = String::from_utf8_lossy(&self.line).trim_end().to_string();
        self.line.clear();
        if text.is_empty() {
            return;
        }
        if self.window.elapsed() >= LOG_WINDOW {
            self.say_dropped();
            self.window = Instant::now();
            self.logged = 0;
        }
        if self.logged >= LOG_LINES_PER_WINDOW {
            self.dropped += 1;
            return;
        }
        self.logged += 1;
        tracing::info!(target: TARGET, mount_id = %self.mount_id, "sandboxed strategy: {text}");
    }

    fn say_dropped(&mut self) {
        if self.dropped > 0 {
            tracing::warn!(target: TARGET, mount_id = %self.mount_id, dropped = self.dropped,
                "sandboxed strategy: stderr lines over the log budget were dropped");
            self.dropped = 0;
        }
    }
}
