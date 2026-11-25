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
/// The lane-twin table (`LANE_TWINS`: a venue whose one account row runs a second engine for its other exec
/// lane), down from `vike-secrets` on 2026-11-22; spelled from the crate root only. The row it derives stays there.
pub(crate) mod lane_twins;
/// The account-leverage table (`LEVERAGE_VENUES`: which venue can make an account's stated leverage real, at
/// which tiers, and how) and its pure refusals, down from `vike-secrets` on 2026-11-23 (decision 0182); spelled
/// from the crate root only. The stated figure (`AccountLeverage`) stays a store column there.
pub(crate) mod leverage_venues;
pub mod link_deadman;
// The one reconnect ladder of every loop (`ReconnectBackoff`, over a `venue_timings` reconnect row). Named by module
// path, never re-exported at the root. No outer `///`: see `venue_wiring`.
pub mod reconnect;
pub mod session;
pub mod venue_amend;
/// How a venue CHANGES a resting order of one kind: in place, by cancel-then-new, or not at all
/// (`AmendMode`, derived from the capability row plus a short exception list). Named by module path.
pub mod venue_amend_mode;
pub mod venue_caps;
// The per-venue CONNECTION SHAPE table (`connection_for`: which loop runs each lane, how a gap is read back, who
// reports and who rebuilds), the sibling of `venue_timings`. Named by module path. No outer `///`: see `venue_wiring`.
pub mod venue_connection;
// Which symbols a venue's lane rates into the account's currency, what its currency read costs and what its engine
// counts money in (`venue_currency_for`). Named by module path, never re-exported at the root. No outer `///`: see
// `venue_wiring`.
pub mod venue_currency;
// How long before a dated contract's expiry a venue takes no opening order (`delivery_guard_for`), and the one verdict
// over that window. Named by module path, never re-exported at the root. No outer `///`: see `venue_wiring`.
pub mod venue_delivery_guard;
/// What a venue calls our `demo` tier, a display hint (decision 0156), spelled from the crate root
/// only (the `AccountTier` precedent, decision 0129).
pub(crate) mod venue_demo_word;
pub mod venue_fields;
// Whether a venue's stream and its fill history name one fill by one trade id (the reconcile pass's booking of our
// own missed fill reads it). Named by module path, never re-exported at the root. No outer `///`: see `venue_wiring`.
pub mod venue_fill_identity;
// Whether the HALT sentinel's `verify` mode can verify anything at a venue (`halt_verify_support`), down from
// `vike-model`. Named by module path, never re-exported at the root. No outer `///`: see `venue_wiring`.
pub mod venue_halt_verify;
// What a venue's stored history means: what a stored volume counts (`stored_volume_for`) and whether the venue publishes
// a funding history (`funding_history_for`). Named by module path, never re-exported at the root. No outer `///`: see
// `venue_wiring`.
pub mod venue_history_facts;
pub mod venue_hold;
// What a venue OFFERS on the leverage axis (`leverage_scope_for`: where it keeps the figure, where it publishes the
// maximum, whether a program may set it); what vike SENDS is `LEVERAGE_VENUES`. Named by module path, never
// re-exported at the root. No outer `///`: see `venue_wiring`.
pub mod venue_leverage_scope;
pub mod venue_margin_support;
// The network a venue's price feed dials and whether its instrument list is kept per network (`network_for`). Named by
// module path, never re-exported at the root. No outer `///`: see `venue_wiring`.
pub mod venue_network;
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
    NAMED_FEE_LANE_WARNING, PERP_SUFFIX, SpotPairLanes, SpotPairLanesRow, SpotPairRow,
    SpotPairSpelling, deribit_expiry, fee_lane, is_dated, is_delivery_day, kept_fee_lane,
    split_perp, spot_pair_lanes_for, spot_pair_row,
};
pub use fees::{
    POLYMARKET_PROB_CURVE, POLYMARKET_V2_FEE_CURVE, fee_schedule_for,
    fee_schedule_for_with_pm_curve, option_delivery_fee_for,
};
pub use ibkr_session::{IbkrBackend, IbkrSession, NotAnIbkrBackend, ibkr_session};
pub use lane_twins::{AccountRowLaneTwin, LANE_TWINS, is_lane_twin_label, lane_twin};
pub use leverage_venues::{
    LEVERAGE_VENUES, LeverageMeans, LeverageVenue, leverage_refusal, leverage_venue,
    leverage_venue_at, row_leverage_refusal,
};
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
pub use venue_hold::{POLYMARKET_ITODE_HOLD_MS, POLYMARKET_SPORTS_GAME_HOLD_MS};
pub use venue_margin_support::venue_margin_support;
pub use venue_rate_limits::{
    VenueRateLimits, VenueRateLimitsHistory, VenueRateLimitsMarket, VenueRateLimitsMeter,
    VenueRateLimitsProvenance, rate_limits_for,
};
pub use venue_tier_scope::{TierScope, venue_tier_scope};
