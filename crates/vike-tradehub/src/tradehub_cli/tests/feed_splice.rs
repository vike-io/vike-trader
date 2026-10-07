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
//! - the REAL [`super::venue_feed_plan`] deribit gate, over an EMPTY credential map (the keyless
//!   arm's own property);
//! - the REAL [`super::wire_venue_feeds`] deribit arm: it builds the sink chain
//!   (`wrap` over [`vike_core::CoreLaneSink`]) onto a REAL paper multi-mount core
//!   (`vike_mount::build_paper_multi_strategy_core_with` — the exact mount seam
//!   `crates/vike-tradehub/tests/daemon/multi_mount_profile.rs` drives) and makes every
//!   `subscribe_*` call with the mount's own key;
//! - the REAL venue decode: each scripted lane feeds documented-grammar Deribit frames through
//!   `vike_deribit::market_data`'s own `parse_chart_bar`/`parse_quote` and the venue's
//!   [`BarFolder`] close inference (the successor bucket's first push IS the close signal) — the
//!   same functions the live `bars_main`/`quotes_main` lanes run;
//! - the REAL shared session driver: every scripted lane rides
//!   `vike_bridge_core::market_pump`'s `run_market_session` over a [`ScriptedStream`] (a real
//!   `MarketStream` impl), on a [`FeedRegistry`] thread with the production stop/join
//!   bookkeeping — the subscribe send and the frame loop are the production code path, not a
//!   hand-rolled iteration;
//! - the REAL core: `CoreLaneSink` → ingest lanes → the runtime dispatching on the mount's own
//!   `(venue, symbol, interval)` key → the mounted `buy_hold` submitting → the paper book
//!   filling.
//!
//! # What is SUBSTITUTED, and why that is not the smoke's "green fake"
//!
//! Exactly one thing: the venue's socket. The smoke's reason (3) refused substituting the whole
//! LANE — parse, fold, sink emission — behind a private seam, because that bypasses the labelling
//! the splice is about. Here the scripted lanes keep the venue's own labelling rule (emissions
//! carry the `"deribit"` const plus the SUBSCRIBED symbol — `quotes_main` and its siblings label
//! from the subscription, never from the frame) and serve frames FOR the subscribed channel, the
//! way the venue serves what was asked for. So a cross-labelled subscription in the arm (the
//! `cex_feed_wiring_pin.rs` failure class the smoke names) makes the scripted lane emit under the
//! wrong key, the core dispatches none of it to the mount, and the FILL assert fails — the exact
//! defect this file was red against when it was planted (`subscribe_bars` handed a foreign
//! literal instead of `c.token_id`).
//!
//! # What one green run does NOT prove
//!
//! The dial: hosts, TLS, reconnect/backoff against a real venue, the REST warmup seed — that
//! stays the smoke's job, which stays `#[ignore]`d and live. And one venue's arm is one venue's
//! arm: the other arms still rest on their `*_plan`/`*_arming` unit tests, the wiring text pins,
//! and the smoke.
//!
//! # The SECOND case: the OANDA data-only splice (the first credentialed-data conversion)
//!
//! The smoke's credentialed-data venues (alpaca/ctrader/oanda/ig) were structural GAPs: their
//! feed credentials arm exec from the same store, so no store could let the feed mount while a
//! validation run's exec stayed paper — and their live splices cannot even run on a weekend (FX
//! and equities close). The `data_only` profile declaration is the seam that unblocks both, and
//! [`a_data_only_oanda_mount_keeps_exec_paper_and_scripted_frames_reach_the_strategy`] is its
//! deterministic proof, one level DEEPER than the deribit case: it drives
//! [`super::live_mount_with`] — the REAL mount path (every venue's plan gate, the credential
//! WITHHOLD, the REAL `vike_mount::build_live_multi_strategy_core` wired-market node, the REAL
//! wire arm) — over a FAKE-key map (never real credentials, never the real store) and a scripted
//! [`FeedCtors::oanda`] constructor, then asserts on `build_node`'s own `live_venues` record —
//! the exec ARMING STATE, not the banner — that the declaration kept exec paper. The fill assert
//! doubles as a BEHAVIOURAL paper-exec proof: a live exec client ignores the core's `on_bar`
//! fill seam (its fills arrive on a user-data stream this test never scripts), so only the paper
//! book can fill the scripted bars.
//!
//! What is substituted is the venue client behind [`FeedCtors::oanda`] (production: the real
//! `vike_oanda::market_feed::Feeds`); the scripted double keeps the venue's own decode
//! (`vike_oanda::parse_candles`, `vike_oanda::market_data::decode_pricing_frame`), its own
//! channel derivations (`to_oanda_instrument`/`granularity`), and its labelling rule (the
//! SUBSCRIBED series, never the frame's `EUR_USD` spelling), over documented-grammar frames —
//! the same anti-green-fake line the deribit case draws. The dial (fxPractice hosts, the Bearer
//! header, reconnect) stays the weekday live smoke's job.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::market_pump::{
    FrameOutcome, MarketPumpOpts, PumpBackoff, run_market_session,
};
use vike_bridge_core::scripted::ScriptedStream;
use vike_data::{DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId};
use vike_deribit::market_data::{
    chart_channel, parse_chart_bar, parse_quote, public_subscribe_frame, quote_channel,
};
use vike_deribit::market_feed::BarFolder;
use vike_mount::{
    MakerMountConfig, PaperHalt, PaperMountOpts, StrategyMountSpec,
    build_paper_multi_strategy_core_with,
};

use super::super::{
    FeedCtors, ProdFeedCtors, ResolvedMount, live_mount_with, ready_mode_line, venue_feed_plan,
    wire_venue_feeds, wired_symbol_for,
};

#[cfg(test)]
mod account_locks;
#[cfg(test)]
mod deribit;
#[cfg(test)]
mod oanda;
#[cfg(test)]
mod ready_banner;

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
fn buy_hold_mount(venue: &str, data_only: bool) -> ResolvedMount {
    let symbol = wired_symbol_for(venue).unwrap_or_else(|| panic!("build_node mounts {venue}"));
    let declaration = if data_only { "data_only = true\n" } else { "" };
    let toml = format!(
        "venue = \"{venue}\"\nsymbol = \"{symbol}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\
         {declaration}\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = 1.0\n"
    );
    let row = crate::config::DaemonProfile::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("the {venue} profile parses and validates: {e:?}"));
    row.validate_for_live().expect("…and passes the pure live gate");
    let cfg = row.to_mount_config();
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("buy_hold resolves from the registry");
    ResolvedMount { row, cfg, spec, strategy }
}

/// [`live_mount_with`] with every argument these seams hold fixed: no run profile (they assert the
/// mount path, not the `[guards]` wiring), the default flags, no origin (an untagged instance is
/// the pre-feature behaviour every one of them was written against), no WAL (an empty map resolves
/// the same `None` journal the process env would on a box that sets neither variable) and no
/// `venue_setting` rows (every feed keeps its charter default).
fn seam_mount(
    mounts: Vec<ResolvedMount>,
    budget: Option<vike_exec::ProfileRisk>,
    policy: &vike_config::Policy,
    vars: HashMap<String, String>,
    lock_dir: &std::path::Path,
    make: &dyn FeedCtors,
) -> Result<super::super::live_mount::LiveMount, String> {
    live_mount_with(
        mounts,
        budget,
        None,
        policy,
        vike_config::Flags::default(),
        vars,
        lock_dir,
        None,
        &HashMap::new(),
        make,
        std::collections::BTreeMap::new(),
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
        .filter(|n| n.starts_with(vike_ops::live_lock::LIVE_LOCK_PREFIX))
        .collect();
    out.sort();
    out
}
