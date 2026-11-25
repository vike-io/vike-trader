//! **The options source over the gatewayhub** — a REST poll thread over the gatewayhub's options
//! provider ([`GatewayOptions`]), the two option feeds ([`FeedOptionsGateway`]), and the option index
//! built from the venue's own instrument rows. Everything they produce folds into ONE
//! [`OptionChainStore`], which the verb reads.
//!
//! # ⚠ It names NO venue: which venue, which underlyings and which indexes are the gatewayhub's answer
//!
//! The venue slug the lane serves, the underlyings it lists and the `markprice.options` index of each
//! are the gatewayhub's LISTING ([`GatewayOptions::listing`]), which the gatewayhub reads from the
//! venue's own instrument rows and holds for an hour. Every pass asks it again (a cache read at the
//! gatewayhub), so a new underlying, a delisted one and a new index reach the served set and the
//! markprice feed without a restart. An operator who will not wait asks for a refresh
//! ([`OptionsSource::refresh`], `Request::OptionsRefresh`). ⚠ **While the listing cannot be had, the source serves NO
//! underlying and says why in words** ([`OptionsSource::underlyings`]'s `Err`, which the verb answers
//! as `NotReady`): before the first pass asked it, and whenever the last ask failed (a gatewayhub
//! that is down). The venue slug, once learned, is kept: it never changes.
//!
//! The logic is the desktop's (`vike-app-core`'s former options poll thread, `fetch_underlying`,
//! `fetch_options` and `fetch_option_index`), moved rather than reinvented: the same nearest-8
//! expiries at ±30 strikes, the same per-underlying last-good merge, the same ticker feed on the
//! first non-empty focus set, the same index cadence. Two things follow EXPIRY that did not: the
//! ticker feed is replaced when a focus front expiry rolls (its instruments expired), and a bundle
//! kept through failed passes never serves an expiry whose instant has passed
//! (`OptionChainStore::snapshot`). What differs is WHO runs it: one
//! process for every client — and, since decision 0164, WHO SPEAKS TO THE VENUE: the gatewayhub owns
//! the connection (`crates/vike-gatewayhub/src/rest/options.rs`'s `OptionsHub` serves it over the venue's
//! spec, `crates/bridges/deribit/src/options.rs`'s `OptionsSpecDeribit`, which holds the one persistent
//! provider and starts the two feeds), this process opens none.
//!
//! # ⚠ What stayed HERE: the poll thread, the folds, the strike window, the recorder
//!
//! The gatewayhub serves FETCHES and the two feed streams; it windows nothing and records nothing.
//! So `fetch_underlying` asks it for the FULL chain (`strikes: None`), records that full chain
//! ([`record_chain`], the hop that used to live inside the venue crate's `fetch_chain`), and only
//! then applies [`STRIKE_WINDOW`] with `vike_options::limit_strikes`. The recorded rows and the
//! served rows are therefore exactly what they were when the provider did both in one call: the
//! window is a display concern, never a recording one.
//!
//! # ⚠ It polls NOTHING until somebody asks
//!
//! [`OptionsSourceGateway`] starts its thread on the FIRST [`OptionsSource::underlyings`] or
//! [`OptionsSource::snapshot`] call (a `Once`), so a datahub whose clients never open the Options tool
//! spends no venue budget and holds no feed; that first request answers `NotReady` and the next one, a
//! poll later, the snapshot. The one exception is the operator's chain RECORDER (`flags.record_chains`):
//! a recording that waited for a viewer would record nothing on a box nobody watches, so a lane with a
//! recorder is started at mount ([`OptionsSourceGateway::start`]).
//!
//! # The venue cost, every term a constant here or the venue's listing (spent by the gatewayhub)
//!
//! - REST: one `get_book_summary_by_currency` per BOOK per poll interval — [`POLL_INTERVAL`], or the
//!   box's `config.datahub_options_poll_secs` row ([`poll_interval`]) — (the gatewayhub's provider's
//!   5 s cache makes the per-expiry fetches of one book one call), over every LISTED underlying;
//! - `markprice.options.{index}` for every listed index: one socket per [`GATEWAY_OPTIONS_MAX_INDEXES`]
//!   indexes, chain-wide, from the first pass, opened again when the listed indexes change;
//! - `ticker.{inst}.100ms`: one socket over the FRONT expiry's ±strikes of the focus underlyings
//!   ([`OptionsFocus`]: the box's two `config.datahub_options_focus_*` rows; no socket at all while
//!   the underlyings row is unset, since it has no default),
//!   chosen by the server from its own grid
//!   (`vike_options::front_expiry_focus_instruments`) — no client input reaches it;
//! - `public/get_instruments`: two requests (options, futures) on `vike_options::index_refresh_due`'s
//!   cadence — every minute while the index is empty, at most every ten minutes after;
//! - the listing: one ask per pass, answered from the gatewayhub's hour-long hold.
//!
//! # ⚠ The gatewayhub is a dependency now: its absence is a failed PASS, never an empty chain
//!
//! A pass that cannot reach the gatewayhub fetches nothing, so it is recorded as a failure and keeps
//! every last-good bundle (the same path a dead venue took); the two feeds re-dial by themselves. The
//! poll thread therefore needs no start order with the daemon.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, Once, PoisonError};
use std::time::{Duration, Instant};

use vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS;
use vike_data::{ChainRecorder, ChainRow, HistoryStore};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::options::{FeedOptionsGateway, GatewayOptions};
use vike_gatewayhub_client::proto::{GATEWAY_OPTIONS_MAX_INDEXES, OptionsListing};
use vike_marketdata::{OptionChain, OptionChainSet, OptionKind};
use vike_options::{OptionIndex, limit_strikes};

use super::book::{OptionChainStore, RISK_FREE};
use super::{OptionsLane, OptionsSource, POLLED_EXPIRIES};

/// The REST re-poll cadence: the desktop's `OPT_POLL_SECS`. With the two feeds carrying mark/IV
/// chain-wide and bid/ask for the focus set, the poll backstops chain STRUCTURE, open interest,
/// volume and the bid/ask of strikes outside the focus set.
///
/// This is the DEFAULT: a box may write the `config.datahub_options_poll_secs` row to change it
/// ([`poll_interval`]), and `vike_config::config::DATAHUB_OPTIONS_POLL_SECS` is the range it is
/// refused outside of (`crates/vike-datahub/tests/chain_recording.rs`'s
/// `the_options_rows_ranges_hold_their_defaults_and_the_cadence_floor_is_the_poll` holds the
/// default inside it, and the chain cadence's floor equal to it).
pub const POLL_INTERVAL: Duration = Duration::from_secs(12);

/// The interval a mount hands [`real_options_lane`]: the `config.datahub_options_poll_secs` row, or
/// [`POLL_INTERVAL`] for a box that wrote none — the lane before the row existed, byte for byte.
pub fn poll_interval(row_secs: Option<u32>) -> Duration {
    row_secs.map_or(POLL_INTERVAL, |secs| Duration::from_secs(u64::from(secs)))
}

/// The chain recorder's idempotency-bucket cadence, in milliseconds: the
/// `config.datahub_chain_cadence_secs` row, or `vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS`
/// for a box that wrote none.
///
/// ⚠ The row is SECONDS and the recorder counts milliseconds, which is why the conversion is one
/// named function with a test rather than a `* 1000` at the call site.
pub fn chain_cadence_ms(row_secs: Option<u32>) -> i64 {
    row_secs.map_or(DEFAULT_CHAIN_CADENCE_MS, |secs| i64::from(secs) * 1_000)
}

/// Strikes held each side of spot per expiry: the widest display preset, so a client's own ±N
/// window never needs a different fetch.
pub const STRIKE_WINDOW: usize = 30;

/// **The 100 ms ticker feed's focus**: the underlyings whose front expiry it streams and the strikes
/// each side of spot, per underlying. A CHOICE, not a venue fact (an underlying the listing does not
/// name streams nothing): the box's `config.datahub_options_focus_underlyings` and
/// `config.datahub_options_focus_strikes` rows, which the mount reads and hands in as this value. The
/// strikes default is stated once, in `vike_config::config` (`DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES`);
/// the underlyings have NONE (owner, 2026-11-13: no hardcoded symbols), so an unset row streams
/// nothing. The loader refuses a pair the gatewayhub's per-feed cap could not take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsFocus {
    /// The focus underlyings, as the venue spells them, in the row's order.
    pub underlyings: Vec<String>,
    /// Strikes each side of spot, per underlying.
    pub strikes: usize,
}

impl OptionsFocus {
    /// The focus the two rows name, through the loader's own grammar
    /// (`vike_config::config::parse_options_focus_underlyings`): unset underlyings are NONE, unset
    /// strikes their default. The loader has already refused a row this cannot read, so an `Err` is
    /// a store that changed under the boot.
    pub fn from_rows(underlyings: Option<&str>, strikes: Option<u32>) -> Result<Self, String> {
        use vike_config::config::{
            DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES, parse_options_focus_underlyings,
        };
        let strikes = strikes.unwrap_or(DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES);
        Ok(Self {
            underlyings: underlyings
                .map(parse_options_focus_underlyings)
                .transpose()?
                .unwrap_or_default(),
            strikes: usize::try_from(strikes).map_err(|_| format!("{strikes} strikes"))?,
        })
    }
}

impl Default for OptionsFocus {
    /// The focus of a box that wrote neither row: no underlying, so the ticker feed streams nothing.
    fn default() -> Self {
        Self::from_rows(None, None).expect("vike-config's default focus parses by its own grammar")
    }
}

/// What a source serves before its first pass asked the gatewayhub for the listing.
const NOT_LISTED_YET: &str =
    "the options listing has not been asked of the gatewayhub yet; the first pass asks it";

/// **The operator's `kind=chain` recorder, as the mount builds it**: `Some` exactly when `armed`
/// (the mount's verdict on the `flags.record_chains` row, which also asks that compaction runs:
/// `crate::datahub_cli`'s `start_options_lane`), writing through `store` — the handle the daemon
/// SERVES, never a second store opened at a root this process already owns. `cadence_ms` is the
/// idempotency bucket ([`chain_cadence_ms`]: the `config.datahub_chain_cadence_secs` row, or
/// `vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS` for a box that wrote none): at most one
/// snapshot per (underlying, expiry) per bucket, however often the poll re-fetches. Each snapshot is
/// ONE append, so the series gains up to [`POLLED_EXPIRIES`] parts per underlying per bucket until
/// a maintenance pass compacts them; every row of the full chain is recorded (the
/// [`STRIKE_WINDOW`] trims only what is served).
///
/// ⚠ The bucket number is `timestamp_ms / cadence_ms` and the idempotency key does not name the cadence,
/// so changing it on a series that already holds snapshots re-numbers its buckets — see
/// `vike_data::rec::chain_rec`'s module doc, and `crates/vike-store/src/chain_rec_tests.rs`'s
/// `a_cadence_change_can_add_a_snapshot_and_never_swallows_one_at_ordinary_ratios` and
/// `a_near_equal_cadence_can_swallow_a_snapshot_written_days_earlier` for exactly what that does
/// and does not cost.
pub fn chain_recorder(
    armed: bool,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cadence_ms: i64,
) -> Option<Arc<ChainRecorder>> {
    armed.then(|| Arc::new(ChainRecorder::with_cadence(store, true, cadence_ms)))
}

/// **The lane this build mounts**: the one source over the gatewayhub, carrying the operator's chain
/// recorder when the root armed one (`None` records nothing), polling every `poll`
/// ([`poll_interval`]), streaming the ticker feed over `focus`, and fetching through the gatewayhub
/// at `link`. A recorder STARTS the source at once — see the module doc.
pub fn real_options_lane(
    chain_rec: Option<Arc<ChainRecorder>>,
    poll: Duration,
    focus: OptionsFocus,
    link: GatewayLink,
) -> OptionsLane {
    let source = OptionsSourceGateway::new(chain_rec, poll, link).with_focus(focus);
    if source.records() {
        source.start();
    }
    OptionsLane::new(vec![Box::new(source) as Box<dyn OptionsSource>])
}

/// What a source knows of the gatewayhub's listing: the venue (kept once learned) and the
/// underlyings it serves now, or why it serves none.
struct ListingState {
    venue: Option<String>,
    served: Result<OptionsListing, String>,
}

/// The listing state shared by the source, its poll thread and the refresh. Cloning it shares it.
#[derive(Clone)]
struct Listing(Arc<Mutex<ListingState>>);

impl Listing {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(ListingState { venue: None, served: Err(NOT_LISTED_YET.into()) })))
    }

    fn lock(&self) -> MutexGuard<'_, ListingState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask the gatewayhub for the listing (`refresh` asks it to read the venue now) and keep the
    /// answer: the listing, or why it could not be had — never the listing an earlier ask left.
    fn ask(&self, remote: &GatewayOptions, refresh: bool) -> Result<OptionsListing, String> {
        let answer = remote
            .listing(refresh)
            .map_err(|e| format!("the options listing could not be had from the gatewayhub: {e}"));
        let mut state = self.lock();
        if let Ok(listing) = &answer {
            state.venue = Some(listing.venue.clone());
        }
        state.served = answer.clone();
        answer
    }

    fn names(&self) -> Result<Vec<String>, String> {
        self.lock().served.as_ref().map(names_of).map_err(String::clone)
    }
}

/// A listing's underlying names, in its order.
fn names_of(listing: &OptionsListing) -> Vec<String> {
    listing.underlyings.iter().map(|u| u.name.clone()).collect()
}

/// The options source over the gatewayhub: a book, the gatewayhub's listing, and the thread that
/// fills both — started once.
pub struct OptionsSourceGateway {
    book: OptionChainStore,
    listing: Listing,
    chain_rec: Option<Arc<ChainRecorder>>,
    /// How long the poll thread sleeps between passes: [`POLL_INTERVAL`], or the box's row.
    poll: Duration,
    /// Where the gatewayhub is and how this process signs in to it: every fetch and both feeds dial
    /// from it.
    link: GatewayLink,
    /// What the ticker feed streams: nothing ([`OptionsFocus::default`]), or the box's rows
    /// ([`Self::with_focus`]).
    focus: OptionsFocus,
    started: Once,
}

impl OptionsSourceGateway {
    /// An unstarted source. `chain_rec` is what the poll pass records every fetched chain through
    /// ([`record_chain`]), so every polled chain lands in the `kind=chain` series when it is enabled.
    /// `poll` is the sleep between passes of the REST poll thread ([`poll_interval`]) — a parameter,
    /// because this module reads no settings. `link` is the gatewayhub dial, built by the root. The
    /// ticker focus is the empty one ([`OptionsFocus::default`]: no ticker feed) until
    /// [`Self::with_focus`].
    pub fn new(chain_rec: Option<Arc<ChainRecorder>>, poll: Duration, link: GatewayLink) -> Self {
        Self {
            book: OptionChainStore::new(),
            listing: Listing::new(),
            chain_rec,
            poll,
            link,
            focus: OptionsFocus::default(),
            started: Once::new(),
        }
    }

    /// The same source streaming the ticker feed over `focus` (the box's rows, read by the mount).
    #[must_use]
    pub fn with_focus(mut self, focus: OptionsFocus) -> Self {
        self.focus = focus;
        self
    }

    /// The ticker focus this source streams.
    pub fn focus(&self) -> &OptionsFocus {
        &self.focus
    }

    /// The sleep between REST passes this source was built with.
    pub fn poll(&self) -> Duration {
        self.poll
    }

    /// Whether this source records chains (an enabled recorder is threaded in).
    pub fn records(&self) -> bool {
        self.chain_rec.as_ref().is_some_and(|r| r.enabled())
    }

    /// The recorder the poll pass writes through, if any — lets a composition root confirm/report its
    /// own wiring (`source.chain_recorder().is_some_and(|r| r.enabled())` = "chains are being
    /// captured"), and lets the wiring be asserted in a test instead of only by reading code.
    pub fn chain_recorder(&self) -> Option<&ChainRecorder> {
        self.chain_rec.as_deref()
    }

    /// The REST client the poll thread runs its passes over: the gatewayhub's options provider, dialled
    /// from this source's link.
    pub fn remote(&self) -> GatewayOptions {
        GatewayOptions::new(self.link.clone())
    }

    /// Start the poll thread, once; later calls do nothing. The thread asks the listing first, and opens
    /// the markprice feed over its indexes.
    pub fn start(&self) {
        self.started.call_once(|| {
            if self.records() {
                tracing::info!(
                    "vike-datahub options: kind=chain snapshot recording is LIVE (the \
                     flags.record_chains row) — every polled chain lands in the store"
                );
            }
            spawn_poll_thread(
                self.book.clone(),
                self.listing.clone(),
                self.remote(),
                self.chain_rec.clone(),
                self.link.clone(),
                self.poll,
                self.focus.clone(),
            );
            tracing::info!(
                "vike-datahub options: the options lane STARTED — polling every listed underlying \
                 every {}s and streaming markprice.options; this box now spends the venue's public \
                 budget from its IP, through the gatewayhub",
                self.poll.as_secs()
            );
        });
    }
}

impl OptionsSource for OptionsSourceGateway {
    fn venue(&self) -> Option<String> {
        self.listing.lock().venue.clone()
    }

    fn underlyings(&self) -> Result<Vec<String>, String> {
        // Asking is wanting: the first ask starts the lane (module doc).
        self.start();
        self.listing.names()
    }

    fn snapshot(&self, underlying: &str, expiries: &[String]) -> Result<OptionChainSet, String> {
        // The first request is what starts the lane (module doc); it answers `NotReady`.
        self.start();
        self.book.snapshot(underlying, expiries, vike_model::now_ms())
    }

    /// The gatewayhub asked to read the venue's listing NOW (`OptionsCall::Listing { refresh: true }`;
    /// it answers the listing it holds when it read one within the last minute). The answer, or why it
    /// could not be had, is what this source serves from then on; the poll's next pass passes over it.
    fn refresh(&self) -> Result<Vec<String>, String> {
        self.listing.ask(&self.remote(), true).map(|listing| names_of(&listing))
    }
}

/// The markprice feeds over `indexes`: one per [`GATEWAY_OPTIONS_MAX_INDEXES`] of them, each folding into
/// `book`. A feed that cannot start is logged and left out; the next change of the indexes tries again.
fn markprice_feeds(
    book: &OptionChainStore,
    link: &GatewayLink,
    indexes: &[String],
) -> Vec<FeedOptionsGateway> {
    indexes
        .chunks(GATEWAY_OPTIONS_MAX_INDEXES)
        .filter_map(|chunk| {
            let book = book.clone();
            FeedOptionsGateway::markprice(link.clone(), chunk.to_vec(), move |rows| {
                book.fold_markprice(&rows, vike_model::now_ms());
            })
            .map_err(|e| {
                tracing::error!("vike-datahub options: a markprice feed could not start: {e}")
            })
            .ok()
        })
        .collect()
}

/// The REST poll: the listing (and the markprice feeds when its indexes changed), the index when due,
/// one pass over every listed underlying, the ticker feed on the first non-empty focus set (and again
/// when a focus front expiry rolls), then sleep
/// `poll`. A pass that fetched nothing is recorded (and logged once per pass) and keeps every last-good
/// bundle.
fn spawn_poll_thread(
    book: OptionChainStore,
    listing: Listing,
    remote: GatewayOptions,
    chain_rec: Option<Arc<ChainRecorder>>,
    link: GatewayLink,
    poll: Duration,
    focus: OptionsFocus,
) {
    std::thread::Builder::new()
        .name("dh-options".into())
        .spawn(move || {
            let focus_underlyings: Vec<&str> =
                focus.underlyings.iter().map(String::as_str).collect();
            let mut index_attempt: Option<Instant> = None;
            // The feeds' handles, held for the life of this thread (which is the life of the process):
            // a feed re-dials by itself and stops only when its handle drops. The markprice feeds are
            // replaced when the listed indexes change, the ticker feed when a focus front expiry rolls
            // (its instruments expired).
            let mut ticker_feed: Option<FeedOptionsGateway> = None;
            let mut marks_indexes: Vec<String> = Vec::new();
            let mut marks_feeds: Vec<FeedOptionsGateway> = Vec::new();
            loop {
                let listed = listing.ask(&remote, false);
                if let Ok(listed) = &listed {
                    let indexes: Vec<String> =
                        listed.underlyings.iter().map(|u| u.index.clone()).collect();
                    if indexes != marks_indexes {
                        // The old feeds stop before the new ones open: one socket set at a time.
                        marks_feeds.clear();
                        marks_feeds = markprice_feeds(&book, &link, &indexes);
                        marks_indexes = indexes;
                    }
                }
                if book.index_refresh_due(index_attempt.map(|at| at.elapsed())) {
                    index_attempt = Some(Instant::now());
                    match fetch_option_index(&remote) {
                        Ok(index) => book.set_index(index),
                        Err(e) => {
                            tracing::warn!("vike-datahub options: option index fetch failed: {e}")
                        }
                    }
                }
                let pass = listed.and_then(|listed| {
                    fetch_options(&remote, chain_rec.as_deref(), &names_of(&listed))
                });
                match pass {
                    Ok(fetched) => {
                        if let Some(focus) =
                            book.merge_pass(fetched, &focus_underlyings, focus.strikes)
                        {
                            // The old feed stops before the new one opens.
                            drop(ticker_feed.take());
                            let ticker = book.clone();
                            match FeedOptionsGateway::ticker(link.clone(), focus, move |row| {
                                ticker.fold_ticker(&row, vike_model::now_ms());
                            }) {
                                Ok(feed) => ticker_feed = Some(feed),
                                Err(e) => tracing::error!(
                                    "vike-datahub options: the ticker feed thread could not \
                                     start: {e}"
                                ),
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("vike-datahub options: chain pass fetched nothing: {e}");
                        book.record_failure(e);
                    }
                }
                std::thread::sleep(poll);
            }
        })
        .expect("spawn the options poll thread");
}

/// One underlying's bundle: the nearest [`POLLED_EXPIRIES`] expiries, each chain greeks-enriched and
/// windowed to ±[`STRIKE_WINDOW`] strikes, plus the front-expiry default. `None` (skip this
/// underlying) when the summary fetch fails or every chain came back empty, so one dead book never
/// fails the whole pass.
///
/// ⚠ Each chain is fetched UNWINDOWED, recorded when `chain_rec` is threaded in, and windowed only
/// afterwards — the order the venue crate's `fetch_chain` ran them in (the strike window is a display
/// concern, never a recording one).
fn fetch_underlying(
    remote: &GatewayOptions,
    chain_rec: Option<&ChainRecorder>,
    underlying: &str,
) -> Option<OptionChainSet> {
    let mut expiries = remote.list_expiries(underlying).ok()?;
    expiries.truncate(POLLED_EXPIRIES);
    let mut chains = BTreeMap::new();
    for e in &expiries {
        if let Ok(chain) = remote.fetch_chain(underlying, &e.date, None, RISK_FREE) {
            // Record the FULL chain (pre-`limit_strikes`). Opt-in + best-effort: absent or disabled
            // recorder = no-op.
            if let Some(rec) = chain_rec {
                record_chain(rec, &chain);
            }
            chains.insert(e.date.clone(), limit_strikes(chain, Some(STRIKE_WINDOW)));
        }
    }
    if chains.is_empty() {
        return None;
    }
    // The front expiry (lowest DTE) is the default selection.
    let default_expiry =
        expiries.iter().min_by_key(|e| e.dte).map(|e| e.date.clone()).unwrap_or_default();
    Some(OptionChainSet { default_expiry, expiries, chains })
}

/// One pass over every underlying in `underlyings` (the gatewayhub's listing) — what the poll thread
/// runs each poll interval ([`poll_interval`]), and so the hop where a threaded-in recorder writes every
/// fetched chain ([`record_chain`], inside `fetch_underlying`). Errors only when NOTHING fetched, so
/// the caller keeps every last-good bundle. Public so
/// `crates/vike-datahub/tests/chain_recording.rs` drives the real pass against a loopback gatewayhub.
pub fn fetch_options(
    remote: &GatewayOptions,
    chain_rec: Option<&ChainRecorder>,
    underlyings: &[String],
) -> Result<BTreeMap<String, OptionChainSet>, String> {
    let mut by_underlying = BTreeMap::new();
    for u in underlyings {
        if let Some(bundle) = fetch_underlying(remote, chain_rec, u) {
            by_underlying.insert(u.clone(), bundle);
        }
    }
    if by_underlying.is_empty() {
        return Err("no option chain fetched for any listed underlying".into());
    }
    Ok(by_underlying)
}

/// The option index: every option and future the venue lists, over the catalog's own parse — two
/// keyless `public/get_instruments` requests, `currency=any`, made by the gatewayhub. Fails when either
/// fails, so the caller keeps its last-good index rather than half of one.
fn fetch_option_index(remote: &GatewayOptions) -> Result<OptionIndex, String> {
    let instruments = remote.option_index_instruments()?;
    Ok(OptionIndex::from_instruments(&instruments))
}

/// The 08:00-UTC epoch-ms of an ISO `Y-M-D` date, or `None` when it is not a calendar date: the
/// grammar of `vike_options::expiry_ms` (which panics where this answers `None`) - each of the first
/// three `-`-separated parts an `i32`, the three a real day - so a date this accepts is exactly a date
/// that cannot panic it, and the value is the one it computes (both held by
/// `the_guard_is_the_grammar_of_the_panicking_parser`).
pub(super) fn checked_expiry_ms(date_iso: &str) -> Option<i64> {
    let mut parts = date_iso.split('-').map(|p| p.parse::<i32>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    let date = chrono::NaiveDate::from_ymd_opt(year, month as u32, day as u32)?;
    Some(date.and_hms_opt(8, 0, 0)?.and_utc().timestamp_millis())
}

/// Flatten one enriched [`OptionChain`] (ONE expiry — the natural `fetch_chain` unit) into
/// `kind=chain` snapshot rows — the pure half of the opt-in chain-recording hook. Every row
/// carries the chain's `asof_ms` as `ts` (all rows of one snapshot share it), the 08:00-UTC
/// `vike_options::expiry_ms` of the chain's expiry date, and the VERBATIM venue `instrument_name` (the
/// `_USDC` suffix survives — same contract as the arm/trade path). A quote without an
/// `instrument_name` (never produced by the venue crate's `build_chain_from_summary`, but the model
/// allows it) gets a synthesized `"{underlying}:{expiry}:{strike}:{C|P}"` id so the row still has a
/// usable per-instrument grouping identity for `chain_as_of`. Quote/greek fields copy through as-is:
/// absent stays absent, never 0.0.
///
/// TIMESTAMP SEMANTICS: `ts` is the chain's `asof_ms` — the FETCH-observation instant, not the
/// venue's own publish time. `crates/bridges/deribit/src/feed/options/chain.rs`'s `DeribitOptionsProvider` serves
/// rows from a `CACHE_TTL_MS` (5s) cache, so a recorded row's `ts` can post-date the actual venue
/// observation by up to that TTL, and two expiries built from one cached payload in the same refresh
/// pass carry slightly different `asof` stamps (each `fetch_chain` re-reads the clock). Immaterial at
/// the recorder's 1-minute default cadence, but it IS a bounded skew in a point-in-time series — a
/// consumer needing sub-5s fidelity must not treat `ts` as the venue's timestamp. The stamp is now
/// made by the gatewayhub, so it precedes this process by the hop (the same box, loopback).
///
/// **TOTAL over wire-fed data.** `chain.expiry.date` was built in this process (`make_expiry`, from a
/// date the venue crate had parse-validated) when this function lived in the venue crate; it now
/// arrives in an `OptionsReplied(Chain)` from the gatewayhub, and `vike_options::expiry_ms` PANICS on a
/// malformed or impossible date (a programmer error there). A panic here would end the detached
/// `dh-options` poll thread for good, and with it the ticker feed handle that thread holds. So the
/// date is checked first (`checked_expiry_ms`, the same grammar) and a chain whose date is not an
/// ISO calendar date is DROPPED, with one `warn`, recording nothing - a daemon bug or a version
/// skew is a missing snapshot the operator is told about, never a dead options lane.
pub fn chain_snapshot_rows(chain: &OptionChain) -> Vec<ChainRow> {
    let Some(exp_ms) = checked_expiry_ms(&chain.expiry.date) else {
        tracing::warn!(
            underlying = %chain.underlying,
            date = ?chain.expiry.date.chars().take(32).collect::<String>(),
            "vike-datahub options: a chain's expiry date is not an ISO calendar date, so the chain \
             was NOT recorded (a gatewayhub bug or a version skew)"
        );
        return Vec::new();
    };
    let mut out = Vec::new();
    for sharpe in &chain.rows {
        for (q, kind) in [(&sharpe.call, OptionKind::Call), (&sharpe.put, OptionKind::Put)] {
            let Some(q) = q else { continue };
            out.push(ChainRow {
                ts: chain.asof_ms,
                underlying: chain.underlying.clone(),
                instrument: q.instrument_name.clone().unwrap_or_else(|| {
                    let cp = kind.as_cp();
                    format!("{}:{}:{}:{}", chain.underlying, chain.expiry.date, sharpe.strike, cp)
                }),
                expiry_ms: exp_ms,
                strike: sharpe.strike,
                kind,
                bid: q.bid,
                ask: q.ask,
                mark: q.mark,
                iv: q.iv,
                open_interest: q.open_interest,
                volume: q.volume,
                delta: q.delta,
                gamma: q.gamma,
                theta: q.theta,
                vega: q.vega,
            });
        }
    }
    out
}

/// Record one enriched chain into the `kind=chain` PIT store (opt-in) — the mirror of `exec.rs`'s
/// `record_properties` for the options surface. Best-effort: the recorder no-ops when disabled
/// and swallows store errors, so the fetch path never fails on recording. The observation instant
/// for the recorder's idempotency bucket is the chain's own `asof_ms` (in ns), keeping bucket and
/// row `ts` in agreement (see [`chain_snapshot_rows`] on what that instant actually is).
///
/// The disabled check comes FIRST so a threaded-but-off recorder is a true no-op: a poll loop
/// re-fetching every expiry of every underlying each pass would otherwise flatten and immediately
/// drop the whole row set (a `String` clone per row for underlying + instrument) every pass. Not
/// the hot fold, so it is cheap either way — but "disabled costs nothing" is the contract
/// `PropertiesRecorder` sets, and this mirrors it.
pub fn record_chain(rec: &ChainRecorder, chain: &OptionChain) {
    if !rec.enabled() {
        return;
    }
    let rows = chain_snapshot_rows(chain);
    rec.record(&chain.source, &chain.underlying, &rows, chain.asof_ms.saturating_mul(1_000_000));
}

#[cfg(test)]
mod tests {
    //! The recording hop, over the DataFusion-free `HistoryStoreMemory` double. These three tests are the
    //! venue crate's former `chain_tests.rs` recorder tests, MOVED with the hop they pin
    //! (`chain_snapshot_rows` and `record_chain` lived in `crates/bridges/deribit/src/feed/options/chain.rs`).
    //! The fixture is an already-built chain (the venue crate's `build_chain_from_summary` output
    //! for BTC 27 Jun 2026) because this crate parses no book summary: the gatewayhub does.

    use vike_data::{HistoryStoreMemory, TimestampRange};
    use vike_marketdata::{
        OptionChainExpiry, OptionChainStrike, OptionQuote, OptionUnderlyingKind,
    };

    use super::*;

    /// 2026-06-02 08:00 UTC — the venue crate's fixture clock.
    const NOW: i64 = 1_780_387_200_000;

    fn quote(name: &str, strike: f64, kind: OptionKind, bid: f64, iv: f64) -> OptionQuote {
        OptionQuote {
            bid: Some(bid),
            ask: Some(bid + 1.0),
            mark: Some(bid + 0.5),
            iv: Some(iv),
            open_interest: Some(120.0),
            volume: Some(8.0),
            delta: Some(0.4),
            gamma: Some(0.0001),
            theta: Some(-30.0),
            vega: Some(55.0),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(strike, kind)
        }
    }

    /// BTC, 27 Jun 2026: a call and a put at 100000, a lone call at 110000.
    fn fixture_chain() -> OptionChain {
        OptionChain {
            underlying: "BTC".to_string(),
            underlying_kind: OptionUnderlyingKind::Crypto,
            underlying_price: Some(104_000.0),
            expiry: OptionChainExpiry {
                date: "2026-06-27".into(),
                dte: 25,
                label: "27 Jun".into(),
            },
            asof_ms: NOW,
            source: "deribit".to_string(),
            rows: vec![
                OptionChainStrike {
                    strike: 100_000.0,
                    call: Some(quote(
                        "BTC-27JUN26-100000-C",
                        100_000.0,
                        OptionKind::Call,
                        5_200.0,
                        0.625,
                    )),
                    put: Some(quote(
                        "BTC-27JUN26-100000-P",
                        100_000.0,
                        OptionKind::Put,
                        4_160.0,
                        0.61,
                    )),
                },
                OptionChainStrike {
                    strike: 110_000.0,
                    call: Some(quote(
                        "BTC-27JUN26-110000-C",
                        110_000.0,
                        OptionKind::Call,
                        2_080.0,
                        0.64,
                    )),
                    put: None,
                },
            ],
        }
    }

    #[test]
    fn chain_snapshot_rows_flatten_fixture_chain() {
        let chain = fixture_chain();
        let rows = chain_snapshot_rows(&chain);
        // 100000 call+put + 110000 call = 3 rows
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.ts == NOW && r.underlying == "BTC"));
        assert!(rows.iter().all(|r| r.expiry_ms == vike_options::expiry_ms("2026-06-27")));
        let call = &rows[0];
        assert_eq!(call.instrument, "BTC-27JUN26-100000-C", "verbatim venue id");
        assert_eq!(call.kind, OptionKind::Call);
        assert_eq!(call.strike, 100_000.0);
        assert_eq!(call.bid, Some(5_200.0), "USD-scaled premium copied through");
        assert_eq!(call.iv, Some(0.625), "decimal IV copied through");
        assert_eq!(call.delta, Some(0.4), "enriched greeks recorded");
        assert_eq!((call.open_interest, call.volume), (Some(120.0), Some(8.0)));
        let put = &rows[1];
        assert_eq!(put.instrument, "BTC-27JUN26-100000-P");
        assert_eq!(put.kind, OptionKind::Put);
        // the 110000 strike has no put — only 3 rows total, and the lone call carries its strike
        assert_eq!(rows[2].strike, 110_000.0);
    }

    /// A quote with no `instrument_name` still gets a usable grouping identity, and an absent field
    /// stays absent (never 0.0).
    #[test]
    fn a_nameless_quote_gets_a_synthesized_id_and_absent_stays_absent() {
        let mut chain = fixture_chain();
        chain.rows = vec![OptionChainStrike {
            strike: 100_000.0,
            call: Some(OptionQuote::new(100_000.0, OptionKind::Call)),
            put: None,
        }];
        let rows = chain_snapshot_rows(&chain);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].instrument, "BTC:2026-06-27:100000:C");
        assert!(rows[0].bid.is_none() && rows[0].iv.is_none() && rows[0].delta.is_none());
    }

    #[test]
    fn record_chain_persists_fixture_and_same_minute_is_one_snapshot() {
        let store = Arc::new(HistoryStoreMemory::new());
        let rec = ChainRecorder::new(store.clone(), true);
        let chain = fixture_chain();
        record_chain(&rec, &chain);
        record_chain(&rec, &chain); // same asof → same minute bucket → store no-op
        let got = store.scan_chain("deribit", "BTC", TimestampRange::all()).unwrap();
        assert_eq!(got.len(), 3, "recorded once, keyed venue=deribit/symbol=BTC");
        assert_eq!(got, chain_snapshot_rows(&chain));
    }

    #[test]
    fn record_chain_disabled_recorder_writes_nothing() {
        let store = Arc::new(HistoryStoreMemory::new());
        let rec = ChainRecorder::new(store.clone(), false);
        record_chain(&rec, &fixture_chain());
        assert!(store.scan_chain("deribit", "BTC", TimestampRange::all()).unwrap().is_empty());
    }

    // ---- Totality over wire-fed data (the bridge's `decoder_never_panics` property, moved) ----------
    //
    // The venue crate's `tests/decoder_never_panics.rs` fed `chain::chain_snapshot_rows` the chains its
    // own builder produced from hostile book summaries, with ISO dates the caller took from
    // `list_expiries` (so always valid). The chain now arrives over the wire, so the date is hostile
    // too: the properties below feed this crate's `chain_snapshot_rows` ARBITRARY strings and hostile
    // floats. No `proptest` here (this crate's manifest names none): a fixed-seed stream makes the same
    // claim reproducibly - a failure prints the seed's iteration and its input.

    /// A xorshift64* stream: arbitrary-looking and reproducible.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
            let i = self.below(xs.len() as u64) as usize;
            &xs[i]
        }
    }

    /// An expiry date that is arbitrary text, or three hostile numbers joined by `-`, or a near-valid
    /// `YYYY-MM-DD` with a month or day the calendar does not have.
    fn hostile_date(rng: &mut Rng) -> String {
        const CHARS: &[char] = &[
            '0',
            '1',
            '2',
            '3',
            '4',
            '5',
            '6',
            '7',
            '8',
            '9',
            '-',
            '-',
            '-',
            '+',
            ' ',
            '.',
            'e',
            'T',
            ':',
            '/',
            '\u{0}',
            '\u{7f}',
            'é',
            '日',
            '\u{1F600}',
        ];
        const NUMBERS: &[&str] = &[
            "0",
            "1",
            "2",
            "6",
            "12",
            "13",
            "27",
            "28",
            "29",
            "30",
            "31",
            "32",
            "99",
            "100",
            "2026",
            "1970",
            "-1",
            "-2026",
            "+7",
            "007",
            "99999999999",
            "2147483648",
            "-2147483649",
            "",
            "x",
        ];
        match rng.below(3) {
            0 => {
                let n = rng.below(24);
                (0..n).map(|_| *rng.pick(CHARS)).collect()
            }
            1 => format!("{}-{}-{}", rng.pick(NUMBERS), rng.pick(NUMBERS), rng.pick(NUMBERS)),
            _ => {
                let (y, m, d) = (1900 + rng.below(300), 1 + rng.below(14), rng.below(40));
                format!("{y:04}-{m:02}-{d:02}")
            }
        }
    }

    fn hostile_float(rng: &mut Rng) -> Option<f64> {
        match rng.below(8) {
            0 => None,
            1 => Some(f64::NAN),
            2 => Some(f64::INFINITY),
            3 => Some(f64::NEG_INFINITY),
            4 => Some(f64::MIN),
            5 => Some(-0.0),
            _ => Some(rng.next() as f64 / 7.0),
        }
    }

    fn hostile_quote(rng: &mut Rng, strike: f64, kind: OptionKind) -> Option<OptionQuote> {
        if rng.below(4) == 0 {
            return None;
        }
        Some(OptionQuote {
            bid: hostile_float(rng),
            ask: hostile_float(rng),
            mark: hostile_float(rng),
            iv: hostile_float(rng),
            open_interest: hostile_float(rng),
            volume: hostile_float(rng),
            delta: hostile_float(rng),
            gamma: hostile_float(rng),
            theta: hostile_float(rng),
            vega: hostile_float(rng),
            instrument_name: (rng.below(2) == 0).then(|| hostile_date(rng)),
            ..OptionQuote::new(strike, kind)
        })
    }

    fn hostile_chain(rng: &mut Rng, date: String) -> OptionChain {
        let mut chain = fixture_chain();
        chain.expiry.date = date;
        chain.underlying = hostile_date(rng);
        chain.asof_ms = rng.next() as i64;
        chain.rows = (0..rng.below(8))
            .map(|_| {
                let strike = hostile_float(rng).unwrap_or(0.0);
                OptionChainStrike {
                    strike,
                    call: hostile_quote(rng, strike, OptionKind::Call),
                    put: hostile_quote(rng, strike, OptionKind::Put),
                }
            })
            .collect();
        chain
    }

    /// **`chain_snapshot_rows` is total over arbitrary dates and hostile numbers.** A parseable date
    /// flattens to at most a call and a put per strike, each stamped with the chain's time and the
    /// date's 08:00-UTC; anything else is dropped whole (and says so), and none of it panics.
    #[test]
    fn chain_snapshot_rows_never_panics_and_drops_an_unparsable_chain_whole() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (mut kept, mut dropped) = (0usize, 0usize);
        for i in 0..20_000 {
            let date = hostile_date(&mut rng);
            let chain = hostile_chain(&mut rng, date.clone());
            let rows = chain_snapshot_rows(&chain);
            let quotes: usize = chain
                .rows
                .iter()
                .map(|r| usize::from(r.call.is_some()) + usize::from(r.put.is_some()))
                .sum();
            assert!(
                rows.len() <= 2 * chain.rows.len(),
                "#{i} {date:?}: more than a call and a put"
            );
            match checked_expiry_ms(&date) {
                Some(ms) => {
                    kept += 1;
                    assert_eq!(rows.len(), quotes, "#{i} {date:?}: every quote is a row");
                    assert!(rows.iter().all(|r| r.expiry_ms == ms && r.ts == chain.asof_ms));
                }
                None => {
                    dropped += 1;
                    assert!(
                        rows.is_empty(),
                        "#{i} {date:?}: a chain without a date records nothing"
                    );
                }
            }
        }
        // The generator reaches both branches, or the property above proves nothing.
        assert!(kept > 500 && dropped > 500, "kept {kept}, dropped {dropped}");
    }

    /// An unparsable date records NOTHING through the whole hook, and the next good chain records.
    #[test]
    fn a_chain_with_an_unparsable_date_is_not_recorded_and_the_next_one_is() {
        let store = Arc::new(HistoryStoreMemory::new());
        let rec = ChainRecorder::new(store.clone(), true);
        for bad in ["2026-02-30", "", "tomorrow", "2026-13-01", "-2026-06-27"] {
            let mut chain = fixture_chain();
            chain.expiry.date = bad.to_string();
            record_chain(&rec, &chain);
        }
        assert!(store.scan_chain("deribit", "BTC", TimestampRange::all()).unwrap().is_empty());
        record_chain(&rec, &fixture_chain());
        assert_eq!(store.scan_chain("deribit", "BTC", TimestampRange::all()).unwrap().len(), 3);
    }

    /// The guard is the grammar of the parser that panics: for every date here it answers what
    /// `vike_options::expiry_ms` computes, and `None` exactly where that panics. (The panics print
    /// their messages into this test's captured output.)
    #[test]
    fn the_guard_is_the_grammar_of_the_panicking_parser() {
        for date in [
            "2026-06-27",
            "2026-6-7",
            "+2026-+06-+27",
            "2026-06-27-and-more",
            "1970-01-01",
            "2024-02-29",
            "2026-02-29",
            "2026-02-30",
            "2026-13-01",
            "2026-00-10",
            "2026-06",
            "2026-06-27 ",
            "2026--06-27",
            "-2026-06-27",
            "99999999999-01-01",
            "",
        ] {
            let parser = std::panic::catch_unwind(|| vike_options::expiry_ms(date)).ok();
            assert_eq!(checked_expiry_ms(date), parser, "{date:?}");
        }
    }
}
