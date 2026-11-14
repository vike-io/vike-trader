//! Universal cross-venue instrument catalog. Bridge-free: depends on `vike-model` only, so the
//! per-venue bridge crates and `vike-chart` can depend on it down-only. Holds a two-mode `CatalogProvider` trait, the tiered fuzzy `Catalog`
//! search the Symbol picker renders from, and the per-venue capability tables (addressing,
//! intervals, session, history channels). The `Instrument` row it stores and searches is
//! `vike_model::Instrument`.
//!
//! ⚠ The `AssetClass` taxonomy itself is `vike_model::AssetClass` and no longer lives here — the
//! hist store has to record whether a series is spot or perp, and `vike-data` is this crate's own
//! layer, which the layer gate refuses. What stayed is the picker's `PickerTab` grouping (see `tab`).

#![warn(unreachable_pub)]

// The picker's own drawers (`tab_for` is a free function: its module doc argues the split).
mod tab;
pub use tab::{PickerTab, tab_for};

#[cfg(test)]
mod test_support;

mod field_map;
pub use field_map::{CatalogFieldMap, parse_with};

mod provider;
pub use provider::{CatalogError, CatalogMode, CatalogProvider, CatalogRoute, CatalogSearchFilter};

// WHICH roster venues can be enumerated, and at whose expense (`docs/decisions/0062`).
mod availability;
pub use availability::{CatalogAvailability, catalog_availability, catalog_source_for};

// A held-session venue's list as a login pushed it (decision 0062's decision 3, amended 2026-11-13).
pub mod held_list;

// WHETHER a process serves the venue-catalog verb, and why (`docs/decisions/0066`).
mod gate;
pub use gate::{CatalogVenueGate, venue_catalog_gate, venue_catalog_gate_line};

// A venue's dated, qualified instrument list as the operator fetched it: a separate source from
// `CatalogCache`, on purpose (`docs/decisions/0066`).
mod instrument_list;
pub use instrument_list::{
    CatalogAnswer, CatalogLocalUpsert, InstrumentList, InstrumentListError, InstrumentLists,
    catalog_answer, merge_sources, upsert_local,
};

// The venues' KEPT lists (`<project>/instruments/<venue>.json`, the datahub's): the file, its one reader, and
// this process's read of them.
mod kept_list;
pub use kept_list::{
    KEPT_LIST_REREAD, VenueInstrumentList, hold_kept_list, kept_file, kept_list, kept_row_of,
    read_kept_list, read_kept_lists, read_session_rows, use_kept_lists,
};

mod catalog;
pub use catalog::{Catalog, merge_ranked};

// What the symbol pickers draw: one line per underlying with a listing per venue.
mod underlying;
pub use underlying::{
    PickerKind, PickerListing, PickerQuery, PickerUnderlying, group_by_underlying, kind_of,
    picker_flat, picker_results, venue_counts,
};

mod persist;
pub use persist::{CatalogCache, CatalogCacheVenueStamp, load_cache, save_cache};

mod session;
pub use session::{
    session_calendar_for, session_class_of, session_venue, trades_every_day, trading_calendar,
};

// Per-venue instrument-ADDRESSING table; FAILS CLOSED (`docs/decisions/0061`, Phase 1).
mod addressing;
pub use addressing::{
    VenueAddressing, VenueAddressingBareSymbol, VenueAddressingNaming, addressing_for,
};

// Per-venue bar-INTERVAL table, keyed like addressing (`docs/decisions/0061`, Phase 4).
mod intervals;
pub use intervals::{
    PROBED_AXIS, VenueIntervals, VenueIntervalsEvidence, VenueIntervalsVerdict, intervals_for,
    venue_interval_word,
};

// Per-venue HISTORY-CHANNELS table and its rendering (the page, the JSON, the sentences).
mod history;
pub use history::history_reference;
pub use history::{
    HistoryChannel, HistoryChannelAccess, HistoryChannelClass, HistoryChannelDepth,
    HistoryChannelEvidence, HistoryChannelEvidenceSource, HistoryChannelKind, HistoryChannelLane,
    HistoryChannelPace, HistoryChannelPerRequest, HistoryChannelState, HistoryChannelStepLookback,
    history_channels_for,
};

// IG's history vocabulary — the epic grammar, the intervals, their depths and PINNED chunk widths —
// for the data daemon (no bridge) and the IG bridge, which must agree on all of it.
mod ig_history;
pub use ig_history::{
    IG_ADMITTED_INTERVALS, IG_HISTORY_STEPS, IG_WEEKLY_POINTS, IG_WINDOW_SHARE_PERCENT,
    IgHistoryStep, RULE_MAX_CHUNK_DAYS, TARGET_ROWS_A_CHUNK, ig_history_step, ig_interval_admitted,
    ig_unmeasured_words, ig_window_points, is_ig_epic,
};

// The polymarket recorder-FAMILY grammar check: a pure refusal a bridge-free process can run offline.
mod family;
pub use family::polymarket_family_minutes;

// Which (venue, symbol) names a coin-margined contract, what one unit of it is worth, and what an option
// is written on, for a process that holds no row of it.
mod inverse;
pub use inverse::{
    BINANCE_COIN_M_FACES, OKX_INVERSE_FACES, contract_value, inverse_face_value,
    inverse_settle_coin, is_inverse_contract, kept_contract_value, option_underlyings,
};

// How each venue's ENGINE spells a symbol: the rule table, the one grammar the control door and the CLI share.
mod venue_symbol;
pub use venue_symbol::{
    SYMBOL_RULES, VenueSymbolRule, VenueSymbolSpelling, bare_pair, bybit_kept_row,
    canonical_symbol, deribit_expiry, door_symbol, is_okx_delivery_date, is_outcome_token, is_pair,
    kept_row, respelled_symbol, venue_engine_symbol,
};

// CORE-symbol <-> EXCHANGE-symbol conversion over the addressing table. The `.P` split itself and
// the fee lane it keys are `vike-venues`' (`vike_venues::split_perp`, `vike_venues::fee_lane`).
mod symbol;
pub use symbol::{
    LEDGER_CURRENCY, engine_lane_holds_stop, fiat_quote_of, foreign_fiat_quoted_list,
    kept_fiat_quote, names_perp, names_spot_pair, off_ledger_list, off_ledger_list_of,
    six_letter_pair, split_perp_at, to_core_symbol, to_exchange_symbol, to_oanda_instrument,
    uses_perp_suffix, writes_a_pair_with_a_slash,
};
