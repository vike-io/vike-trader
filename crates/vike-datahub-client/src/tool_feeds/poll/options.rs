//! The Options half of the datahub poll: what the Options tool asks the backend's datahub for, how
//! often, and how an answer lands in [`DatahubPollSnapshot`]. It runs on the shared
//! [`DatahubPoller`] (`DatahubPoller<OptionsPollState>`), its own thread beside the News and
//! Calendar half's (`super::spawn`).
//!
//! # Where the data comes from
//!
//! The desktop opens NO socket to Deribit (owner ruling, 2026-10-11). The chains are the datahub's
//! `Request::OptionsChain` snapshot: the SERVER holds the Deribit provider and its two WebSocket
//! feeds and folds the streamed rows itself (`vike_options::apply_ticker_to_chains` and its
//! markprice twin), so this side only asks and merges. The index the Greeks tool looks option
//! names up in is built from the same datahub's `Request::VenueCatalog` listing for `"deribit"`
//! ([`crate::catalog_fetch::fetch_venue_catalog`]) by `vike_options::OptionIndex::from_instruments`,
//! never parsed from a name.
//!
//! ⚠ **There is NO fallback.** A server that does not advertise the verb, a lane its operator left
//! unarmed, a refusal, no backend and a dead link each put ONE sentence in
//! [`DatahubPollSnapshot::options_status`] and keep the last good chains; nothing ever dials Deribit
//! instead (owner ruling).
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
//! [`OptionsPollState::plan`] turns the live picks into one request per underlying: the selected (or
//! front) expiry plus the held positions' expiries. Every other underlying the tool knows of is
//! asked for its front expiry every [`BACKGROUND_EVERY`], so the selector has chains to show.
//!
//! # Cadence and the connection
//!
//! [`OPTIONS_POLL_MS`]: one pass a second. The server answers from its own folded snapshot, so a pass
//! is a read of that box's memory, never a venue call: the venue budget is the server's. The
//! connection is HELD across passes ([`super::connection`]), unlike the catalog refresh's one dial
//! per press, because a keyed handshake is a nonce round trip and paying it every second for the
//! life of a window would be most of the traffic. It is dropped and re-dialled after any error and
//! whenever the dial (the active backend's datahub address or key name) changes.
//!
//! # Last good
//!
//! An answered underlying REPLACES its bundle whole: each request names every expiry wanted for it,
//! so nothing wanted is lost. An underlying whose request was refused, failed or not sent keeps the
//! bundle it had. A failed LINK is transient: the status is the quiet reconnecting line for the
//! first passes of a streak and names the cause after that ([`crate::link`]). The index follows the same rule: a listing that fails or is refused keeps the
//! last good index.
//!
//! ⚠ Last good is a rule about ONE server's answers. When the dial names another datahub (a
//! backend switch, a changed key name, no datahub at all), [`OptionsPollState::observe_dial`] clears the
//! chains, the selector, the index and the status at once: another server's data is never shown
//! as the new one's last good.
//!
//! # The selector
//!
//! Every reply carries the set the server SERVES (`served_underlyings`); the selector lists it in the
//! server's order, and each served underlying no window shows is asked for its front expiry every
//! [`BACKGROUND_EVERY`]. Until a reply has named the set, the catalog's option underlyings stand in
//! for it. No name is ranked on this side: both lists come in the listing's order (most listed
//! options first), so the default underlying is the venue's biggest book, not a compiled symbol.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_marketdata::OptionChainSet;
use vike_model::{Contract, Instrument};
use vike_options::OptionIndex;
use vike_utility::mutex::lock;

use super::connection::{DatahubPollConnection, OPTIONS_REPLY_TIMEOUT};
use super::{DatahubPollSnapshot, DatahubPoller};
use crate::FEATURE_OPTIONS_CHAIN;
use crate::catalog_fetch::{CatalogFetchReport, fetch_venue_catalog, render_catalog_fetch_status};
use crate::dial::{DatahubDial, NO_BACKEND_NOTE};
use crate::link::{DatahubLinkError, DatahubLinkStreak, RECONNECTING_LINE};
use crate::options::{
    MAX_OPTIONS_EXPIRIES, OptionsChainOutcome, OptionsChainReply, OptionsRefusal,
};

/// How often the poll thread asks, in milliseconds. One second: the server folds the venue's
/// 100 ms ticker and markprice feeds into the snapshot it answers from, so a faster ask would read
/// the same snapshot twice and a slower one would show a stale grid between asks.
pub(crate) const OPTIONS_POLL_MS: u64 = 1000;

/// The one venue the Options tool reads; a data-source KEY (an address, not a spelling).
pub(crate) const OPTIONS_VENUE: &str = "deribit";

/// How long a recorded pick stays live without being recorded again: three polls, so a window
/// that is redrawn once per pass (the pass's own UI wake) never lapses between two of them.
const INTEREST_TTL: Duration = Duration::from_millis(3 * OPTIONS_POLL_MS);

/// How often an underlying no window is showing is asked for its front expiry — the asks that put
/// it in the selector at all.
const BACKGROUND_EVERY: Duration = Duration::from_secs(10);

/// How one Options pass ended. The first three asked nothing; `Asked` carries one answer per
/// request, in plan order, and stops at the first `Err` (the connection is dropped there).
#[derive(Debug)]
pub(crate) enum OptionsPass {
    /// No datahub address is resolved, so nothing was dialled.
    NoBackend,
    /// The dial (or the handshake riding it) failed; nothing was sent.
    ConnectFailed { addr: String, error: DatahubLinkError },
    /// The server does not advertise [`FEATURE_OPTIONS_CHAIN`]; refused client-side, no frame sent.
    ServerUnsupported { addr: String, features: Vec<String> },
    /// The requests were sent: `(underlying, answer)` per request.
    Asked { addr: String, replies: Vec<(String, Result<OptionsChainReply, DatahubLinkError>)> },
}

impl OptionsPass {
    /// The status sentence of a pass that asked nothing; `None` for [`Self::Asked`], whose
    /// sentence comes from its answers.
    pub(crate) fn not_asked_line(&self) -> Option<String> {
        match self {
            Self::NoBackend => Some(NO_BACKEND_NOTE.to_string()),
            Self::ConnectFailed { addr, error } => Some(format!(
                "the datahub at {addr} could not be reached ({}) — no option chain was \
                 requested.",
                error.cause
            )),
            Self::ServerUnsupported { addr, features } => Some(format!(
                "the datahub at {addr} serves no option chains: it does not advertise \
                 \"{FEATURE_OPTIONS_CHAIN}\" (it advertised [{}]) — nothing was sent. That server \
                 predates the verb: upgrade it.",
                features.join(", ")
            )),
            Self::Asked { .. } => None,
        }
    }
}

/// A request that was sent and came back as an error (the server's `Response::Error`, or a
/// transport failure mid-request).
fn failed_line(addr: &str, underlying: &str, error: &str) -> String {
    format!("the datahub at {addr} could not answer the {underlying} option chain: {error}")
}

/// Every request answered with chains.
fn live_line(addr: &str) -> String {
    format!("LIVE · Deribit via the datahub at {addr}")
}

/// A window is open, but no underlying is known yet to ask for: the instrument list that names
/// them has not been adopted. `note` is that list's own sentence, when it was refused or failed.
fn waiting_line(note: Option<&str>) -> String {
    match note {
        Some(note) => {
            format!("no option underlying is known yet — the deribit instrument list: {note}")
        }
        None => "waiting for the datahub's deribit instrument list".to_string(),
    }
}

/// One thing a window wants this pass: an underlying (`None`: the tool's default, the first known
/// one in the listing's order) and an expiry (`None`: the front).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OptionsPick {
    pub(crate) underlying: Option<String>,
    pub(crate) expiry: Option<String>,
}

/// **The UI's half of the poll thread**: the picks the windows recorded and the channel that wakes
/// the thread. Cheap to clone (an `Arc` and a `Sender`), and every clone is the same link, which is
/// why it rides inside [`DatahubPollSnapshot`] and reaches every tool body through the frame's copy of it.
#[derive(Clone)]
pub struct OptionsLink {
    picks: Arc<Mutex<HashMap<OptionsPick, Instant>>>,
    wake: Sender<()>,
}

impl Default for OptionsLink {
    /// A link no thread listens on: picks are recorded and every wake is dropped. What
    /// `DatahubPollSnapshot::default()` holds until `spawn_tool_fetchers` installs the live one.
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
        let pick = OptionsPick {
            underlying: underlying.map(str::to_string),
            expiry: expiry.map(str::to_string),
        };
        self.want_at(pick, Instant::now());
    }

    /// Record what the Greeks tool needs to price `names`: each indexed option's own expiry, and
    /// the front expiry (its spot) for a perpetual's or dated future's underlying. A name the index
    /// does not hold asks for nothing, exactly as it prices as `unpriced`. The default underlying
    /// is recorded too, so an open Portfolio Greeks tab keeps the index and a spot coming with no
    /// position at all.
    pub fn want_positions<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
        index: &OptionIndex,
    ) {
        let now = Instant::now();
        self.want_at(OptionsPick { underlying: None, expiry: None }, now);
        for name in names {
            let pick = if let Some(addr) = index.address(name) {
                OptionsPick {
                    underlying: Some(addr.underlying.to_string()),
                    expiry: Some(addr.expiry_iso),
                }
            } else if let Some(underlying) = index.delta_one_underlying(name) {
                OptionsPick { underlying: Some(underlying.to_string()), expiry: None }
            } else {
                continue;
            };
            self.want_at(pick, now);
        }
    }

    /// [`Self::want`] at a given instant — the clock is a parameter so the TTL is testable.
    pub(crate) fn want_at(&self, pick: OptionsPick, now: Instant) {
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
    pub(crate) fn live_picks(&self, now: Instant) -> Vec<OptionsPick> {
        let mut picks = lock(&self.picks);
        picks.retain(|_, at| now.saturating_duration_since(*at) < INTEREST_TTL);
        picks.keys().cloned().collect()
    }
}

/// **The Options arm's selection**: `vike_options::resolve_options_selection` over the fetched
/// bundles, then the window's pick recorded on [`DatahubPollSnapshot::options_link`].
///
/// ⚠ One rule is added, and it is what a PARTIAL bundle needs: a bundle carries the chains of the
/// expiries that were ASKED for, and the strip lists every expiry the server lists. An expiry the
/// strip lists but whose chain has not arrived yet is PENDING, not invalid, so the pick survives
/// and is asked for; clearing it (the selector's rule for an expiry it does not hold) would undo
/// every click on an expiry nobody had asked for yet. An expiry the server does not list is still
/// cleared, so an underlying switch never strands the grid on a date the new book lacks.
pub fn options_selection<'a>(
    td: &'a DatahubPollSnapshot,
    underlying_selection: &Option<String>,
    expiry_selection: &mut Option<String>,
) -> Option<&'a OptionChainSet> {
    let pick = expiry_selection.clone();
    let bundle = vike_options::resolve_options_selection(
        &td.options_by_underlying,
        &td.options_underlyings,
        underlying_selection,
        expiry_selection,
    );
    if expiry_selection.is_none()
        && let (Some(bundle), Some(pick)) = (bundle, pick)
        && bundle.expiries.iter().any(|e| e.date == pick)
    {
        *expiry_selection = Some(pick);
    }
    td.options_link.want(underlying_selection.as_deref(), expiry_selection.as_deref());
    // The selector lists every SERVED underlying, so a pick can name one that has not answered
    // yet: show nothing for it rather than another underlying's chains under its name.
    let pending = underlying_selection.as_ref().is_some_and(|u| {
        !td.options_by_underlying.contains_key(u) && td.options_underlyings.contains(u)
    });
    if pending { None } else { bundle }
}

/// What the Options poll thread has learned between passes: which underlyings exist, what the
/// server told it about its bounds, and when it last asked for the background underlyings and the
/// index. The state of the Options half of [`DatahubPoller`].
#[derive(Debug, Default)]
pub struct OptionsPollState {
    /// The option underlyings of the last adopted deribit listing, in its order
    /// (`option_underlyings`).
    pub(crate) catalog_underlyings: Vec<String>,
    /// The underlyings the server says it serves (every reply's `served_underlyings`), in the
    /// order it lists them. When known, it replaces the catalog's list: the server's answer is
    /// about the server.
    pub(crate) served_underlyings: Vec<String>,
    /// The dial (address, device-key epoch) the last pass ran under; `None` before the first pass.
    dial_seen: Option<Option<(String, u64)>>,
    /// The most expiries one request may name (learned from a `TooManyExpiries` refusal).
    pub(crate) expiry_cap: Option<usize>,
    /// When the background underlyings were last asked for.
    last_background: Option<Instant>,
    /// When the index was last fetched, and from which datahub address (`None`: no backend).
    index_attempt: Option<(Instant, Option<String>)>,
    /// Why the index is empty, when a listing was refused, failed or truncated.
    pub(crate) index_note: Option<String>,
    /// The held chain connection's run of link failures (`crate::link`).
    streak: DatahubLinkStreak,
}

/// The poll's name in its log lines.
const POLL: &str = "options chain";

impl OptionsPollState {
    /// Every underlying the tool knows of, in selector order: the server's list (or, until it named
    /// one, the catalog's) in its own order, then every other underlying that has answered, by name.
    /// No name is ranked here: the order is the listing's.
    pub(crate) fn known(&self, by_underlying: &BTreeMap<String, OptionChainSet>) -> Vec<String> {
        let source = if self.served_underlyings.is_empty() {
            &self.catalog_underlyings
        } else {
            &self.served_underlyings
        };
        let mut known = source.clone();
        known.extend(by_underlying.keys().filter(|u| !source.contains(u)).cloned());
        known
    }

    /// **This pass's requests**: `(underlying, expiries)` in selector order, from the live
    /// `picks`. An empty expiry list asks for the server's front expiry. Empty when no window is
    /// open, or when no underlying is known yet to resolve a default against.
    pub(crate) fn plan(
        &mut self,
        picks: &[OptionsPick],
        by_underlying: &BTreeMap<String, OptionChainSet>,
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
        plan.sort_by_key(|(u, _)| known.iter().position(|k| k == u).unwrap_or(usize::MAX));
        plan
    }

    /// Take what a reply says about the server: the set it serves (every answer carries it, a
    /// refusal included; empty when the server serves the venue nothing) and, from a
    /// `TooManyExpiries` refusal, its expiry bound. The sentence is [`Self::merge`]'s business.
    pub(crate) fn learn(&mut self, reply: &OptionsChainReply) {
        self.served_underlyings = reply.served_underlyings.clone();
        if let OptionsChainOutcome::Refused(OptionsRefusal::TooManyExpiries { max }) =
            &reply.outcome
        {
            self.expiry_cap = Some(*max);
        }
    }

    /// **The selector's order**: the underlyings the server SERVES, in the order it lists them,
    /// once an answer has named them; until then (or from a server that serves the venue nothing),
    /// the ones that answered, in [`Self::known`]'s order. A served underlying no window shows yet
    /// is fetched within [`BACKGROUND_EVERY`].
    pub(crate) fn selector(&self, by_underlying: &BTreeMap<String, OptionChainSet>) -> Vec<String> {
        if self.served_underlyings.is_empty() {
            self.known(by_underlying)
                .into_iter()
                .filter(|u| by_underlying.contains_key(u))
                .collect()
        } else {
            self.served_underlyings.clone()
        }
    }

    /// **A different datahub is a different world.** When the dial names another address or device-key
    /// epoch than the last pass saw (a backend switch, a new `config.datahub_addr`, a new device key, or
    /// no datahub at all), everything the old server answered is CLEARED at once — the chains, the selector,
    /// the index and the status — together with everything learned from it. Its data must never
    /// be shown as the new server's last good. Returns whether it cleared. The first dial a poller
    /// ever sees clears nothing: there is nothing yet to clear.
    pub(crate) fn observe_dial(
        &mut self,
        td: &mut DatahubPollSnapshot,
        dial: Option<&DatahubDial>,
    ) -> bool {
        let identity = dial.map(|d| (d.address.clone(), d.epoch));
        let changed = self.dial_seen.as_ref().is_some_and(|seen| *seen != identity);
        self.dial_seen = Some(identity);
        if !changed {
            return false;
        }
        td.options_by_underlying.clear();
        td.options_underlyings.clear();
        td.options_index = OptionIndex::default();
        td.options_status.clear();
        let dial_seen = self.dial_seen.take();
        *self = Self { dial_seen, ..Self::default() };
        true
    }

    /// Whether the index is due now: `vike_options::index_refresh_due` decides, over the time since
    /// the last attempt AT THIS ADDRESS — a new datahub is a first attempt, so connecting a backend
    /// does not wait out the empty index's retry period.
    fn index_due(&self, td: &DatahubPollSnapshot, addr: Option<&str>, now: Instant) -> bool {
        let since = match &self.index_attempt {
            Some((at, asked)) if asked.as_deref() == addr => {
                Some(now.saturating_duration_since(*at))
            }
            _ => None,
        };
        // An empty index retries at the refill rate itself, so a run of failures cannot drain the burst.
        let due = vike_options::index_refresh_due(
            &td.options_index,
            &td.options_by_underlying,
            since,
            crate::catalog::CATALOG_VENUE_REFILL,
        );
        // The decision is pure; the gap it acted on is logged HERE, once per refresh decision and
        // never per answer.
        if due
            && !td.options_index.is_empty()
            && let Some((missing, first)) = td.options_index.missing_from(&td.options_by_underlying)
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
    pub(crate) fn merge(&mut self, td: &mut DatahubPollSnapshot, pass: OptionsPass) {
        let (addr, replies) = match pass {
            OptionsPass::Asked { addr, replies } => (addr, replies),
            OptionsPass::ConnectFailed { addr, error } if error.transient => {
                td.options_status = self.link_failed(&addr, &error.cause);
                return;
            }
            not_asked => {
                // A refused dial and an old server both ANSWERED: the link itself works.
                if let OptionsPass::ConnectFailed { addr, .. }
                | OptionsPass::ServerUnsupported { addr, .. } = &not_asked
                {
                    self.streak.reached(POLL, addr);
                }
                if let Some(line) = not_asked.not_asked_line() {
                    td.options_status = line;
                }
                return;
            }
        };
        let mut problem: Option<String> = None;
        let mut link_lost: Option<String> = None;
        for (underlying, answer) in replies {
            match answer {
                Ok(reply) => {
                    self.learn(&reply);
                    match reply.outcome {
                        OptionsChainOutcome::Chains(bundle) => {
                            td.options_by_underlying.insert(underlying, bundle);
                        }
                        outcome => {
                            let line = OptionsChainReply { outcome, ..reply }.describe();
                            problem.get_or_insert(line);
                        }
                    }
                }
                Err(error) if error.transient => link_lost = Some(error.cause),
                Err(error) => {
                    problem.get_or_insert_with(|| failed_line(&addr, &underlying, &error.cause));
                }
            }
        }
        td.options_underlyings = self.selector(&td.options_by_underlying);
        if let Some(cause) = link_lost {
            td.options_status = self.link_failed(&addr, &cause);
            return;
        }
        self.streak.reached(POLL, &addr);
        td.options_status = match (problem, td.options_index.is_empty(), &self.index_note) {
            (Some(line), _, _) => line,
            (None, true, Some(note)) => {
                format!("{} — the Greeks tool cannot price yet: {note}", live_line(&addr))
            }
            (None, _, _) => live_line(&addr),
        };
    }

    /// The status of a pass whose LINK failed (`crate::link`): the quiet reconnecting line
    /// while the streak is short, the cause once it is not. The chains are kept either way.
    fn link_failed(&mut self, addr: &str, cause: &str) -> String {
        self.streak.failed(POLL, addr, cause).unwrap_or_else(|| RECONNECTING_LINE.to_string())
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
    bundle: Option<&OptionChainSet>,
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

/// The underlyings of the deribit OPTIONS in a listing (the payload's own field, never the name), in
/// the listing's order: the most listed options first (`vike_options::underlyings_by_listed_options`,
/// the rule the server's own listing follows).
fn option_underlyings(instruments: &[Instrument]) -> Vec<String> {
    vike_options::underlyings_by_listed_options(
        instruments.iter().filter(|i| i.venue == OPTIONS_VENUE).filter_map(|i| match &i.contract {
            Contract::Option(spec) => spec.underlying.as_deref().filter(|u| !u.is_empty()),
            _ => None,
        }),
    )
}

/// The Options half's poller: its replies bounded by [`OPTIONS_REPLY_TIMEOUT`].
impl Default for DatahubPoller<OptionsPollState> {
    fn default() -> Self {
        Self::with_reply_timeout(OPTIONS_REPLY_TIMEOUT)
    }
}

/// **The Options poll thread's whole state, one pass at a time** — public so the loopback suite
/// (`crates/vike-datahub-client/tests/options_poll.rs`) drives the exact pass the thread runs.
impl DatahubPoller<OptionsPollState> {
    /// A poller whose held connection bounds each options reply by `reply_timeout` instead of
    /// [`OPTIONS_REPLY_TIMEOUT`] — the seam the loopback suite drives a silent datahub through in
    /// milliseconds. The poll thread uses [`Default`].
    #[must_use]
    pub fn with_reply_timeout(reply_timeout: Duration) -> Self {
        Self {
            state: OptionsPollState::default(),
            connection: DatahubPollConnection::with_reply_timeout(reply_timeout),
        }
    }

    /// **One pass**: read the live picks off `shared`'s link; with none, ask nothing and return
    /// `false`. Otherwise refresh the index when it is due, plan, ask the datahub over the held
    /// connection and merge the answers, then return `true` (the caller wakes the UI, which is what
    /// renews an open window's picks). Every network call is made OUTSIDE the lock, so a slow
    /// server never stalls the frame.
    pub fn run_once(
        &mut self,
        shared: &Mutex<DatahubPollSnapshot>,
        dial: Option<&DatahubDial>,
        now: Instant,
    ) -> bool {
        // A switched datahub clears the old one's data before anything else, window open or not.
        let link = {
            let mut td = lock(shared);
            self.state.observe_dial(&mut td, dial);
            td.options_link.clone()
        };
        let picks = link.live_picks(now);
        if picks.is_empty() {
            return false;
        }
        let addr = dial.map(|d| d.address.as_str());
        let index_due = self.state.index_due(&lock(shared), addr, now);
        if index_due {
            self.state.index_attempt = Some((now, addr.map(str::to_string)));
            let report = fetch_venue_catalog(dial, OPTIONS_VENUE);
            if let Some(index) = self.state.adopt_index(report) {
                lock(shared).options_index = index;
            }
        }
        let plan = {
            let td = lock(shared);
            self.state.plan(&picks, &td.options_by_underlying, now)
        };
        if plan.is_empty() {
            lock(shared).options_status = waiting_line(self.state.index_note.as_deref());
            return true;
        }
        let pass = self.connection.options_pass(dial, &plan);
        self.state.merge(&mut lock(shared), pass);
        true
    }
}

/// Why the poll thread woke.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OptionsWake {
    /// A Refresh press or a new pick; any further queued wakes were drained with it.
    Refresh,
    /// The cadence elapsed.
    Cadence,
    /// Every sender is gone; the cadence was slept out rather than spun.
    Closed,
}

/// Wait for the next pass: the `cadence`, or a wake on `rx`, whichever comes first. A burst of
/// wakes coalesces to ONE pass, and a closed channel sleeps the cadence instead of busy-looping.
pub(crate) fn wait_for_next_poll(rx: &Receiver<()>, cadence: Duration) -> OptionsWake {
    match rx.recv_timeout(cadence) {
        Ok(()) => {
            while rx.try_recv().is_ok() {}
            OptionsWake::Refresh
        }
        Err(RecvTimeoutError::Timeout) => OptionsWake::Cadence,
        Err(RecvTimeoutError::Disconnected) => {
            std::thread::sleep(cadence);
            OptionsWake::Closed
        }
    }
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
