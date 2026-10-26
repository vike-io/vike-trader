//! Dukascopy FX venue bridge. Moved into crates/bridges/dukascopy (crate-reorg Phase 3, PR J —
//! the last venue move); the colocated Java sidecar Gradle project lives at
//! crates/bridges/dukascopy/jforex-bridge/ (sibling of src/).
//!
//! `data.rs` is the keyless deep tick-history source (`.bi5`), already ported and live-tested: the
//! FETCH half. The `.bi5` FORMAT — the tick record and its decoders, the archive import lane's
//! daily-file grammar and refusing decoder, the per-instrument price scale — is the leaf crate
//! `vike-bi5`'s (`crates/vike-bi5/src/lib.rs`), and callers name it there.
//! `config.rs` reads one account's JForex login (its `LOGIN` / `PASSWORD` fields) AND carries the
//! pure resolution of the three runtime facts the sidecar is spawned with — the JVM, the jar and
//! that JVM's `user.home` ([`resolve_dukascopy_tools`]). `exec.rs`
//! drives the JForex Java sidecar (`jforex-bridge/`) over JSON-lines stdio (`proto.rs`); without
//! the jar it degrades to Unavailable — the live gate. `netting.rs` re-anchors the core's blended
//! fold to the sidecar's per-order netting truth (law A7) via the `position` protocol line.
//! `quotes.rs` is the PRICE LANE (any-symbol): the bus the exec client's sidecar and the gatewayhub's
//! credentialed stream meet on, over the `subscribe` command and the `tick` envelope. `home_conversion.rs` turns
//! the tick of a conversion pair into the rate that books a foreign-quoted pair in the account's currency.
//! `ready_balance.rs` turns the sidecar's `ready` balance into the engine's one wallet frame per start (USD account
//! on a USD-quoted mount only). `account.rs` answers which `account` ROW a start trades — moved down from `vike-tradehub-core` by
//! decision 0088 (Verdict 3). `mount.rs` is this venue's `VenueAdapter` — the arm, the account
//! CHOICE over the root's store read, the one-sidecar refusal texts and the confirmation park,
//! moved down by the venue-adapter contract (docs/decisions/0096); only the generic one-per-process
//! RULE stayed in `vike-tradehub-core`.

mod account;
pub mod adapter;
pub mod any_symbol;
mod catalog;
mod config;
mod data;
mod exec;
pub mod home_conversion;
pub mod jforex_grid;
mod netting;
mod proto;
pub mod quotes;
mod ready_balance;
pub mod recon_client;

pub use account::{DukascopyAccount, DukascopyRefusal, confirmation_for, resolve_account};
pub use catalog::{DukascopyCatalog, bundled_instruments};
pub use config::{
    BRIDGE_JAR_FILE, DukascopyConfig, DukascopyTools, JAVA_HOME_ENV, JFOREX_BRIDGE_JAR_ENV,
    JFOREX_HOME_SUBDIR, JFOREX_TOOL_DIR, JRE_TOOL_DIR, load_dukascopy_config_from,
    resolve_dukascopy_tools,
};
pub use data::{
    fetch_bars_range, fetch_hour, fetch_quotes_range, fetch_ticks_range, hour_url, point_divisor,
    ticks_to_bars,
};
pub use exec::{DukascopyError, DukascopyExecutionClient, SidecarState};
pub use proto::{Command, Envelope, encode_line, parse_command, parse_envelope};
pub use recon_client::{DukascopyReconClient, PositionSnapshot, VenuePosition};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_venues::VenueCaps = vike_venues::venue_caps::DUKASCOPY;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_venues::caps_for("dukascopy");
        assert_eq!(super::CAPS, caps);
        // the JForex sidecar client wires submit/cancel and modify (`IOrder.setOpenPrice` /
        // `setRequestedAmount`, `StrategyBridge.handleModify`). reduce-only
        // is declared CONSERVATIVELY false (the Java netting side is unverifiable from Rust). No live
        // DataClient, but `.bi5` TICK backfill in vike-backfill — plus BARS resampled from those
        // ticks (`vike_backfill::venues::dukascopy` calls `hist.resample_quotes_to_bars`), which is why
        // `backfill_bars` is TRUE. The field means "can produce bars from the venue", settled in
        // its own doc; `vike_backfill::caps` has always declared `{bar: true, quote: true}` here,
        // and the two tables were in open contradiction until that split was resolved.
        assert!(caps.supports_modify);
        assert!(!caps.supports_reduce_only);
        assert!(caps.backfill_ticks && caps.backfill_bars);
        assert!(!caps.has_live_data());
        // Expanded axes (w2-task-5): the Rust side forwards the OrderRequest verbatim; the Java
        // sidecar (`StrategyBridge.java`) accepts `"market"`/`"limit"`/`"stop"` and terminally
        // rejects every other order_type — the preflight only moves that refusal to the core edge.
        // The one trigger kind is the stop-loss of an open position (`handleStop`: the venue holds the
        // order's stop-loss price; no take-profit is wired); TIF/margin forwarded but never honored →
        // default rows; no batch.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
