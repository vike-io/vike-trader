//! **The face of SDK 3 on this host** (decision 0296's step 6; `crates/vike-rust-strategy/adapters/sdk-3/ADAPTER`):
//! this boundary's guest glue as a file pinned to SDK 3 sees it. SDK 3's API is still this host's (it was cut
//! at `ce00ae41af`, and the boundary moved since only inside `vike-rust-strategy`: the host counts a strategy's
//! failed hooks itself, `Strategy::failures`), so this is a COPY of the host's own face,
//! `crates/vike-rust-strategy/src/guest/face.rs`: every method by its own name.
//!
//! ⚠ When a later PR renames or re-signs a method SDK 3 still spells the old way, it changes THIS file in the
//! same PR: `name as old_name` in the list, or a hand-written method. A lent row laid out otherwise retires
//! SDK 3 instead (decision 0296's verdict 7). Compiled only into SDK 3's builds, never into this crate; CI
//! compiles it against the `sdk-3` tag when the boundary or an adapter changes.

use crate::guest::HostBroker;

impl vike_model::OrderBroker for HostBroker {
    crate::guest::broker_methods!(
        place,
        place_exit_leg,
        place_at_close,
        place_close_from_fill,
        rests_reduce_only_while_flat,
        place_group,
        amend,
        cancel,
        deny,
        publish_forecast,
        average_price,
        symbols,
        position,
        price,
        equity,
        index,
        now,
        bars,
        quote_vwap,
        depth_within_price,
        contract_value,
        instrument_grid,
        input_bars,
        input_whole,
        position_at,
        price_at,
        average_price_at,
        bars_at,
        instrument_grid_at,
        contract_value_at,
        input_names,
        input_bars_at,
        input_whole_at,
        history_only_grows,
        closed_trades,
    );
}
