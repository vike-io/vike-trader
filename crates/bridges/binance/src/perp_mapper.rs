//! Binance's USDⓈ-M futures ORDER_TRADE_UPDATE → vike event mapper: the venue face of the shared
//! [`crate::family::perp_mapper`].
//!
//! Aster's perp user-data stream is Binance-verbatim, so the mapping logic itself lives once in
//! `family` and both venues re-export it under their own names (F0, dedup rung 1). The contract,
//! the golden fixtures (`tests/offline/r6_binance_perp_parity.rs`), and the public path here are
//! unchanged — see the family module for the liquidation/funding/dual-publish notes.
//!
//! [`map_binance_perp_opts`] is the flag-aware twin the Binance perp pump uses to enable the opt-in
//! TRADE_LITE early fast-fill hint (`VIKE_BINANCE_TRADE_LITE_FILL=1`); the 3-arg
//! [`map_binance_perp`] hard-wires the hint OFF (byte-identical to before).

pub use crate::family::perp_mapper::{
    map_perp as map_binance_perp, map_perp_opts as map_binance_perp_opts,
};
