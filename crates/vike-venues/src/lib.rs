//! vike-venues — the per-venue TABLES: what each venue can do, as one NAMED row per roster venue.
//!
//! They were `vike-model`'s `venues/` module (plus its fee registry, the credential-field table and
//! the attribution mechanic) until decision 0162 cut them out: an edit of one venue's row rebuilt
//! every crate that depends on the vocabulary, including the ten that name no table at all. What
//! stays in `vike-model` is the roster (`vike_model::VENUES`, `vike_model::venues::roster_id`) and
//! the vocabulary the tables are rows OVER (`MarginMode`, `FeeSchedule`, `OrderRequest`, `Event`,
//! `AccountTier`). This crate sits one hop above it, at `layer = 12`.
//!
//! **The contract is the roster's** (`vike_model::venues`' module doc): every table's completeness
//! test iterates `vike_model::VENUES` and demands a declared row for each, so adding a venue to the
//! roster fails every table's tests until its rows land. The tables declare their `just new-venue`
//! insertion points as marker comments; the SET of them is derived by `just new-venue-sites` and
//! gated by `crates/vike-ops/tests/venues/new_venue_gate.rs`, never listed here.
//!
//! **Spelling.** A table is named by its module path, `vike_venues::venue_caps::VenueCapabilities`; the
//! names that were spelled from the old crate root are spelled from THIS crate's root
//! (`vike_venues::capabilities_for`), and the three `pub(crate)` modules below are spelled from the root
//! only. There is no re-export of any of it from `vike-model`.

#![warn(unreachable_pub)]

pub mod attribution;
pub mod credential_keys;
// Every venue's credential key names as one typed table, and `CredentialKey`, the name's type.
pub mod credential_schema;
/// The fee LANE of a `(venue, symbol)` pair (`fee_lane`, the key `fee_schedule_for` takes) and the
/// symbol-string readers it rests on, down from `vike-catalog`; spelled from the crate root only.
pub(crate) mod fee_lane;
// The per-venue static fee registry (`fee_schedule_for`) and the Polymarket V2 fee curve. The
// `FeeSchedule` type and the round-trip fee math stay in `vike-model`.
pub mod fees;
/// IBKR's one session (decision 0140): `IbkrBackend` and `ibkr_session`, spelled from the crate
/// root only (the `AccountTier` precedent, decision 0129).
pub(crate) mod ibkr_session;
pub mod link_deadman;
pub mod session;
pub mod venue_amend;
/// How a venue CHANGES a resting order of one kind: in place, by cancel-then-new, or not at all
/// (`AmendMode`, derived from the capability row plus a short exception list). Named by module path.
pub mod venue_amend_mode;
pub mod venue_caps;
/// What a venue calls our `demo` tier, a display hint (decision 0156), spelled from the crate root
/// only (the `AccountTier` precedent, decision 0129).
pub(crate) mod venue_demo_word;
pub mod venue_fields;
pub mod venue_hold;
pub mod venue_margin_support;
pub mod venue_order_client_id_budget;
pub mod venue_rate_limits;
pub mod venue_restore;
/// `TierScope` (decision 0140), spelled from the crate root only.
pub(crate) mod venue_tier_scope;
/// The per-venue time-in-force table. It came DOWN from `vike-bridge-core` (the module doc says
/// why) and is NOT re-exported at the crate root: every consumer names it
/// `vike_venues::venue_time_in_force::…`, and a second spelling at the root would be an alias with no caller.
pub mod venue_time_in_force;
/// The per-venue CONNECTION and LIVENESS timing table (decision 0160): an exec lane, a market lane
/// and a depth block per venue, every value a `Timing` carrying its own `why`.
pub mod venue_timings;
/// Every venue ADDRESS as `venue_url` rows: the vocabulary, the release's seed (`VENUE_URL_SEED`)
/// and `VenueUrls::url`, the ONE lookup. Named by module path, never re-exported at the root.
pub mod venue_urls;
// How the trading daemon wires each venue; never a symbol. Not re-exported at the crate root: every
// consumer names `vike_venues::venue_wiring::…`. No outer `///` here: rustdoc would merge it with
// `venue_wiring.rs`'s `//!` and resolve that module doc's links from THIS scope, where `EngineMount` is not.
pub mod venue_wiring;

pub use fee_lane::{
    DEX_MARKER, NAMED_FEE_LANE_WARNING, PAIR_SEPARATOR, PERP_SUFFIX, SpotPairRow, SpotPairSpelling,
    fee_lane, fiat_code, is_day_month_year, is_hyperliquid_spot, kept_fee_lane, split_perp,
    spot_pair_row,
};
pub use fees::{
    POLYMARKET_PROB_CURVE, POLYMARKET_V2_FEE_CURVE, fee_schedule_for,
    fee_schedule_for_with_pm_curve,
};
pub use ibkr_session::{IbkrBackend, IbkrSession, NotAnIbkrBackend, ibkr_session};
pub use link_deadman::{LinkDeadMan, link_deadman_default};
pub use session::{
    DAY_MINUTES, SessionCalendar, SessionCalendarSegment, SessionCalendarState, WEEK_MINUTES,
    session_for, venue_is_open,
};
pub use venue_amend::{AmendSemantics, amend_capabilities_note, amend_semantics};
pub use venue_caps::{
    DataClientVerb, EMULATION_REFUSED_VENUES, OrderEmulationKind, PreflightDeny, TriggerType,
    VenueCapabilities, balance_is_wallet_cash, capabilities_for, emulated_child, emulation_of,
    order_list_refusal, preflight_order, preflight_order_at,
};
pub use venue_demo_word::venue_demo_word;
pub use venue_hold::{MS_PER_SECOND, POLYMARKET_ITODE_HOLD_MS, POLYMARKET_SPORTS_GAME_HOLD_MS};
pub use venue_margin_support::venue_margin_support;
pub use venue_rate_limits::{
    VenueRateLimits, VenueRateLimitsHistory, VenueRateLimitsMarket, VenueRateLimitsMeter,
    VenueRateLimitsProvenance, rate_limits_for,
};
pub use venue_tier_scope::{TierScope, venue_tier_scope};
