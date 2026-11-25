//! **A KIND (a) adapter's face** (decision 0296's step 6; `crates/vike-rust-strategy/tests/sdk_adapter.rs`): the
//! face of a synthetic older SDK whose `vike_model::OrderBroker` still spells `history_only_grows` as
//! `inputs_only_grow` (the 12 -> 13 rename, undone). The host's own face with ONE line changed, the method
//! listed as `history_only_grows as inputs_only_grow`: the same body under the SDK's name, so it costs what
//! the host's does.

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
        history_only_grows as inputs_only_grow,
        closed_trades,
    );
}

