//! The Dukascopy `.bi5` tick-archive FORMAT: what a `.bi5` file is and how to read one, with no
//! network, no store, no filesystem walk and no venue. It was the format half of
//! `crates/bridges/dukascopy` until the gatewayhub change set the datahub's goal of linking no venue
//! bridge (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`, verdict
//! 8); the bridge keeps the FETCH and imports the format from here.
//!
//! - `tick.rs` is the record and the HOURLY file: [`Tick`], [`decode_ticks`], [`decompress`] and
//!   [`tick_to_quote`], the layout the HTTP datafeed serves (one UTC hour per file).
//! - `archive.rs` is the archive import lane's format half — the DAILY `.bi5` files a user synced
//!   from the vendor's bucket: their path grammar, bounded decompression and a decoder that refuses
//!   rather than guesses ([`decode_daily_file`], [`Bi5Refusal`]). Its module doc is the contract.
//! - `price_scale.rs` is the per-instrument point values the archive lane admits
//!   ([`bi5_price_scale`], [`unscaled_instrument_refusal`]).
//!
//! # Where it sits, and why it is a crate
//!
//! A leaf (`layer = 15`): it names `vike-model` (the day length), `vike-marketdata` (the
//! [`QuoteTick`](vike_marketdata::QuoteTick) a tick maps to) and the external LZMA decoder the
//! bridge already used, and nothing else. It is its own crate because two consumers that must not
//! share a dependency tree both need the decoder: the bridge, whose CDN fetch decodes the hours it
//! downloads, and the datahub's archive-import lane (`crates/vike-datahub/src/import/formats.rs`),
//! which decodes the files already on its disk and so needs no bridge, no `vike-bridge-core` and no
//! HTTP client behind it.
//!
//! Every public item is named at this crate's root (`vike_bi5::decode_daily_file`); the children
//! are private.

mod archive;
mod price_scale;
mod tick;

pub use archive::decode::{decode_daily_file, decompress_bounded};
pub use archive::limits::{Bi5Header, Bi5Limits, read_bi5_header};
pub use archive::path::{Bi5Day, Bi5Path, classify_bi5_path, is_bi5_layout_dir};
pub use archive::refusal::Bi5Refusal;
pub use price_scale::{
    Bi5PriceScale, bi5_price_scale, bi5_price_scales, unscaled_instrument_refusal,
};
pub use tick::{HOUR_MS, REC_LEN, Tick, decode_ticks, decompress, tick_to_quote};
