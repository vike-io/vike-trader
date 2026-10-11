//! The source-ratchet gates over this crate's `src/` and DDL: `cargo test -p vike-secrets --test gates`.

#[path = "../support/mod.rs"]
mod support;

mod ddl_parser;
mod null_discriminator;
mod sqlite_sequence;
mod store_link;
