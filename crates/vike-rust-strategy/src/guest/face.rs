//! **The face of THIS crate's SDK** (`crate::guest`'s `face`, decision 0296's step 6): the
//! `vike_model::OrderBroker` impl a strategy file built against the SDK this host was cut with sees, every
//! method the boundary's own body (`crate::guest::broker_methods!`).
//!
//! ⚠ **The template of every adapter.** An older SDK whose boundary moved builds against this host's
//! glue through its adapter's `face.rs` (`crate::sdk::SdkAdapter`), which stands where this file stands:
//! the same list, with that SDK's differences only. An SDK whose API this host's glue still compiles
//! against is adapted by a COPY of this file; a renamed or re-signed method is its line in the list as
//! `history_only_grows as inputs_only_grow`, the same body under the old name (it costs nothing), or a
//! method written out by hand. ⚠ A lent row laid out otherwise is NOT a difference a face carries: decision
//! 0296's verdict 7 RETIRES such an SDK (the owner's "Б" of 2026-11-20), `crate::sdk::usable` refuses it
//! whatever adapter exists, and its pinned files move with `vike-cli strategy migrate`.

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
