//! The Options tool's CLIENT half: what the tool asks the backend's datahub for, how often, and how
//! an answer lands in [`ToolData`].
//!
//! # Where the data comes from
//!
//! The desktop opens NO socket to Deribit (owner ruling, 2026-10-11). The chains are the datahub's
//! `Request::OptionsChain` snapshot: the SERVER holds the Deribit provider and its two WebSocket
//! feeds and folds the streamed rows itself (`vike_options::apply_ticker_to_chains` and its
//! markprice twin), so this side only asks and merges. The index the Greeks tool looks option
//! names up in is built from the same datahub's `Request::VenueCatalog` listing for `"deribit"`
//! ([`crate::data::catalog_wire::fetch_venue_catalog`]) by `vike_options::OptionIndex::from_instruments`,
//! never parsed from a name.
//!
//! ⚠ **There is NO fallback.** A server that does not advertise the verb, a lane its operator left
//! unarmed, a refusal, no backend and a dead link each put ONE sentence in [`ToolData::opt_status`]
//! and keep the last good chains; nothing ever dials Deribit instead (owner ruling).
//!
//! # What is asked: the interest
//!
//! [`OptionsLink`] is the UI-to-poll-thread half. Every frame an Options window draws, it records
//! its selection through [`options_selection`]; every frame the Greeks tool draws, it records each
//! held option's expiry ([`OptionsLink::want_positions`]). A pick lives [`INTEREST_TTL`]: a closed
//! window stops being asked for within three polls, and with no window open the thread dials
//! nothing at all. A NEW pick wakes the thread at once, so a click on an expiry the last answer did
//! not carry lands in one round trip rather than one cadence. The pass itself wakes the UI, which
//! is what keeps an open window's picks renewed while nobody touches it.
//!
//! [`PollState::plan`] turns the live picks into one request per underlying: the selected (or
//! front) expiry plus the held positions' expiries. Every other underlying the tool knows of is
//! asked for its front expiry every [`BACKGROUND_EVERY`], so the selector has chains to show.
//!
//! # Cadence and the connection
//!
//! [`OPT_POLL_MS`]: one pass a second. The server answers from its own folded snapshot, so a pass
//! is a read of that box's memory, never a venue call: the venue budget is the server's. The
//! connection is HELD across passes ([`super::options_wire`]), unlike the catalog refresh's one dial
//! per press, because a keyed handshake is a nonce round trip and paying it every second for the
//! life of a window would be most of the traffic. It is dropped and re-dialled after any error and
//! whenever the dial (the active backend's datahub address or key name) changes.
//!
//! # Last good
//!
//! An answered underlying REPLACES its bundle whole: each request names every expiry wanted for it,
//! so nothing wanted is lost. An underlying whose request was refused, failed or not sent keeps the
//! bundle it had. The index follows the same rule: a listing that fails or is refused keeps the
//! last good index.
//!
//! ⚠ Last good is a rule about ONE server's answers. When the dial names another datahub (a
//! backend switch, a changed key name, no datahub at all), [`PollState::observe_dial`] clears the
//! chains, the selector, the index and the status at once: another server's data is never shown
//! as the new one's last good.
//!
//! # The selector
//!
//! Every reply carries the set the server SERVES (`served_underlyings`); the selector lists it, and
//! each served underlying no window shows is asked for its front expiry every [`BACKGROUND_EVERY`].
//! Until a reply has named the set, the catalog's option underlyings stand in for it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_datahub_client::options::{
    MAX_OPTIONS_EXPIRIES, OptionsChainOutcome, OptionsChainReply, OptionsRefusal,
};
use vike_marketdata::UnderlyingChains;
use vike_model::{Contract, Instrument};
use vike_options::OptionIndex;

use super::options_wire::{OptionsPass, OptionsWire, failed_line, live_line, waiting_line};
use super::view::ToolData;
use crate::data::catalog_wire::{
    CatalogDial, CatalogFetchReport, fetch_venue_catalog, render_catalog_fetch_status,
};

/// How often the poll thread asks, in milliseconds. One second: the server folds the venue's
/// 100 ms ticker and markprice feeds into the snapshot it answers from, so a faster ask would read
/// the same snapshot twice and a slower one would show a stale grid between asks.
pub(crate) const OPT_POLL_MS: u64 = 1000;

/// The one venue the Options tool reads; a data-source KEY (an address, not a spelling).
pub(crate) const OPTIONS_VENUE: &str = "deribit";

/// How long a recorded pick stays live without being recorded again: three polls, so a window
/// that is redrawn once per pass (the pass's own UI wake) never lapses between two of them.
const INTEREST_TTL: Duration = Duration::from_millis(3 * OPT_POLL_MS);

/// How often an underlying no window is showing is asked for its front expiry — the asks that put
/// it in the selector at all.
const BACKGROUND_EVERY: Duration = Duration::from_secs(10);

/// One thing a window wants this pass: an underlying (`None`: the tool's default, the first known
/// one by `vike_options::underlying_rank`) and an expiry (`None`: the front).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Pick {
    pub(crate) underlying: Option<String>,
    pub(crate) expiry: Option<String>,
}

/// **The UI's half of the poll thread**: the picks the windows recorded and the channel that wakes
/// the thread. Cheap to clone (an `Arc` and a `Sender`), and every clone is the same link, which is
/// why it rides inside [`ToolData`] and reaches every tool body through the frame's copy of it.
#[derive(Clone)]
pub struct OptionsLink {
    picks: Arc<Mutex<HashMap<Pick, Instant>>>,
    wake: Sender<()>,
}

impl Default for OptionsLink {
    /// A link no thread listens on: picks are recorded and every wake is dropped. What
    /// `ToolData::default()` holds until `spawn_tool_fetchers` installs the live one.
    fn default() -> Self {
        Self::new(std::sync::mpsc::channel().0)
    }
}

impl OptionsLink {
    /// A link whose wakes go to `wake` — the receiving half is the poll thread's.
    #[must_use]
    pub fn new(wake: Sender<()>) -> Self {
        Self { picks: Arc::default(), wake }
    }

    /// The Refresh pill: ask NOW instead of at the next cadence. A closed channel (the thread gone
    /// at shutdown) is ignored, so the UI can never panic on it.
    pub fn refresh(&self) {
        let _ = self.wake.send(());
    }

    /// Record that a window shows `underlying` (`None`: the default) at `expiry` (`None`: the
    /// front). Called every frame the window draws; only a pick that was not already live wakes
    /// the thread.
    pub fn want(&self, underlying: Option<&str>, expiry: Option<&str>) {
        let pick =
            Pick { underlying: underlying.map(str::to_string), expiry: expiry.map(str::to_string) };
        self.want_at(pick, Instant::now());
    }

    /// Record what the Greeks tool needs to price `names`: each indexed option's own expiry, and
    /// the front expiry (its spot) for a perpetual's or dated future's underlying. A name the index
    /// does not hold asks for nothing, exactly as it prices as `unpriced`. The default underlying
    /// is recorded too, so an open Greeks window keeps the index and a spot coming with no
    /// position at all.
    pub fn want_positions<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
        index: &OptionIndex,
    ) {
        let now = Instant::now();
        self.want_at(Pick { underlying: None, expiry: None }, now);
        for name in names {
            let pick = if let Some(addr) = index.address(name) {
                Pick {
                    underlying: Some(addr.underlying.to_string()),
                    expiry: Some(addr.expiry_iso),
                }
            } else if let Some(underlying) = index.delta_one_underlying(name) {
                Pick { underlying: Some(underlying.to_string()), expiry: None }
            } else {
                continue;
            };
            self.want_at(pick, now);
        }
    }

    /// [`Self::want`] at a given instant — the clock is a parameter so the TTL is testable.
    pub(crate) fn want_at(&self, pick: Pick, now: Instant) {
        let fresh = {
            let mut picks = lock(&self.picks);
            let fresh = picks
                .get(&pick)
                .is_none_or(|at| now.saturating_duration_since(*at) >= INTEREST_TTL);
            picks.insert(pick, now);
            fresh
        };
        if fresh {
            self.refresh();
        }
    }

    /// The picks still live at `now`; the lapsed ones are forgotten.
    pub(crate) fn live_picks(&self, now: Instant) -> Vec<Pick> {
        let mut picks = lock(&self.picks);
        picks.retain(|_, at| now.saturating_duration_since(*at) < INTEREST_TTL);
        picks.keys().cloned().collect()
    }
}

/// **The Options arm's selection**: `vike_options::resolve_options_selection` over the fetched
/// bundles, then the window's pick recorded on [`ToolData::opt_link`].
///
/// ⚠ One rule is added, and it is what a PARTIAL bundle needs: a bundle carries the chains of the
/// expiries that were ASKED for, and the strip lists every expiry the server lists. An expiry the
/// strip lists but whose chain has not arrived yet is PENDING, not invalid, so the pick survives
/// and is asked for; clearing it (the selector's rule for an expiry it does not hold) would undo
/// every click on an expiry nobody had asked for yet. An expiry the server does not list is still
/// cleared, so an underlying switch never strands the grid on a date the new book lacks.
pub fn options_selection<'a>(
    td: &'a ToolData,
    underlying_sel: &Option<String>,
    expiry_sel: &mut Option<String>,
) -> Option<&'a UnderlyingChains> {
    let pick = expiry_sel.clone();
    let bundle = vike_options::resolve_options_selection(
        &td.opt_by_underlying,
        &td.opt_underlyings,
        underlying_sel,
        expiry_sel,
    );
    if expiry_sel.is_none()
        && let (Some(bundle), Some(pick)) = (bundle, pick)
        && bundle.expiries.iter().any(|e| e.date == pick)
    {
        *expiry_sel = Some(pick);
    }
    td.opt_link.want(underlying_sel.as_deref(), expiry_sel.as_deref());
    // The selector lists every SERVED underlying, so a pick can name one that has not answered
    // yet: show nothing for it rather than another underlying's chains under its name.
    let pending = underlying_sel
        .as_ref()
        .is_some_and(|u| !td.opt_by_underlying.contains_key(u) && td.opt_underlyings.contains(u));
    if pending { None } else { bundle }
}

/// What the poll thread has learned between passes: which underlyings exist, what the server told
/// it about its bounds, and when it last asked for the background underlyings and the index.
#[derive(Debug, Default)]
pub(crate) struct PollState {
    /// The option underlyings of the last adopted deribit listing.
    pub(crate) catalog_underlyings: BTreeSet<String>,
    /// The underlyings the server says it serves (every reply's `served_underlyings`). When
    /// known, it replaces the catalog's set: the server's answer is about the server.
    pub(crate) served_underlyings: BTreeSet<String>,
    /// The dial (address, key name) the last pass ran under; `None` before the first pass.
    dial_seen: Option<Option<(String, String)>>,
    /// The most expiries one request may name (learned from a `TooManyExpiries` refusal).
    pub(crate) expiry_cap: Option<usize>,
    /// When the background underlyings were last asked for.
    last_background: Option<Instant>,
    /// When the index was last fetched, and from which datahub address (`None`: no backend).
    index_attempt: Option<(Instant, Option<String>)>,
    /// Why the index is empty, when a listing was refused, failed or truncated.
    pub(crate) index_note: Option<String>,
}

impl PollState {
    /// Every underlying the tool knows of, in selector order: the server's set (or, until it named
    /// one, the catalog's) plus every underlying that has answered.
    pub(crate) fn known(&self, by_underlying: &BTreeMap<String, UnderlyingChains>) -> Vec<String> {
        let source = if self.served_underlyings.is_empty() {
            &self.catalog_underlyings
        } else {
            &self.served_underlyings
        };
        let set: BTreeSet<&String> = source.iter().chain(by_underlying.keys()).collect();
        let mut known: Vec<String> = set.into_iter().cloned().collect();
        known.sort_by_key(|u| vike_options::underlying_rank(u));
        known
    }

    /// **This pass's requests**: `(underlying, expiries)` in selector order, from the live
    /// `picks`. An empty expiry list asks for the server's front expiry. Empty when no window is
    /// open, or when no underlying is known yet to resolve a default against.
    pub(crate) fn plan(
        &mut self,
        picks: &[Pick],
        by_underlying: &BTreeMap<String, UnderlyingChains>,
        now: Instant,
    ) -> Vec<(String, Vec<String>)> {
        if picks.is_empty() {
            return Vec::new();
        }
        let known = self.known(by_underlying);
        // underlying -> (the front is wanted, the explicit expiries wanted)
        let mut wanted: BTreeMap<String, (bool, BTreeSet<String>)> = BTreeMap::new();
        for pick in picks {
            let Some(underlying) = pick.underlying.clone().or_else(|| known.first().cloned())
            else {
                continue;
            };
            let slot = wanted.entry(underlying).or_default();
            match &pick.expiry {
                None => slot.0 = true,
                Some(expiry) => {
                    slot.1.insert(expiry.clone());
                }
            }
        }
        let background_due = self
            .last_background
            .is_none_or(|at| now.saturating_duration_since(at) >= BACKGROUND_EVERY);
        if background_due && !known.is_empty() {
            self.last_background = Some(now);
            for underlying in &known {
                wanted.entry(underlying.clone()).or_insert((true, BTreeSet::new()));
            }
        }
        let mut plan: Vec<(String, Vec<String>)> = wanted
            .into_iter()
            .map(|(underlying, (front, explicit))| {
                let bundle = by_underlying.get(&underlying);
                let expiries = request_expiries(front, explicit, bundle, self.expiry_cap);
                (underlying, expiries)
            })
            .collect();
        plan.sort_by_key(|(u, _)| vike_options::underlying_rank(u));
        plan
    }

    /// Take what a reply says about the server: the set it serves (every answer carries it, a
    /// refusal included; empty when the server serves the venue nothing) and, from a
    /// `TooManyExpiries` refusal, its expiry bound. The sentence is [`Self::merge`]'s business.
    pub(crate) fn learn(&mut self, reply: &OptionsChainReply) {
        self.served_underlyings = reply.served_underlyings.iter().cloned().collect();
        if let OptionsChainOutcome::Refused(OptionsRefusal::TooManyExpiries { max }) =
            &reply.outcome
        {
            self.expiry_cap = Some(*max);
        }
    }

    /// **The selector's order**: the underlyings the server SERVES, ranked, once an answer has
    /// named them; until then (or from a server that serves the venue nothing), the ones that
    /// answered. A served underlying no window shows yet is fetched within [`BACKGROUND_EVERY`].
    pub(crate) fn selector(
        &self,
        by_underlying: &BTreeMap<String, UnderlyingChains>,
    ) -> Vec<String> {
        let mut order: Vec<String> = if self.served_underlyings.is_empty() {
            by_underlying.keys().cloned().collect()
        } else {
            self.served_underlyings.iter().cloned().collect()
        };
        order.sort_by_key(|u| vike_options::underlying_rank(u));
        order
    }

    /// **A different datahub is a different world.** When the dial names another address or key
    /// name than the last pass saw (a backend switch, a new `config.datahub_addr`, or no datahub
    /// at all), everything the old server answered is CLEARED at once — the chains, the selector,
    /// the index and the status — together with everything learned from it. Its data must never
    /// be shown as the new server's last good. Returns whether it cleared. The first dial a poller
    /// ever sees clears nothing: there is nothing yet to clear.
    pub(crate) fn observe_dial(&mut self, td: &mut ToolData, dial: Option<&CatalogDial>) -> bool {
        let identity = dial.map(|d| (d.addr.clone(), d.key_name.clone()));
        let changed = self.dial_seen.as_ref().is_some_and(|seen| *seen != identity);
        self.dial_seen = Some(identity);
        if !changed {
            return false;
        }
        td.opt_by_underlying.clear();
        td.opt_underlyings.clear();
        td.opt_index = OptionIndex::default();
        td.opt_status.clear();
        let dial_seen = self.dial_seen.take();
        *self = Self { dial_seen, ..Self::default() };
        true
    }

    /// Whether the index is due now: `vike_options::index_refresh_due` decides, over the time since
    /// the last attempt AT THIS ADDRESS — a new datahub is a first attempt, so connecting a backend
    /// does not wait out the empty index's retry period.
    fn index_due(&self, td: &ToolData, addr: Option<&str>, now: Instant) -> bool {
        let since = match &self.index_attempt {
            Some((at, asked)) if asked.as_deref() == addr => {
                Some(now.saturating_duration_since(*at))
            }
            _ => None,
        };
        let due = vike_options::index_refresh_due(&td.opt_index, &td.opt_by_underlying, since);
        // The decision is pure; the gap it acted on is logged HERE, once per refresh decision and
        // never per answer.
        if due
            && !td.opt_index.is_empty()
            && let Some((missing, first)) = td.opt_index.missing_from(&td.opt_by_underlying)
        {
            tracing::debug!(
                missing,
                first = %first,
                "options: grid instruments absent from the option index (the Greeks tool prices \
                 them as unpriced); refetching it"
            );
        }
        due
    }

    /// One listing fetch's result: the index to adopt, or `None` (keep the last good one) with the
    /// reason kept for the status line.
    pub(crate) fn adopt_index(&mut self, report: CatalogFetchReport) -> Option<OptionIndex> {
        match report {
            CatalogFetchReport::Listed { instruments, truncated, .. } => {
                self.catalog_underlyings = option_underlyings(&instruments);
                self.index_note = truncated.then(|| {
                    "the datahub's deribit instrument list was TRUNCATED at the wire cap, so the \
                     options past it price as unpriced"
                        .to_string()
                });
                Some(OptionIndex::from_instruments(&instruments))
            }
            other => {
                let note = render_catalog_fetch_status(&other);
                tracing::warn!("options: the deribit instrument list was not adopted: {note}");
                self.index_note = Some(note);
                None
            }
        }
    }

    /// Fold one pass into the tool's data: each answered underlying replaces its bundle, the
    /// selector order is recomputed, and the status is the FIRST sentence that is not "live" (a
    /// refusal, a failure, or the reason nothing was asked), else the live line.
    pub(crate) fn merge(&mut self, td: &mut ToolData, pass: OptionsPass) {
        let (addr, replies) = match pass {
            OptionsPass::Asked { addr, replies } => (addr, replies),
            not_asked => {
                if let Some(line) = not_asked.not_asked_line() {
                    td.opt_status = line;
                }
                return;
            }
        };
        let mut problem: Option<String> = None;
        for (underlying, answer) in replies {
            match answer {
                Ok(reply) => {
                    self.learn(&reply);
                    match reply.outcome {
                        OptionsChainOutcome::Chains(bundle) => {
                            td.opt_by_underlying.insert(underlying, bundle);
                        }
                        outcome => {
                            let line = OptionsChainReply { outcome, ..reply }.describe();
                            problem.get_or_insert(line);
                        }
                    }
                }
                Err(error) => {
                    problem.get_or_insert_with(|| failed_line(&addr, &underlying, &error));
                }
            }
        }
        td.opt_underlyings = self.selector(&td.opt_by_underlying);
        td.opt_status = match (problem, td.opt_index.is_empty(), &self.index_note) {
            (Some(line), _, _) => line,
            (None, true, Some(note)) => {
                format!("{} — the Greeks tool cannot price yet: {note}", live_line(&addr))
            }
            (None, _, _) => live_line(&addr),
        };
    }
}

/// The expiry list ONE request names. Empty asks for the server's front expiry, which is all a
/// front-only want needs. With explicit expiries the front rides along by its DATE, which the last
/// answer named; until one has, the front is asked for alone, and the explicit ones join the next
/// pass. The cap is the server's `MAX_OPTIONS_EXPIRIES`, or the smaller number a `TooManyExpiries`
/// refusal named; it truncates the tail (the explicit expiries, latest first to go).
fn request_expiries(
    front: bool,
    explicit: BTreeSet<String>,
    bundle: Option<&UnderlyingChains>,
    cap: Option<usize>,
) -> Vec<String> {
    if explicit.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if front {
        match bundle.map(|b| b.default_expiry.as_str()).filter(|d| !d.is_empty()) {
            Some(default) => out.push(default.to_string()),
            None => return Vec::new(),
        }
    }
    for expiry in explicit {
        if !out.contains(&expiry) {
            out.push(expiry);
        }
    }
    out.truncate(cap.unwrap_or(MAX_OPTIONS_EXPIRIES).clamp(1, MAX_OPTIONS_EXPIRIES));
    out
}

/// The underlyings of the deribit OPTIONS in a listing (the payload's own field, never the name).
fn option_underlyings(instruments: &[Instrument]) -> BTreeSet<String> {
    instruments
        .iter()
        .filter(|i| i.venue == OPTIONS_VENUE)
        .filter_map(|i| match &i.contract {
            Contract::Option(spec) => spec.underlying.clone().filter(|u| !u.is_empty()),
            _ => None,
        })
        .collect()
}

/// **The poll thread's whole state, one pass at a time** — public so the loopback suite
/// (`crates/vike-app-core/tests/options_wire.rs`) drives the exact pass the thread runs.
#[derive(Default)]
pub struct OptionsPoller {
    state: PollState,
    wire: OptionsWire,
}

impl OptionsPoller {
    /// A poller whose held connection bounds each options reply by `reply_timeout` instead of
    /// `OPTIONS_REPLY_TIMEOUT` — the seam the loopback suite drives a silent datahub through in
    /// milliseconds. The poll thread uses [`Default`].
    pub fn with_reply_timeout(reply_timeout: Duration) -> Self {
        Self { state: PollState::default(), wire: OptionsWire::with_reply_timeout(reply_timeout) }
    }

    /// **One pass**: read the live picks off `shared`'s link; with none, ask nothing and return
    /// `false`. Otherwise refresh the index when it is due, plan, ask the datahub over the held
    /// connection and merge the answers, then return `true` (the caller wakes the UI, which is what
    /// renews an open window's picks). Every network call is made OUTSIDE the lock, so a slow
    /// server never stalls the frame.
    pub fn run_once(
        &mut self,
        shared: &Mutex<ToolData>,
        dial: Option<&CatalogDial>,
        now: Instant,
    ) -> bool {
        // A switched datahub clears the old one's data before anything else, window open or not.
        let link = {
            let mut td = lock(shared);
            self.state.observe_dial(&mut td, dial);
            td.opt_link.clone()
        };
        let picks = link.live_picks(now);
        if picks.is_empty() {
            return false;
        }
        let addr = dial.map(|d| d.addr.as_str());
        let index_due = self.state.index_due(&lock(shared), addr, now);
        if index_due {
            self.state.index_attempt = Some((now, addr.map(str::to_string)));
            let report = fetch_venue_catalog(dial, OPTIONS_VENUE);
            if let Some(index) = self.state.adopt_index(report) {
                lock(shared).opt_index = index;
            }
        }
        let plan = {
            let td = lock(shared);
            self.state.plan(&picks, &td.opt_by_underlying, now)
        };
        if plan.is_empty() {
            lock(shared).opt_status = waiting_line(self.state.index_note.as_deref());
            return true;
        }
        let pass = self.wire.pass(dial, &plan);
        self.state.merge(&mut lock(shared), pass);
        true
    }
}

/// Why the poll thread woke.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wake {
    /// A Refresh press or a new pick; any further queued wakes were drained with it.
    Refresh,
    /// The cadence elapsed.
    Cadence,
    /// Every sender is gone; the cadence was slept out rather than spun.
    Closed,
}

/// Wait for the next pass: the `cadence`, or a wake on `rx`, whichever comes first. A burst of
/// wakes coalesces to ONE pass, and a closed channel sleeps the cadence instead of busy-looping.
pub(crate) fn wait_for_next_poll(rx: &Receiver<()>, cadence: Duration) -> Wake {
    match rx.recv_timeout(cadence) {
        Ok(()) => {
            while rx.try_recv().is_ok() {}
            Wake::Refresh
        }
        Err(RecvTimeoutError::Timeout) => Wake::Cadence,
        Err(RecvTimeoutError::Disconnected) => {
            std::thread::sleep(cadence);
            Wake::Closed
        }
    }
}

/// Lock, recovering a poisoned guard: a panic elsewhere must not take the Options tool with it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A deribit option [`Instrument`] for tests, built the way the catalog builds one: the margining
/// follows the settlement (coin-settled = inverse, else linear), the expiry is 08:00 UTC of
/// `expiry_iso`.
#[cfg(test)]
pub(crate) fn test_option(
    name: &str,
    underlying: &str,
    expiry_iso: &str,
    strike: f64,
    right: vike_marketdata::OptionKind,
    settle: &str,
) -> Instrument {
    let margining = if settle == underlying {
        vike_model::Margining::Inverse
    } else {
        vike_model::Margining::Linear
    };
    Instrument::new(
        OPTIONS_VENUE,
        name,
        underlying,
        settle,
        Contract::Option(vike_model::OptionSpec {
            right,
            strike,
            expiry_ms: vike_options::expiry_ms(expiry_iso),
            style: vike_model::ExerciseStyle::European,
            margining,
            settle: settle.to_string(),
            value_ccy: Some(underlying.to_string()),
            underlying: Some(underlying.to_string()),
        }),
    )
}

/// A deribit perpetual [`Instrument`] for tests (`base` is its underlying).
#[cfg(test)]
pub(crate) fn test_delta_one(name: &str, base: &str) -> Instrument {
    Instrument::new(
        OPTIONS_VENUE,
        name,
        base,
        "USD",
        Contract::Perp(vike_model::PerpSpec {
            margining: vike_model::Margining::Inverse,
            settle: base.to_string(),
            value_ccy: Some("USD".to_string()),
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
}

/// The pure halves of the poll: the link's TTL and wake, the plan, what a reply teaches, the merge
/// and its status sentences, the selection over a partial bundle and the index adoption.
#[path = "options_tests.rs"]
#[cfg(test)]
mod options_tests;

/// The poll thread's wait: a Refresh wakes it early, a burst coalesces to one pass, the cadence
/// elapses with no signal, and a closed channel never spins.
#[path = "refresh_wake_tests.rs"]
#[cfg(test)]
mod refresh_wake_tests;
