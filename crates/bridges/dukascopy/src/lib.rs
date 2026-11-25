//! Dukascopy FX venue bridge. Moved into crates/bridges/dukascopy (crate-reorg Phase 3, PR J —
//! the last venue move); the colocated Java sidecar Gradle project lives at
//! crates/bridges/dukascopy/jforex-bridge/ (sibling of src/).
//!
//! `history.rs` is the keyless deep tick-history source (`.bi5`), already ported and live-tested: the
//! FETCH half; `ticks.rs` serves it to the gatewayhub's tick engine an hour file at a time
//! (`TicksSpecDukascopy`). The `.bi5` FORMAT — the tick record and its decoders, the archive import lane's
//! daily-file grammar and refusing decoder, the per-instrument price scale — is the leaf crate
//! `vike-bi5`'s (`crates/vike-bi5/src/lib.rs`), and callers name it there.
//! `config.rs` reads one account's JForex login (its `LOGIN` / `PASSWORD` fields) AND carries the
//! pure resolution of the three runtime facts the sidecar is spawned with — the JVM, the jar and
//! that JVM's `user.home` ([`resolve_dukascopy_tools`]). `execution/mod.rs`
//! drives the JForex Java sidecar (`jforex-bridge/`) over JSON-lines stdio (`transport.rs`); without
//! the jar it degrades to Unavailable — the live gate. `execution/netting.rs` re-anchors the core's blended
//! fold to the sidecar's per-order netting truth (law A7) via the `position` protocol line.
//! `reconcile/mod.rs` and `reconcile/query.rs` are the RECONCILE leg: a `recon` question put to the sidecar (the venue's own
//! orders and net positions, `ReconSnapshot.java`) and the three report pulls built from its one answer.
//! `feed.rs` is the PRICE LANE (any-symbol): the bus the exec client's sidecar and the gatewayhub's
//! credentialed stream meet on, over the `subscribe` command and the `tick` envelope. `account/quote_rate.rs` turns
//! the tick of a conversion pair into the rate that books a foreign-quoted pair in the account's currency.
//! `account/ready_balance.rs` turns the sidecar's `ready` balance into the engine's one wallet frame per start (USD account
//! on a USD-quoted mount only). `account/pick.rs` answers which `account` ROW a start trades — moved down from `vike-tradehub-core` by
//! decision 0088 (Verdict 3). `mount.rs` is this venue's `VenueAdapter` — the arm, the account
//! CHOICE over the root's store read, the one-sidecar refusal texts and the confirmation park,
//! moved down by the venue-adapter contract (docs/decisions/0096); only the generic one-per-process
//! RULE stayed in `vike-tradehub-core`.

pub mod adapter;
mod catalog;
mod config;
// The account's ledger currency, the mounted pair's money and the rate a converting node's facts answer carries.
pub mod ledger;
// Every refusal this crate makes, its id and its one text (`vike_utility::refusal_enum!`).
pub mod refusals;
// The crate's own test helpers (the sidecar-side pieces the price-lane tests share, here and in the
// gatewayhub's; the market order its protocol tests and `sidecar` suites send), for its unit tests and,
// through the self dev-dependency, its `tests/` binaries; never in a normal build.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
// The tick lane's spec: the gatewayhub's `Fetch { Ticks }`, an hour file at a time.
pub mod ticks;

pub mod account;
mod execution;
pub mod feed;
mod history;
pub mod instrument;
pub mod reconcile;
mod transport;

pub use account::pick::{
    DukascopyAccount, DukascopyRefusal, DukascopyRefusalId, confirmation_for, resolve_account,
};
pub use catalog::CATALOG_CLASSES;
pub use config::{
    BRIDGE_JAR_FILE, DukascopyConfig, DukascopyTools, JAVA_HOME_ENV, JFOREX_BRIDGE_JAR_ENV,
    JFOREX_HOME_SUBDIR, JFOREX_TOOL_DIR, JRE_TOOL_DIR, load_dukascopy_config_from,
    resolve_dukascopy_tools,
};
pub use execution::{DukascopyError, ExecutionClientDukascopy};
pub use history::{DukascopyTicks, ticks_to_bars};
pub use instrument::terms::VenueTerms;
pub use reconcile::query::ReconcileAnswer;
pub use reconcile::{ReconcileClientDukascopy, ReconcileSource};
pub use transport::{
    AdoptedOrder, Envelope, ReconcileOrder, ReconcilePosition, SidecarCommand, UnsettledVerb,
    encode_line, parse_command, parse_envelope,
};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::DUKASCOPY;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("dukascopy");
        assert_eq!(super::CAPABILITIES, capabilities);
        // the JForex sidecar client wires submit/cancel and modify (`IOrder.setOpenPrice` /
        // `setRequestedAmount`, `StrategyBridge.handleModify`). reduce-only
        // is declared CONSERVATIVELY false (the Java netting side is unverifiable from Rust). No live
        // DataClient, but `.bi5` TICK backfill in vike-backfill — plus BARS resampled from those
        // ticks (`vike_backfill::venues::dukascopy` calls `hist.resample_quotes_to_bars`), which is why
        // `backfill_bars` is TRUE. The field means "can produce bars from the venue", settled in
        // its own doc; `vike_backfill::capabilities` has always declared `{bar: true, quote: true}` here,
        // and the two tables were in open contradiction until that split was resolved.
        assert!(capabilities.supports_modify);
        assert!(!capabilities.supports_reduce_only);
        assert!(capabilities.backfill_ticks && capabilities.backfill_bars);
        assert!(!capabilities.has_live_data());
        // Expanded axes (w2-task-5): the Rust side forwards the OrderRequest verbatim; the Java
        // sidecar (`StrategyBridge.java`) accepts `"market"`/`"limit"`/`"stop"` and terminally
        // rejects every other order_type — the preflight only moves that refusal to the core edge.
        // The one trigger kind is the stop-loss of an open position (`handleStop`: the venue holds the
        // order's stop-loss price); the take-profit is not a trigger KIND here but a reduce-only limit (the
        // venue holds the order's take-profit price, `Protection.TAKE_PROFIT`), so the reduce-only kinds are
        // all three; TIF/margin forwarded but never honored → default rows; no batch.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        // + the venue's own stop-limit and if-touched ENTRY orders (`TriggerEntry.java`), never reduce-only
        assert_eq!(
            capabilities.supported_order_kinds,
            &["market", "limit", "stop", "stop_limit", "take_profit"]
        );
        assert_eq!(capabilities.reduce_only_kinds, &["market", "limit", "stop"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        assert_eq!(capabilities.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(!capabilities.supports_post_only);
    }

    /// **The core refuses a ladder WHOLE on dukascopy, in the sidecar's words**
    /// (`crates/vike-venues/src/venue_caps.rs`'s `WHOLE_EXIT_VENUES`): a stop-loss and a take-profit
    /// are a price on a whole JForex order, so a ladder's partial target lands inside the entry's
    /// order, and `StopPlan.java` refuses it in a string literal that carries the row's
    /// `bridge_words` (`StopPlanTest.java` runs that refusal). The Java is read at run time (the
    /// public mirror ships no `jforex-bridge/` sources a test may `include_str!`), and a checkout
    /// without it says so.
    #[test]
    fn the_whole_exit_row_quotes_the_sidecars_refusal_of_a_part_of_an_order() {
        let row = vike_venues::venue_caps::WHOLE_EXIT_VENUES
            .iter()
            .find(|w| w.venue == "dukascopy")
            .expect("dukascopy is a whole-exit venue");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("jforex-bridge/src/main/java/vike/jforex/StopPlan.java");
        let Ok(java) = std::fs::read_to_string(&path) else {
            eprintln!("SKIPPED: {} is not in this checkout", path.display());
            return;
        };
        // the literal's opening quote, so a comment that says the same does not count
        let literal = format!("\" {}", row.bridge_words);
        assert!(java.contains(&literal), "{} has no literal {literal:?}", path.display());
    }
}
