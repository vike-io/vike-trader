//! **The Deribit options source** — the one constructor that NAMES the venue: a REST poll thread
//! over `vike_deribit::chain::DeribitOptionsProvider`, the two keyless public option feeds, and the
//! option index built from Deribit's own `public/get_instruments`. Everything they produce folds
//! into ONE [`OptionsBook`], which the verb reads.
//!
//! The logic is the desktop's (`vike-app-core`'s former options poll thread, `fetch_underlying`,
//! `fetch_options` and `fetch_option_index`), moved rather than reinvented: the same nearest-8
//! expiries at ±30 strikes, the same per-underlying last-good merge, the same once-only ticker feed
//! on the first non-empty focus set, the same index cadence. What differs is WHO runs it: one
//! process for every client.
//!
//! # ⚠ It polls NOTHING until somebody asks
//!
//! [`DeribitOptions`] starts its threads on the FIRST [`OptionsSource::snapshot`] call (a `Once`),
//! so a datahub whose clients never open the Options tool spends no Deribit budget and holds no
//! socket; that first request answers `NotReady` and the next one, a poll later, the snapshot. The
//! one exception is the operator's chain RECORDER (`flags.record_chains`): a recording that waited
//! for a viewer would record nothing on a box nobody watches, so a lane with a recorder is started
//! at mount ([`DeribitOptions::start`]).
//!
//! # The venue cost, every term a constant here
//!
//! - REST: one `get_book_summary_by_currency` per BOOK per [`POLL_INTERVAL`] (BTC, ETH and the
//!   shared USDC book SOL lives in; the provider's 5 s cache makes the per-expiry fetches one call);
//! - `markprice.options.{btc_usd,eth_usd,sol_usdc}`: one socket, chain-wide, from the first pass;
//! - `ticker.{inst}.100ms`: one socket over the FRONT expiry's ±[`FOCUS_STRIKES`] strikes of
//!   [`FOCUS_UNDERLYINGS`], chosen by the server from its own grid
//!   (`vike_options::front_expiry_focus_instruments`) — no client input reaches it;
//! - `public/get_instruments`: two requests (options, futures) on `vike_options::index_refresh_due`'s
//!   cadence — every minute while the index is empty, at most every ten minutes after.

use std::collections::BTreeMap;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use vike_data::{ChainRecorder, HistStore};
use vike_deribit::chain::DeribitOptionsProvider;
use vike_deribit::options_feed::{
    MAINNET_WS, spawn_deribit_markprice_options_feed, spawn_deribit_ticker_feed,
};
use vike_marketdata::UnderlyingChains;
use vike_options::OptionIndex;

use super::book::{OptionsBook, RISK_FREE};
use super::{OptionsLane, OptionsSource, POLLED_EXPIRIES};

/// The venue slug this source is mounted under.
pub const VENUE: &str = "deribit";

/// The REST re-poll cadence: the desktop's `OPT_POLL_SECS`. With the two feeds carrying mark/IV
/// chain-wide and bid/ask for the focus set, the poll backstops chain STRUCTURE, open interest,
/// volume and the bid/ask of strikes outside the focus set.
pub const POLL_INTERVAL: Duration = Duration::from_secs(12);

/// Strikes held each side of spot per expiry: the widest display preset, so a client's own ±N
/// window never needs a different fetch.
pub const STRIKE_WINDOW: usize = 30;

/// The underlyings whose front expiry the 100 ms ticker feed streams.
pub const FOCUS_UNDERLYINGS: [&str; 2] = ["BTC", "ETH"];

/// Strikes each side of spot the ticker feed streams, per focus underlying (call and put each).
pub const FOCUS_STRIKES: usize = 10;

/// The `markprice.options` indexes: the two coin books and the USDC book SOL's options live in.
pub const MARKPRICE_INDEXES: [&str; 3] = ["btc_usd", "eth_usd", "sol_usdc"];

/// **The operator's `kind=chain` recorder, as the mount builds it**: `Some` exactly when `armed`
/// (the mount's verdict on the `flags.record_chains` row, which also asks that compaction runs:
/// `crate::datahub_cli`'s `mount_options_lane`), writing through `store` — the handle the daemon
/// SERVES, never a second store opened at a root this process already owns. The cadence is the recorder's default,
/// `vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS`: at most one snapshot per
/// (underlying, expiry) per minute, however often [`POLL_INTERVAL`] re-fetches. Each snapshot is
/// ONE append, so the series gains up to [`POLLED_EXPIRIES`] parts per underlying per minute until
/// a maintenance pass compacts them; every row of the full chain is recorded (the
/// [`STRIKE_WINDOW`] trims only what is served).
pub fn chain_recorder(
    armed: bool,
    store: Arc<dyn HistStore + Send + Sync>,
) -> Option<Arc<ChainRecorder>> {
    armed.then(|| Arc::new(ChainRecorder::new(store, true)))
}

/// **The lane this build mounts**: the one Deribit source, carrying the operator's chain recorder
/// when the root armed one (`None` records nothing). A recorder STARTS the source at once — see the
/// module doc.
pub fn real_options_lane(chain_rec: Option<Arc<ChainRecorder>>) -> OptionsLane {
    let source = DeribitOptions::new(chain_rec);
    if source.records() {
        source.start();
    }
    OptionsLane::new(vec![(VENUE.to_string(), Box::new(source) as Box<dyn OptionsSource>)])
}

/// The Deribit source: a book, the provider's underlyings, and the threads that fill the book —
/// started once.
pub struct DeribitOptions {
    book: OptionsBook,
    underlyings: Vec<String>,
    chain_rec: Option<Arc<ChainRecorder>>,
    started: Once,
}

impl DeribitOptions {
    /// An unstarted source. `chain_rec` is threaded into the poll's provider
    /// (`DeribitOptionsProvider::with_chain_recorder`), so every polled chain lands in the
    /// `kind=chain` series when it is enabled.
    pub fn new(chain_rec: Option<Arc<ChainRecorder>>) -> Self {
        // Canonical order (BTC, ETH, SOL, then the rest): the set every reply carries as
        // `served_underlyings`, which is what a client's selector shows.
        let mut underlyings: Vec<String> = DeribitOptionsProvider::new()
            .list_underlyings()
            .iter()
            .map(|u| (*u).to_string())
            .collect();
        underlyings.sort_by_key(|u| vike_options::underlying_rank(u));
        Self { book: OptionsBook::new(), underlyings, chain_rec, started: Once::new() }
    }

    /// Whether this source records chains (an enabled recorder is threaded in).
    pub fn records(&self) -> bool {
        self.chain_rec.as_ref().is_some_and(|r| r.enabled())
    }

    /// The provider the poll thread runs: a fresh mainnet one, with this source's recorder threaded
    /// in when it has one (`DeribitOptionsProvider::with_chain_recorder`).
    pub fn provider(&self) -> DeribitOptionsProvider {
        let provider = DeribitOptionsProvider::new();
        match self.chain_rec.clone() {
            Some(rec) => provider.with_chain_recorder(rec),
            None => provider,
        }
    }

    /// Start the poll thread and the markprice feed, once; later calls do nothing.
    pub fn start(&self) {
        self.started.call_once(|| {
            if self.records() {
                tracing::info!(
                    "vike-datahub options: kind=chain snapshot recording is LIVE (the \
                     flags.record_chains row) — every polled Deribit chain lands in the store"
                );
            }
            spawn_poll_thread(self.book.clone(), self.provider());
            // Detached for the process lifetime, like the desktop's: the feed owns its own stop
            // and reconnects by itself; process exit reaps it.
            let markprice = self.book.clone();
            let _feed = spawn_deribit_markprice_options_feed(
                MAINNET_WS.to_string(),
                MARKPRICE_INDEXES.iter().map(|i| (*i).to_string()).collect(),
                move |rows| {
                    markprice.fold_markprice(&rows, vike_model::now_ms());
                },
            );
            tracing::info!(
                "vike-datahub options: the Deribit options lane STARTED — polling {} every {}s and \
                 streaming markprice.options; this process now spends Deribit's public budget \
                 from this box's IP",
                self.underlyings.join("/"),
                POLL_INTERVAL.as_secs()
            );
        });
    }
}

impl OptionsSource for DeribitOptions {
    fn underlyings(&self) -> Vec<String> {
        self.underlyings.clone()
    }

    fn snapshot(&self, underlying: &str, expiries: &[String]) -> Result<UnderlyingChains, String> {
        // The first request is what starts the lane (module doc); it answers `NotReady`.
        self.start();
        self.book.snapshot(underlying, expiries)
    }
}

/// The REST poll: index when due, one pass over every underlying, the ticker feed on the first
/// non-empty focus set, then sleep. A pass that fetched nothing is recorded (and logged once per
/// pass) and keeps every last-good bundle.
fn spawn_poll_thread(book: OptionsBook, mut provider: DeribitOptionsProvider) {
    std::thread::Builder::new()
        .name("dh-options".into())
        .spawn(move || {
            let mut index_attempt: Option<Instant> = None;
            loop {
                if book.index_refresh_due(index_attempt.map(|at| at.elapsed())) {
                    index_attempt = Some(Instant::now());
                    match fetch_option_index() {
                        Ok(index) => book.set_index(index),
                        Err(e) => {
                            tracing::warn!("vike-datahub options: option index fetch failed: {e}")
                        }
                    }
                }
                match fetch_options(&mut provider) {
                    Ok(fetched) => {
                        if let Some(focus) =
                            book.merge_pass(fetched, &FOCUS_UNDERLYINGS, FOCUS_STRIKES)
                        {
                            let ticker = book.clone();
                            let _feed = spawn_deribit_ticker_feed(
                                MAINNET_WS.to_string(),
                                focus,
                                move |row| {
                                    ticker.fold_ticker(&row, vike_model::now_ms());
                                },
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("vike-datahub options: chain pass fetched nothing: {e}");
                        book.record_failure(e);
                    }
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("spawn the options poll thread");
}

/// One underlying's bundle: the nearest [`POLLED_EXPIRIES`] expiries, each chain greeks-enriched and
/// windowed to ±[`STRIKE_WINDOW`] strikes, plus the front-expiry default. `None` (skip this
/// underlying) when the summary fetch fails or every chain came back empty, so one dead book never
/// fails the whole pass.
fn fetch_underlying(
    provider: &mut DeribitOptionsProvider,
    underlying: &str,
) -> Option<UnderlyingChains> {
    let mut expiries = provider.list_expiries(underlying).ok()?;
    expiries.truncate(POLLED_EXPIRIES);
    let mut chains = BTreeMap::new();
    for e in &expiries {
        if let Ok(chain) = provider.fetch_chain(underlying, &e.date, Some(STRIKE_WINDOW), RISK_FREE)
        {
            chains.insert(e.date.clone(), chain);
        }
    }
    if chains.is_empty() {
        return None;
    }
    // The front expiry (lowest DTE) is the default selection.
    let default_expiry =
        expiries.iter().min_by_key(|e| e.dte).map(|e| e.date.clone()).unwrap_or_default();
    Some(UnderlyingChains { default_expiry, expiries, chains })
}

/// One pass over every underlying the provider offers — what the poll thread runs each
/// [`POLL_INTERVAL`], and so the hop where a threaded-in recorder writes every fetched chain
/// (`DeribitOptionsProvider::fetch_chain`). Errors only when NOTHING fetched, so the caller keeps
/// every last-good bundle. Public so `crates/vike-datahub/tests/chain_recording.rs` drives the
/// real pass against a loopback book summary.
pub fn fetch_options(
    provider: &mut DeribitOptionsProvider,
) -> Result<BTreeMap<String, UnderlyingChains>, String> {
    let mut by_underlying = BTreeMap::new();
    for u in provider.list_underlyings() {
        if let Some(bundle) = fetch_underlying(provider, u) {
            by_underlying.insert(u.to_string(), bundle);
        }
    }
    if by_underlying.is_empty() {
        return Err("no Deribit option chain fetched for any underlying".into());
    }
    Ok(by_underlying)
}

/// The option index: every Deribit option and future, over the catalog's own parse — two keyless
/// `public/get_instruments` requests, `currency=any`. Fails when either fails, so the caller keeps
/// its last-good index rather than half of one.
fn fetch_option_index() -> Result<OptionIndex, String> {
    let mut instruments = Vec::new();
    for kind in ["option", "future"] {
        let rows = vike_deribit::catalog::fetch_instruments("any", kind)
            .map_err(|e| format!("deribit {kind} instruments: {e}"))?;
        instruments.extend(rows);
    }
    Ok(OptionIndex::from_instruments(&instruments))
}
