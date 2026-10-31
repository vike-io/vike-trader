//! `AssembledFeed` — the ONE `VenueFeed` implementation: a venue label, a resolver, a client.
//! Every venue-specific decision is made by whoever CONSTRUCTS it (the data daemon's recording
//! table, `crates/vike-datahub/src/recording.rs`'s `build_recording_feed`), never here.
use std::collections::BTreeSet;

use vike_data::{DataClient, live::SymbolResolver};

use crate::runtime::VenueFeed;
use crate::session::Stream;

pub struct AssembledFeed {
    venue: String,
    /// The venue's book pump already emits derived L1 quotes (polymarket's `PumpMode::Book`, per
    /// `crates/bridges/polymarket/src/market_feed.rs`'s module doc), so subscribing `Quotes` beside
    /// `Book` would open a second socket and write every quote row twice.
    book_carries_quotes: bool,
    resolver: Box<dyn SymbolResolver>,
    client: Box<dyn DataClient + Send>,
    /// The venue serves QUOTES and nothing else, so the feed asks for nothing else (see [`Self::quotes_only`]).
    quotes_only: bool,
}

impl AssembledFeed {
    pub fn new(
        venue: impl Into<String>,
        book_carries_quotes: bool,
        resolver: Box<dyn SymbolResolver>,
        client: Box<dyn DataClient + Send>,
    ) -> Self {
        Self { venue: venue.into(), book_carries_quotes, resolver, client, quotes_only: false }
    }

    /// This feed subscribes `Stream::Quotes` and no other stream, whatever the profile requests.
    ///
    /// For a venue that declares quotes alone. The recorder probes every requested stream of every key it
    /// holds and learns an `Unsupported` once per stream; a broker that checks a stream ceiling BEFORE the
    /// bridge's capability refusal answers those probes with the ceiling's refusal instead once it is full,
    /// and the recorder then retries and logs three refusals per held key on every tick, for ever.
    #[must_use]
    pub fn quotes_only(mut self) -> Self {
        self.quotes_only = true;
        self
    }
}

impl VenueFeed for AssembledFeed {
    fn venue(&self) -> &str {
        &self.venue
    }
    fn family(&self) -> Option<&str> {
        self.resolver.group()
    }
    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        self.resolver.desired(now_ms)
    }
    fn client(&mut self) -> &mut dyn DataClient {
        self.client.as_mut()
    }
    /// Drop `Quotes` whenever `Book` is also requested AND this venue's book carries quotes; keep
    /// it otherwise — without `Book`, the quote-only pump is exactly what was asked for.
    fn narrow(&self, requested: &[Stream]) -> Vec<Stream> {
        if self.quotes_only {
            return requested.iter().copied().filter(|s| *s == Stream::Quotes).collect();
        }
        if !self.book_carries_quotes {
            return requested.to_vec();
        }
        let has_book = requested.contains(&Stream::Book);
        requested.iter().copied().filter(|s| !(has_book && *s == Stream::Quotes)).collect()
    }
}

#[path = "assembled_tests.rs"]
#[cfg(test)]
mod assembled_tests;
