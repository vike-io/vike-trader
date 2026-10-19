//! The measuring half of the PRICE hop harness (`crates/vike-datahub/tests/price_hop.rs`): the stamps, the
//! three arrival SHAPES, the driver, the quantiles and the `HOP-LATENCY` lines.
//!
//! ⚠ A COPY of `crates/vike-gatewayhub/tests/support/hop_stats.rs`, the order and event harnesses' file: a
//! test file does not cross a crate. The two print the same lines, and a change to one is made in both. Its
//! one reader is `price_hop.rs`, which `#[path]`-includes it (a bare `mod` would resolve beside the
//! binary root).
//!
//! # What a sample is
//!
//! One `Instant` clock for the whole process (the harness, the daemon and the peer share the box), read as
//! nanoseconds since [`Stamps`]'s base. A sample is `landed[i] - sent[i]`: `sent[i]` is stamped on the
//! sending thread right BEFORE the call a core makes (the payload is built first), `landed[i]` on the
//! thread that sees the effect (the scripted adapter's submit hook, or the spin-waiting receiver that
//! stands for the core's ingest). Nothing on either stamping path locks or allocates.
//!
//! # The three shapes
//!
//! * [`Shape::Paced`]: one in flight. The sender waits for the effect, then sleeps [`GAP`]: the mean rate of
//!   an order path, and the queue always empty.
//! * [`Shape::Burst`]: `d` back to back, the round ends when all landed: the k-th has up to k-1 frames ahead
//!   of it (the worst burst).
//! * [`Shape::Rate`]: open loop at `hz` events a second against a fixed schedule, so a slow hop shows as a
//!   queue and not as a slower sender. Latency is still stamped at the real send instant, so a sender
//!   that falls behind its schedule reads as late sends, not as hop time.
//!
//! A cell whose events stop landing for [`STALL`] is CUT: the rest count `lost` and the cell ends. The
//! line says so; nothing is asserted about it.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a wait may go without ONE more event landing before the rest count as lost.
pub const STALL: Duration = Duration::from_secs(5);
/// The pause between two samples of a paced or burst cell (the mean-rate gap).
pub const GAP: Duration = Duration::from_micros(200);

/// The arrival shape of one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// One in flight at a time.
    Paced,
    /// `d` back to back per round.
    Burst(usize),
    /// Open loop at this many events a second.
    Rate(u32),
}

impl Shape {
    /// The word the line carries.
    pub fn word(self) -> String {
        match self {
            Shape::Paced => "paced".to_string(),
            Shape::Burst(d) => format!("burst{d}"),
            Shape::Rate(hz) => format!("rate{hz}"),
        }
    }
}

/// Two nanosecond slots per sample, 0 = not yet.
pub struct Stamps {
    base: Instant,
    sent: Vec<AtomicU64>,
    landed: Vec<AtomicU64>,
}

impl Stamps {
    /// Room for `n` samples.
    pub fn new(n: usize) -> Self {
        let slots = |n: usize| (0..n).map(|_| AtomicU64::new(0)).collect::<Vec<_>>();
        Self { base: Instant::now(), sent: slots(n), landed: slots(n) }
    }

    /// Nanoseconds since the base, +1 so that 0 means "not yet".
    pub fn now_ns(&self) -> u64 {
        self.base.elapsed().as_nanos() as u64 + 1
    }

    /// Sample `i` is being handed over now; the instant.
    pub fn stamp_sent(&self, i: usize) -> u64 {
        let at = self.now_ns();
        self.sent[i].store(at, Ordering::Release);
        at
    }

    /// Sample `i` took effect now. The FIRST landing of an index counts; a later one (a replay) does not.
    pub fn land(&self, i: usize) {
        let at = self.now_ns();
        if let Some(slot) = self.landed.get(i) {
            let _ = slot.compare_exchange(0, at, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    /// Whether sample `i` has landed.
    pub fn has_landed(&self, i: usize) -> bool {
        self.landed[i].load(Ordering::Acquire) != 0
    }

    /// When sample `i` was sent, if it was.
    pub fn sent_at(&self, i: usize) -> Option<u64> {
        let v = self.sent[i].load(Ordering::Acquire);
        (v != 0).then_some(v)
    }

    /// Latency of sample `i`, if it was sent and landed.
    pub fn latency(&self, i: usize) -> Option<u64> {
        let (s, l) = (self.sent[i].load(Ordering::Acquire), self.landed[i].load(Ordering::Acquire));
        (s != 0 && l >= s).then(|| l - s)
    }
}

/// One cell's samples.
pub struct Run {
    /// Per timed sample that landed, ns from the send stamp to the effect.
    pub latency: Vec<u64>,
    /// Per timed sample, ns the sending call itself took (what the core's fold thread pays).
    pub emit: Vec<u64>,
    /// Samples that never landed (all of the cut round).
    pub lost: usize,
    /// The sample index the cell was cut at, if it was.
    pub cut: Option<usize>,
}

/// Wait until every index of `range` has landed. `false` = they stopped landing for [`STALL`].
fn wait_landed(stamps: &Stamps, range: std::ops::Range<usize>) -> bool {
    let count = |r: &std::ops::Range<usize>| r.clone().filter(|&i| stamps.has_landed(i)).count();
    let (mut seen, mut moved) = (count(&range), Instant::now());
    while seen < range.len() {
        if moved.elapsed() >= STALL {
            return false;
        }
        std::hint::spin_loop();
        let now = count(&range);
        if now != seen {
            (seen, moved) = (now, Instant::now());
        }
    }
    true
}

/// Drive `payloads` (one per sample, built by the caller BEFORE the run so building is not timed) through
/// `send` in `shape`. Sample `k` is index `base + k` in `stamps`; the first `warmup` samples are driven
/// but not timed.
pub fn drive<T>(
    shape: Shape,
    base: usize,
    warmup: usize,
    stamps: &Stamps,
    payloads: Vec<T>,
    mut send: impl FnMut(T),
) -> Run {
    let total = payloads.len();
    let mut payloads: Vec<Option<T>> = payloads.into_iter().map(Some).collect();
    let mut emit = Vec::with_capacity(total.saturating_sub(warmup));
    let mut fire = |k: usize| {
        let payload = payloads[k].take().expect("each sample is sent once");
        stamps.stamp_sent(base + k);
        let started = Instant::now();
        send(payload);
        if k >= warmup {
            emit.push(started.elapsed().as_nanos() as u64);
        }
    };
    let mut cut = None;
    match shape {
        Shape::Paced => {
            for k in 0..total {
                fire(k);
                if !wait_landed(stamps, base + k..base + k + 1) {
                    cut = Some(k);
                    break;
                }
                std::thread::sleep(GAP);
            }
        }
        Shape::Burst(d) => {
            let mut start = 0;
            while start < total {
                let end = (start + d).min(total);
                for k in start..end {
                    fire(k);
                }
                if !wait_landed(stamps, base + start..base + end) {
                    cut = Some(start);
                    break;
                }
                start = end;
                std::thread::sleep(GAP);
            }
        }
        Shape::Rate(hz) => {
            let period = Duration::from_nanos(1_000_000_000 / u64::from(hz.max(1)));
            let origin = Instant::now();
            for k in 0..total {
                let due = origin + period * k as u32;
                while Instant::now() < due {
                    std::hint::spin_loop();
                }
                fire(k);
            }
            if !wait_landed(stamps, base..base + total) {
                // The first sample that never landed.
                cut = (0..total).find(|&k| !stamps.has_landed(base + k));
            }
        }
    }
    // Only whole rounds before a cut are timed; a Rate cell times every sample that landed.
    let timed_end = match (shape, cut) {
        (Shape::Rate(_), _) | (_, None) => total,
        (_, Some(at)) => at,
    };
    let latency: Vec<u64> = (warmup..timed_end).filter_map(|k| stamps.latency(base + k)).collect();
    let lost = match (shape, cut) {
        (_, None) => 0,
        (Shape::Rate(_), Some(_)) => (0..total).filter(|&k| !stamps.has_landed(base + k)).count(),
        (_, Some(at)) => total - at,
    };
    Run { latency, emit, lost, cut }
}

/// `p`-th percentile of a sorted sample, the core-hop harness's definition (`n * p`, floored).
fn pct(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

/// p50, p99, p99.9 and max of a sample; `None` for an empty one.
pub fn quantiles(sample: &[u64]) -> Option<[u64; 4]> {
    let mut sorted = sample.to_vec();
    sorted.sort_unstable();
    let max = *sorted.last()?;
    Some([pct(&sorted, 0.50), pct(&sorted, 0.99), pct(&sorted, 0.999), max])
}

fn shown(q: Option<[u64; 4]>, k: usize) -> String {
    q.map_or_else(|| "-".to_string(), |q| q[k].to_string())
}

/// The greppable line of one cell: `leg` names what was measured, `shape` how it arrived.
pub fn line(leg: &str, shape: Shape, run: &Run) -> String {
    let (l, e) = (quantiles(&run.latency), quantiles(&run.emit));
    format!(
        "HOP-LATENCY leg={leg} shape={} n={} lost={} p50_ns={} p99_ns={} p999_ns={} max_ns={} \
         emit_p50_ns={} emit_p99_ns={} emit_max_ns={}\n",
        shape.word(),
        run.latency.len(),
        run.lost,
        shown(l, 0),
        shown(l, 1),
        shown(l, 2),
        shown(l, 3),
        shown(e, 0),
        shown(e, 1),
        shown(e, 3),
    )
}

/// The paired difference of one cell against its in-process baseline, quantile by quantile (the two runs
/// are independent, so it is a difference of quantiles, not a quantile of differences). `-` when either
/// side kept no sample.
pub fn delta_line(leg: &str, baseline: &str, shape: Shape, hop: &Run, base: &Run) -> String {
    let (h, b) = (quantiles(&hop.latency), quantiles(&base.latency));
    let d = |k: usize| match (h, b) {
        (Some(h), Some(b)) => (h[k] as i64 - b[k] as i64).to_string(),
        _ => "-".to_string(),
    };
    format!(
        "HOP-LATENCY-DELTA leg={leg} baseline={baseline} shape={} dp50_ns={} dp99_ns={} dp999_ns={} \
         dmax_ns={}\n",
        shape.word(),
        d(0),
        d(1),
        d(2),
        d(3),
    )
}

/// Written straight to the stderr HANDLE: libtest swallows `eprintln!` of a passing test unless the runner
/// passes `--nocapture`, and a measurement nobody can read is no measurement (the same reason the core-hop
/// harness writes its line this way).
pub fn report(text: &str) {
    let mut err = std::io::stderr();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

/// The box this ran on, for the line that goes with the numbers: load, the cores this process may use.
pub fn box_line(tag: &str) -> String {
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_else(|_| "n/a".to_string());
    let allowed = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("Cpus_allowed_list")).map(|l| l.replace('\t', " "))
        })
        .unwrap_or_else(|| "Cpus_allowed_list: n/a".to_string());
    let cpus = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    format!(
        "HOP-LATENCY-BOX {tag} loadavg=\"{}\" {allowed} available_parallelism={cpus}\n",
        load.trim()
    )
}

/// A SPIN-WAITING receiver: the thread that stands for the core's ingest, polling `poll` flat out and
/// handing each message to `on` (which stamps the landing). Spinning, not parking, so the measured wake
/// is the hop's and not the receiver's own scheduler latency. Stopped and joined on drop.
pub struct Spin {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Spin {
    /// Start the receiver thread under `name`.
    pub fn start<T: Send + 'static>(
        name: &str,
        mut poll: impl FnMut() -> Option<T> + Send + 'static,
        mut on: impl FnMut(T) + Send + 'static,
    ) -> Spin {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    match poll() {
                        Some(message) => on(message),
                        None => std::hint::spin_loop(),
                    }
                }
            })
            .expect("spawn the hop receiver");
        Spin { stop, thread: Some(thread) }
    }
}

impl Drop for Spin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
