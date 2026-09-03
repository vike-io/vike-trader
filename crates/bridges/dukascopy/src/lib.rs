//! Dukascopy FX venue bridge. Moved into crates/bridges/dukascopy (crate-reorg Phase 3, PR J —
//! the last venue move); the colocated Java sidecar Gradle project lives at
//! crates/bridges/dukascopy/jforex-bridge/ (sibling of src/).
//!
//! `data.rs` is the keyless deep tick-history source (`.bi5`), already ported and live-tested.
//! `config.rs` carries the JForex login creds (two demo accounts) AND the pure resolution of the
//! two runtime artifacts the sidecar is spawned with ([`resolve_dukascopy_tools`]). `exec.rs`
//! drives the JForex Java sidecar (`jforex-bridge/`) over JSON-lines stdio (`proto.rs`); without
//! the jar it degrades to Unavailable — the live gate. `netting.rs` re-anchors the core's blended
//! fold to the sidecar's per-order netting truth (law A7) via the `position` protocol line.

mod catalog;
mod config;
mod data;
mod exec;
mod netting;
mod proto;
pub mod recon_client;

pub use catalog::{bundled_instruments, DukascopyCatalog};
pub use config::{
    dukascopy_env_var_names, load_dukascopy_config_from, resolve_dukascopy_tools, DukascopyAccount,
    DukascopyConfig, DukascopyTools, BRIDGE_JAR_FILE, JAVA_HOME_ENV, JFOREX_BRIDGE_JAR_ENV,
    JFOREX_TOOL_DIR, JRE_TOOL_DIR,
};
pub use data::{
    decode_ticks, decompress, fetch_bars_range, fetch_hour, fetch_ticks_range, hour_url,
    point_divisor, ticks_to_bars, Tick,
};
pub use exec::{DukascopyError, DukascopyExecutionClient};
pub use proto::{encode_line, parse_command, parse_envelope, Command, Envelope};
pub use recon_client::{DukascopyReconClient, PositionSnapshot, VenuePosition};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::DUKASCOPY;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("dukascopy");
        assert_eq!(super::CAPS, caps);
        // the JForex sidecar client wires submit/cancel only; modify is the trait no-op. reduce-only
        // is declared CONSERVATIVELY false (the Java netting side is unverifiable from Rust). No live
        // DataClient, but `.bi5` TICK backfill in vike-backfill — plus BARS resampled from those
        // ticks (`vike_backfill::dukascopy` calls `hist.resample_quotes_to_bars`), which is why
        // `backfill_bars` is TRUE. The field means "can produce bars from the venue", settled in
        // its own doc; `vike_backfill::caps` has always declared `{bar: true, quote: true}` here,
        // and the two tables were in open contradiction until that split was resolved.
        assert!(!caps.supports_modify);
        assert!(!caps.supports_reduce_only);
        assert!(caps.backfill_ticks && caps.backfill_bars);
        assert!(!caps.has_live_data());
        // Expanded axes (w2-task-5): the Rust side forwards the OrderRequest verbatim; the Java
        // sidecar (`StrategyBridge.java`) accepts exactly `"market"`/`"limit"` and terminally
        // rejects every other order_type — the preflight only moves that refusal to the core edge.
        // No trigger kinds; TIF/margin forwarded but never honored → default rows; no batch.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit"]);
        assert_eq!(caps.trigger_types, &[] as &[TriggerType]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
