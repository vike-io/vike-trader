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
    CatalogAnswer, CatalogLocalUpsert, InstrumentList, InstrumentListError, InstrumentListErrorId,
    InstrumentLists, catalog_answer, merge_sources, upsert_local,
};

// The refusals this crate words for a person, each with its id (`vike_utility::refusal_enum!`).
pub mod refusals;

// The venues' KEPT lists (`<project>/instruments/<venue>.json`, the datahub's): the file, its one reader, and
// this process's read of them.
mod kept_list;
pub use kept_list::{
    KEPT_LIST_REREAD, VenueInstrumentList, hold_kept_list, kept_file, kept_list, kept_list_now,
    kept_raw_of_user_name, kept_row_of, kept_user_name_of_raw, read_kept_list, read_kept_lists,
    read_session_rows, use_kept_lists,
};
// Test support: the recorded kept lists a test that holds no datahub hands a face reader.
#[cfg(any(test, feature = "test-support"))]
mod recorded_kept_lists;
#[cfg(any(test, feature = "test-support"))]
pub use recorded_kept_lists::{hold_recorded_kept_lists, recorded_kept_lists};

mod catalog;
pub use catalog::{Catalog, merge_ranked};

// What the symbol pickers draw: one line per underlying with a listing per venue.
mod underlying;
pub use underlying::{
    SymbolPickerKind, SymbolPickerQuery, SymbolPickerUnderlying, SymbolPickerUnderlyingListing,
    group_by_underlying, kind_of, picker_flat, picker_results, same_instrument_per_venue,
    venue_counts,
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

// Which (venue, symbol) names a coin-margined contract, what one unit of it is worth (off the name, or its kept
// row where only the row states it), and what an option is written on.
mod inverse;
pub use inverse::{
    contract_value, inverse_settle_coin, is_coin_settled_option, is_inverse_contract,
    kept_contract_value, kept_is_inverse_contract, option_underlyings,
};

// An okx contract's grid in BASE units: the one contracts-to-base conversion the bridge and the properties tape share.
mod okx_grid;
pub use okx_grid::okx_grid_in_base;

// The door's reading of a typed symbol (its shape, then the user name its kept row answers, decision 0290) and the
// rule table of what each venue's adapter does with an order's symbol: one reading the control door and the CLI share.
mod venue_symbol;
pub use venue_symbol::{
    SYMBOL_RULES, VenueSymbolRule, VenueSymbolSpelling, bi5_dataset_name, bi5_dataset_row,
    door_symbol, door_user_symbol, is_outcome_token, kept_row, no_kept_list_refusal,
    respelled_symbol, venue_engine_symbol,
};

// Decision 0290's ONE translator: an instrument's user, store and wire names, per venue and class, spelled ONCE by
// the kept list's writer and read off the KEPT row by everyone else.
mod instrument_names;
pub use instrument_names::{
    CFD_SUFFIX, DEX_SEPARATOR, kept_row_of_store_name, kept_row_of_user_name, names_of_row,
    names_of_store_name, names_of_user_name, predates_symbol_names, spell_naming_of_row, spelled,
    user_name_of_row, wire_name_of_row,
};

// CORE-symbol <-> EXCHANGE-symbol conversion over the addressing table. The `.P` split itself and
// the fee lane it keys are `vike-venues`' (`vike_venues::split_perp`, `vike_venues::fee_lane`).
mod symbol;
pub use symbol::{
    books_cash, engine_lane_holds_stop, fee_in_the_fills_money, fiat_quote_of,
    foreign_fiat_quoted_list, kept_fiat_quote, names_spot_pair, off_ledger_list,
    off_ledger_list_of, on_futures_lane, six_letter_pair, split_perp_at, to_oanda_instrument,
    uses_perp_suffix,
};
