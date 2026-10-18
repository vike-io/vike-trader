//! The DETERMINISTIC venue-feed splice test — scripted frames in, a paper fill out, no network,
//! the default CI lane (never `#[ignore]`d).
//!
//! `crates/vike-tradehub/tests/venue_feed_splice_smoke.rs` proves the splice LIVE (the shipped
//! binary against real Deribit mainnet, `#[ignore]`d, run explicitly); its module doc records why
//! a scripted CI test used to be structurally unavailable — reason (1): `wire_venue_feeds`
//! constructed each venue's feed object itself, so the only caller-injectable seam (`wrap`)
//! wrapped the SINK, never the stream. [`FeedCtors`] is the extraction that closed that reason,
//! and this module is the test it exists for.
//!
//! # What runs REAL here
//!
//! - the REAL [`super::venue_feed_plan`] gates (deribit's over an EMPTY credential map — the keyless
//!   arm's own property);
//! - the REAL [`super::wire_venue_feeds`] arm: it builds the sink chain (`wrap` over
//!   [`vike_tradehub_core::CoreLaneSink`]) onto a REAL paper multi-mount core
//!   (`vike_tradehub_core::build_paper_multi_strategy_core_with` — the exact mount seam
//!   `crates/vike-tradehub/tests/daemon/multi_mount_profile.rs` drives) and makes every
//!   `subscribe_*` call with the mount's own key;
//! - the REAL core: `CoreLaneSink` → ingest lanes → the runtime dispatching on the mount's own
//!   `(venue, symbol, interval)` key → the mounted `buy_hold` submitting → the paper book
//!   filling.
//!
//! # What is SUBSTITUTED, and why that is not the smoke's "green fake"
//!
//! The venue's whole feed: the [`FeedCtors`] seam hands the arm a [`ScriptedFeed`], a `DataClient` whose
//! lanes emit closed bars and a quote straight into the sink. Before wave 2 the double kept the VENUE's
//! own decode (the deribit and oanda frame parsers) and ran the shared session driver; that code is the
//! gatewayhub's now — this process links no bridge — so the decode is proven where it lives
//! (`crates/vike-gatewayhub`'s venue tests and the bridges' own fixture suites) and the hop by the
//! datahub's and the gatewayhub's loopback suites. What this test still pins is the half that is THIS
//! crate's: the arm's subscription LABELS and its sink chain onto the core lanes. The double keeps the
//! labelling rule that makes a cross-labelled arm detectable: its emissions carry the venue label
//! plus the SUBSCRIBED symbol, never a frame's spelling. So a cross-labelled subscription in the arm
//! (the `cex_feed_wiring_pin.rs` failure class the smoke names) makes the scripted lane emit under the
//! wrong key, the core dispatches none of it to the mount, and the FILL assert fails — the exact
//! defect this file was red against when it was planted (`subscribe_bars` handed a foreign literal
//! instead of `c.token_id`).
//!
//! # What one green run does NOT prove
//!
//! The dial: the datahub's strategy lane, the gatewayhub's hop, hosts, TLS, reconnect/backoff against a
//! real venue — that stays the smoke's job, which stays `#[ignore]`d and live, and the hop harnesses'.
//! And one venue's arm is one venue's arm: the other arms still rest on their `*_plan`/`*_arming` unit
//! tests, the wiring text pins, and the smoke.
//!
//! # The SECOND case: the OANDA data-only splice (the first credentialed-data conversion)
//!
//! The smoke's credentialed-data venues (alpaca/ctrader/oanda/ig) were structural GAPs: their
//! feed credentials arm exec from the same store, so no store could let the feed mount while a
//! validation run's exec stayed paper — and their live splices cannot even run on a weekend (FX
//! and equities close). The `data_only` profile declaration is the seam that unblocks both, and
//! [`a_data_only_oanda_mount_keeps_exec_paper_and_scripted_frames_reach_the_strategy`] is its
//! deterministic proof, one level DEEPER than the deribit case: it drives
//! [`super::live_runtime_with`] — the REAL mount path (every venue's plan gate, the credential
//! WITHHOLD, the REAL `vike_tradehub_core::build_live_multi_strategy_core` wired-market runtime, the REAL
//! wire arm) — over a FAKE-key account row (never real credentials, never the real store) and a scripted
//! [`FeedCtors::client`] constructor, then asserts on `assemble_runtime`'s own `live_venues` record —
//! the exec ARMING STATE, not the banner — that the declaration kept exec paper. The fill assert
//! doubles as a BEHAVIOURAL paper-exec proof: a live exec client ignores the core's `on_bar`
//! fill seam (its fills arrive on a user-data stream this test never scripts), so only the paper
//! book can fill the scripted bars.
//!
//! The feed's own credentials are NOT part of this seam any more: the tradehub resolves none (the
//! gatewayhub reads the `paper` account behind a `data_only` venue itself,
//! `AccountDirectory::data_feed_credentials`), so what the old case asserted about "the constructor
//! received the plan's credentials" moved to the gatewayhub's `accounts_tests.rs`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_data::{DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId};
use vike_tradehub_core::{
    MakerConfig, PaperCoreOpts, PaperHalt, StrategyMountSpec, build_paper_multi_strategy_core_with,
};

use super::super::{
    BackendCtor, FeedCtors, MountRows, ResolvedAdapter, live_runtime_with, ready_mode_line,
    venue_feed_plan, wire_venue_feeds, wired_symbol,
};
use super::StubCtors;

#[cfg(test)]
mod account_locks;
#[cfg(test)]
mod credentialed;
#[cfg(test)]
mod deribit;
#[cfg(test)]
mod fail_closed;
#[cfg(test)]
mod feed_outage;
#[cfg(test)]
mod oanda;
#[cfg(test)]
mod ready_banner;

/// One flat closed 1m bar at `ts` (epoch ms) priced `px`.
fn flat_bar(ts: i64, px: f64) -> vike_marketdata::Bar {
    vike_marketdata::Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The scripted stand-in a splice test's [`FeedCtors::client`] hands the REAL arm: records every
/// subscribe the arm makes (the LABEL half of the splice) and serves the bar and quote lanes one scripted
/// run each (the DATA half), on [`FeedRegistry`] threads exactly like the real clients (same
/// spawn/stop/join bookkeeping), so the teardown the arm's caller performs is the production path too.
///
/// The emissions keep the real lanes' labelling rule: the venue `label` plus the SUBSCRIBED symbol,
/// never a frame's own spelling. Four ascending closed bars are enough to trigger the mounted `buy_hold`
/// (its first event) AND fill its market order (paper fills are next-bar, through the core's `on_bar`
/// seam). `full` is the venue's verb set: deribit serves trades and a book (label-recorded, no frames:
/// prints feed the `PriceBoard`'s last-trade rung and drive no strategy verb), oanda serves neither (the
/// real client's caps refusal, kept so an arm padded out to "look like its neighbours" fails the mount
/// loudly here exactly as it would in production).
pub(super) struct ScriptedFeed {
    label: &'static str,
    full: bool,
    sink: Arc<dyn LiveDataSink>,
    subs: Arc<Mutex<Vec<(&'static str, String)>>>,
    registry: FeedRegistry,
}

impl ScriptedFeed {
    pub(super) fn new(
        label: &'static str,
        full: bool,
        sink: Arc<dyn LiveDataSink>,
        subs: Arc<Mutex<Vec<(&'static str, String)>>>,
    ) -> Self {
        Self { label, full, sink, subs, registry: FeedRegistry::new() }
    }

    fn record(&self, verb: &'static str, key: String) {
        self.subs.lock().expect("subs record").push((verb, key));
    }

    fn spawn(
        &mut self,
        name: String,
        body: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.registry.spawn(name, body).map_err(|e| LiveDataError::Subscribe(e.to_string()))
    }
}

impl DataClient for ScriptedFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.record("bars", format!("{symbol}@{interval}"));
        let (sink, label) = (Arc::clone(&self.sink), self.label);
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        self.spawn(format!("scripted-{label}-{symbol}@{interval}"), move |_stop| {
            for i in 1..=4_i64 {
                sink.close_bar(label, &symbol, &interval, flat_bar(60_000 * i, 100.0 + i as f64));
            }
        })
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.record("quotes", symbol.to_string());
        let (sink, label) = (Arc::clone(&self.sink), self.label);
        let symbol = symbol.to_string();
        self.spawn(format!("scripted-{label}-{symbol}-quotes"), move |_stop| {
            sink.quote(
                label,
                &symbol,
                vike_marketdata::QuoteTick {
                    ts: 60_000,
                    local_ts: vike_model::now_ms(),
                    bid: 99.5,
                    ask: 100.5,
                    bid_size: 40.0,
                    ask_size: 50.0,
                    symbol: symbol.clone(),
                },
            );
        })
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if !self.full {
            return Err(LiveDataError::Unsupported("this venue serves no trade tape"));
        }
        self.record("trades", symbol.to_string());
        let label = self.label;
        self.spawn(format!("scripted-{label}-{symbol}-trades"), |_stop| {})
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if !self.full {
            return Err(LiveDataError::Unsupported("this venue serves no L2 book"));
        }
        self.record("book", symbol.to_string());
        let label = self.label;
        self.spawn(format!("scripted-{label}-{symbol}-book"), |_stop| {})
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

// Same as `crates/vike-tradehub-core/tests/common/mod.rs`'s `wait_until` (`secs`, 10 ms poll); NOT
// `crates/vike-cli/tests/common/mod.rs`'s (20 ms) nor the `Duration`-taking copies.
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// One `buy_hold` mount (size 1.0) of `venue` on its wired 1m market — `data_only = true` declared
/// when `data_only` — resolved through the daemon's real profile load path: parsed, validated,
/// passed through the pure live gate, lowered, and its strategy resolved from the registry.
fn buy_hold_mount(venue: &str, data_only: bool) -> ResolvedAdapter {
    let symbol = wired_symbol(venue).unwrap_or_else(|| panic!("assemble_runtime mounts {venue}"));
    let declaration = if data_only { "data_only = true\n" } else { "" };
    let toml = format!(
        "venue = \"{venue}\"\nsymbol = \"{symbol}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\
         {declaration}\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = 1.0\n"
    );
    let row = crate::config::DaemonProfile::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("the {venue} profile parses and validates: {e:?}"));
    row.validate_for_live().expect("…and passes the pure live gate");
    let cfg = row.to_maker_config();
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("buy_hold resolves from the registry");
    ResolvedAdapter { row, cfg, spec, strategy }
}

/// What a seam mounts UNDER: the machine `policy` rows, and the `account` table snapshot that arms
/// its venues (decision 0119 — an account trades at its own tier while active; no `policy` row arms
/// anything). Production reads the snapshot in `live_runtime`; a seam
/// plants it, which is the one substitution [`live_runtime_with`]'s `accounts` parameter exists for.
pub(super) struct SeamPolicy {
    pub(super) policy: vike_config::Policy,
    pub(super) accounts: vike_secrets::account_directory::AccountDirectory,
}

impl SeamPolicy {
    /// The default machine policy plus ONE active, unlabelled `account` row per `(venue, tier)`
    /// given — the rows a box arms those venues' default accounts with.
    pub(super) fn armed(rows: &[(&str, vike_model::AccountTier)]) -> Self {
        let planted =
            rows.iter().fold(vike_tradehub_core::AdapterPolicy::default(), |p, (venue, tier)| {
                p.with_account(
                    venue,
                    &vike_model::accounts::account_keys::AccountLabel::Default,
                    *tier,
                )
            });
        SeamPolicy { policy: vike_config::Policy::default(), accounts: planted.accounts }
    }

    /// The same rows, holding `book` as every account's (and venue's) credentials, by FIELD on the
    /// account ROW id (decision 0156 item 6) — the store's credential half, planted. Called AFTER
    /// the rows are planted: [`vike_tradehub_core::AdapterPolicy::with_account`] rebuilds the directory.
    pub(super) fn holding(self, book: vike_secrets::account_credentials::CredentialBook) -> Self {
        SeamPolicy { accounts: self.accounts.with_credentials(book), ..self }
    }

    /// The [`vike_tradehub_core::AdapterPolicy`] `live_runtime_with` builds from these two — for a test's
    /// anti-vacuity probe, which must ask the projection the mount itself will ask.
    pub(super) fn adapter_policy(&self) -> vike_tradehub_core::AdapterPolicy {
        vike_tradehub_core::AdapterPolicy {
            accounts: self.accounts.clone(),
            ..vike_tradehub_core::AdapterPolicy::from(&self.policy)
        }
    }
}

/// [`live_runtime_with`] with every argument these seams hold fixed: no run profile (they assert the
/// mount path, not the `[guards]` wiring), the default flags, no origin (an untagged instance is
/// the pre-feature behaviour every one of them was written against), no WAL (an empty rung resolves
/// the same `None` journal a box with no `config.journal_dir` row resolves) and no
/// `venue_setting` rows (every feed keeps its charter default). The `account` rows are the
/// seam's own ([`SeamPolicy`]), never the box's store.
fn seam_live_runtime(
    mounts: Vec<ResolvedAdapter>,
    budget: Option<vike_model::ProfileRisk>,
    seam: &SeamPolicy,
    vars: HashMap<String, String>,
    lock_dir: &std::path::Path,
    make: &dyn FeedCtors,
) -> Result<super::super::live_runtime::LiveRuntime, String> {
    seam_live_runtime_over(mounts, budget, seam, vars, lock_dir, make, &LocalCtor)
}

/// The in-process backend over this daemon's own registry — what the live mount ran over before the
/// venues moved behind the gatewayhub, and what these seams keep driving: they assert the mount path
/// (plan gate, withhold, assemble, feed wiring), not the wire, which `tests/remote_backend_wire.rs`
/// drives over a fake gatewayhub. W9b replaces it with a `ScriptedBackend` when the registry leaves.
pub(super) struct LocalCtor;

impl BackendCtor for LocalCtor {
    fn backend(
        &self,
        vars: &HashMap<String, String>,
        policy: &vike_tradehub_core::AdapterPolicy,
        _data_only: &std::collections::HashSet<String>,
    ) -> std::sync::Arc<dyn vike_tradehub_core::VenueBackend> {
        // The policy is already post-withhold at the mount call: the scripted table needs no overlay.
        let _ = vars;
        std::sync::Arc::new(vike_tradehub_core::ScriptedBackend::credentialed(
            policy,
            &crate::scripted_arming_tests::absent_venues(),
        ))
    }
}

/// [`seam_live_runtime`] over a backend of the test's choosing.
fn seam_live_runtime_over(
    mounts: Vec<ResolvedAdapter>,
    budget: Option<vike_model::ProfileRisk>,
    seam: &SeamPolicy,
    vars: HashMap<String, String>,
    lock_dir: &std::path::Path,
    make: &dyn FeedCtors,
    ctor: &dyn BackendCtor,
) -> Result<super::super::live_runtime::LiveRuntime, String> {
    live_runtime_with(
        mounts,
        budget,
        None,
        &seam.policy,
        vike_config::Flags::default(),
        vars,
        lock_dir,
        None,
        &crate::profile_rows::JournalRung::default(),
        make,
        seam.accounts.clone(),
        // No journal (the empty rung above), so nothing to restore.
        vike_core::replay::BootRestore::off(),
        // No `config.*` rows of the trading-mount batch: every default, the mount as it was.
        MountRows::default(),
        ctor,
    )
}

/// Teardown through the production handles: every feed joined, the live-event forwarder stopped,
/// then the core — `main`'s order, without the deadline scaffolding.
fn tear_down(handle: vike_core::CoreHandle, mut teardown: crate::feeds::LiveTeardown) {
    for f in &mut teardown.feeds {
        f.shutdown();
    }
    teardown.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.shutdown_and_join();
}

/// The `LIVE-<venue>.lock` sentinels present in `dir`, sorted — a claim's only observable trace.
fn sentinels(dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .expect("the throwaway state dir is readable")
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(vike_boot::live_lock::LIVE_LOCK_PREFIX))
        .collect();
    out.sort();
    out
}
