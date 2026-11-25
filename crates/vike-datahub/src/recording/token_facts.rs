//! **A recorded polymarket token's FACTS, written into the store beside its recorded rows** (PM-2, the owner's
//! choice A of 2026-11-16): its price step, minimum size, share step, venue hold and class, as one `kind=properties`
//! row of the token (`vike_data::HistoryStore::append_symbol_properties`, the store's existing per-symbol facts
//! shape), so a backtest or tick replay over the recorded rows reads them back (`properties_as_of`) and judges an
//! order as the venue would. `<project>/instruments/polymarket.json`, the kept list, does not grow per token.
//!
//! * **When**: at record time. [`RecordsTokenFacts`] wraps the polymarket recorder feed's resolver, and the first
//!   time a tick resolves a token it reads the token's facts row through the gatewayhub (the venue's catalog search
//!   by token id: Gamma, then the CLOB's hold) and writes it, stamped at that tick, before the token's first
//!   recorded row. A read that fails is said once per token and asked again at the next tick; nothing is written
//!   for a token the venue lists no row of. One row per token per UTC day (the commit key, namespaced `token-facts:`
//!   and declared on the `properties` kind's row of `vike_data::store::store_kind`), so a restart rewrites nothing.
//! * **A `tick_size_change`**: the feed re-grids the token's book and every recorded book update from then on
//!   carries the new tick (`vike_polymarket`'s feed, the pmxt mapper's rule). [`TickWatch`], the recorder's sink,
//!   notes a book update whose tick differs from the facts written, and the next tick writes a facts row with the
//!   new tick stamped at the change, so the recorded step follows the venue from that moment on.
//! * **Rows recorded before this have no facts**: nothing is written for them, and a run over them keeps its
//!   warnings.
//! * **Each family's live tokens are left for the server** ([`TokenFacts::publishing`]): on every tick the family's
//!   resolved tokens whose row was read, each with its outcome and its window's end as the row states them, replace
//!   the family's entry in `crate::live_family_tokens::LiveFamilyTokensHeld` wholesale, which
//!   `Request::LiveFamilyTokens` answers from. No read is made for it: the row is the one the facts came from.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use vike_data::live::SymbolResolver;
use vike_data::{DataClientSink, HistoryStore};
use vike_datahub_client::family::LiveFamilyToken;
use vike_gatewayhub_client::client::GatewayLink;
use vike_marketdata::{Bar, FundingRate, L2Book, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{Contract, Instrument, StreamDelay, StreamStatus, SymbolProperties};

use crate::live_family_tokens::LiveFamilyTokensHeld;

/// The venue whose recorded tokens carry their facts.
const VENUE: &str = "polymarket";

/// One token's facts rows, read from the venue: `Ok(empty)` when it lists none.
pub type FactsLookup = Arc<dyn Fn(&str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// The production lookup: the gatewayhub's keyless catalog search by token id, given up after the family
/// resolve's own bound (`vike_gatewayhub_client::resolve::RESOLVE_REPLY_BOUND`), since a recorder tick waits for it.
#[must_use]
pub fn gateway_lookup(link: GatewayLink) -> FactsLookup {
    let lookup = crate::server::keyless_lookup(link);
    Arc::new(move |token: &str| lookup(VENUE, token))
}

/// The facts the recorder has written, and the tick changes its sink saw since the last tick.
#[derive(Default)]
struct Kept {
    /// token → the facts last written for it.
    written: HashMap<String, SymbolProperties>,
    /// `(token, ts, tick)`: a recorded book update whose tick differs from the facts written.
    changes: Vec<(String, i64, f64)>,
    /// Tokens whose read failed and was said, so the next failure is not said again.
    said: HashSet<String>,
    /// token → its outcome and its window's end, off the row read for its facts: what [`TokenFacts::publish`]
    /// leaves for the server. Kept while a family holds the token.
    outcomes: HashMap<String, (String, Option<i64>)>,
}

/// The facts writer shared by [`RecordsTokenFacts`] and [`TickWatch`].
pub struct TokenFacts {
    store: Arc<dyn HistoryStore + Send + Sync>,
    lookup: FactsLookup,
    kept: Mutex<Kept>,
    /// The market-rules writer the same tick feeds (`super::token_rules`), when the box records texts.
    rules: Option<super::token_rules::TokenRules>,
    /// Where each family's live tokens are left for `Request::LiveFamilyTokens`.
    live: LiveFamilyTokensHeld,
}

impl TokenFacts {
    /// A writer into `store`, reading each token's facts through `lookup`.
    #[must_use]
    pub fn new(store: Arc<dyn HistoryStore + Send + Sync>, lookup: FactsLookup) -> Arc<Self> {
        Self::publishing(store, lookup, None, LiveFamilyTokensHeld::default())
    }

    /// [`TokenFacts::new`], with the market-rules writer each tick also feeds (`None`: no rules are kept), leaving
    /// each family's live tokens in `live` too (the module doc).
    #[must_use]
    pub fn publishing(
        store: Arc<dyn HistoryStore + Send + Sync>,
        lookup: FactsLookup,
        rules: Option<super::token_rules::TokenRules>,
        live: LiveFamilyTokensHeld,
    ) -> Arc<Self> {
        Arc::new(Self { store, lookup, kept: Mutex::default(), rules, live })
    }

    /// Leave `family`'s live tokens for the server: each of `tokens` whose row was read, with its outcome and end.
    /// A token no family holds any more is forgotten.
    pub fn publish(&self, family: &str, tokens: &BTreeSet<String>) {
        let live: Vec<LiveFamilyToken> = {
            let kept = self.kept();
            tokens
                .iter()
                .filter_map(|token| {
                    kept.outcomes.get(token).map(|(outcome, end_ms)| LiveFamilyToken {
                        family: family.to_string(),
                        token: token.clone(),
                        outcome: outcome.clone(),
                        end_ms: *end_ms,
                    })
                })
                .collect()
        };
        self.live.set_family(family, live);
        self.kept().outcomes.retain(|token, _| self.live.holds(token));
    }

    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Write the facts of every token of `tokens` none were written for yet (stamped `now_ms`), then the tick
    /// changes [`TickWatch`] noted. Store failures are logged: recording never stops on a facts row.
    pub fn record(&self, tokens: &BTreeSet<String>, now_ms: i64) {
        {
            // A token that rotated out is forgotten: the next window's tokens are new ones.
            let mut kept = self.kept();
            kept.written.retain(|t, _| tokens.contains(t));
            kept.said.retain(|t| tokens.contains(t));
        }
        let new: Vec<String> =
            tokens.iter().filter(|t| !self.kept().written.contains_key(*t)).cloned().collect();
        for token in new {
            let Some(facts) = self.read(&token) else { continue };
            let date = vike_model::time::epoch_ms_to_utc_date(now_ms);
            if self.write(&token, now_ms, facts, &format!("token-facts:{VENUE}:{token}:{date}")) {
                let mut kept = self.kept();
                kept.said.remove(&token);
                kept.written.insert(token, facts);
            }
        }
        let changes = std::mem::take(&mut self.kept().changes);
        for (token, ts, tick) in changes {
            let Some(mut facts) = self.kept().written.get(&token).copied() else { continue };
            if facts.tick_size == tick {
                continue;
            }
            facts.tick_size = tick;
            if self.write(&token, ts, facts, &format!("token-facts:{VENUE}:{token}:tick:{ts}")) {
                tracing::info!(
                    token = %token,
                    tick,
                    ts,
                    "recorder: a polymarket token's tick changed; its facts follow"
                );
                self.kept().written.insert(token, facts);
            }
        }
        if let Some(rules) = &self.rules {
            rules.record(tokens, now_ms);
        }
    }

    /// The token's facts row as the venue states it, its class set, or `None` (said once per token).
    fn read(&self, token: &str) -> Option<SymbolProperties> {
        let row = (self.lookup)(token).and_then(|rows| {
            rows.into_iter().find(|r| r.raw_symbol == token).ok_or_else(|| {
                "the venue lists no open market with this token, so no facts were written"
                    .to_string()
            })
        });
        match row {
            Ok(row) => {
                if let Contract::PredictionOutcome(outcome) = &row.contract {
                    let held = (outcome.outcome.clone(), outcome.resolution_ms);
                    self.kept().outcomes.insert(token.to_string(), held);
                }
                Some(SymbolProperties { asset_class: Some(row.asset_class()), ..row.properties })
            }
            Err(why) => {
                if self.kept().said.insert(token.to_string()) {
                    tracing::warn!(token, %why, "recorder: a polymarket token's facts could not be read; asked again next tick");
                }
                None
            }
        }
    }

    fn write(&self, token: &str, ts: i64, facts: SymbolProperties, key: &str) -> bool {
        match self.store.append_symbol_properties(VENUE, token, &[(ts, facts)], Some(key)) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(token, error = %e, "recorder: a polymarket token's facts could not be written; written again next tick");
                false
            }
        }
    }

    /// Note a recorded book update of `token` whose tick differs from the facts written for it.
    pub fn observe_tick(&self, token: &str, ts: i64, tick: f64) {
        let mut kept = self.kept();
        let differs = kept.written.get(token).is_some_and(|f| f.tick_size != tick)
            && !kept.changes.iter().any(|(t, _, k)| t == token && *k == tick);
        if differs && tick.is_finite() && tick > 0.0 {
            kept.changes.push((token.to_string(), ts, tick));
        }
    }
}

/// The polymarket recorder feed's resolver, writing the facts of each token it resolves ([`TokenFacts::record`]).
pub struct RecordsTokenFacts {
    inner: Box<dyn SymbolResolver>,
    facts: Arc<TokenFacts>,
}

impl RecordsTokenFacts {
    #[must_use]
    pub fn new(inner: Box<dyn SymbolResolver>, facts: Arc<TokenFacts>) -> Self {
        Self { inner, facts }
    }
}

impl SymbolResolver for RecordsTokenFacts {
    fn group(&self) -> Option<&str> {
        self.inner.group()
    }

    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        let tokens = self.inner.desired(now_ms)?;
        self.facts.record(&tokens, now_ms);
        if let Some(family) = self.inner.group() {
            self.facts.publish(family, &tokens);
        }
        Ok(tokens)
    }
}

/// The recorder's sink, passing every verb to `inner` and noting a polymarket book update's tick
/// ([`TokenFacts::observe_tick`]): a lock and a compare on the feed's thread, never a write.
pub struct TickWatch {
    inner: Arc<dyn DataClientSink>,
    facts: Arc<TokenFacts>,
}

impl TickWatch {
    #[must_use]
    pub fn new(inner: Arc<dyn DataClientSink>, facts: Arc<TokenFacts>) -> Self {
        Self { inner, facts }
    }
}

impl DataClientSink for TickWatch {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.inner.seed_bars(venue, symbol, interval, bars);
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.forming_bar(venue, symbol, interval, bar);
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.mark_tick(venue, symbol, px, ts);
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
    }
    vike_data::forward_l2_snapshot!(inner);
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        self.inner.quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        self.inner.trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.inner.book(venue, symbol, book);
    }
    fn book_update(&self, venue: &str, symbol: &str, update: L2BookUpdate) {
        if venue == VENUE {
            self.facts.observe_tick(symbol, update.timestamp, update.tick_size);
        }
        self.inner.book_update(venue, symbol, update);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.inner.stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        self.inner.data_delay(venue, symbol, stream, delay);
    }
    fn funding(&self, venue: &str, symbol: &str, funding: FundingRate) {
        self.inner.funding(venue, symbol, funding);
    }
}

#[path = "token_facts_tests.rs"]
#[cfg(test)]
mod token_facts_tests;
