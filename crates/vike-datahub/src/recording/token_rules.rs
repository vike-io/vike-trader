//! **A recorded polymarket token's market RULES, kept as texts while it is recorded** (book plan W38): the
//! question, the rules (Gamma's `description`) and the resolution source of the market of every token the
//! polymarket recorder resolves, as `kind=text` rows of the series `vike_data::text_log::TEXT_KIND_POLYMARKET_RULES`
//! / `POLYMARKET_RULES_RECORDER_COLLECTOR` (the text kind's doc names the columns). Armed by the box's
//! `flags.record_market_rules` row through `crate::recorder::RecorderArgs::market_rules`; off (the default),
//! nothing here runs.
//!
//! * **When**: [`TokenRules::record`] runs on every recorder tick, called by `super::token_facts`' facts
//!   writer with the tokens the tick resolved. A token is READ (one Gamma point lookup through the gatewayhub,
//!   `vike_data::source::MarketRulesSource::of_token`) the first time it is seen and then at most once per UTC
//!   day, so a market's rules are re-read daily for as long as it is recorded; reading one token marks every
//!   token of its market read, so a two-outcome market costs one read. A tick makes at most
//!   [`RULES_READS_PER_TICK`] reads, since it runs inside the recorder's subscription refresh: the rest wait for the
//!   ticks after, so a slow Gamma stalls a refresh by a few read bounds, never by one per market.
//! * **What is stored**: a market whose text this recorder has not stored is stored as it is; a market whose
//!   WORDING (`vike_data::TextRow::revision`: question, resolution source, rules) differs from the one stored is
//!   stored again with `amends` = its condition id: the rules CHANGED, and the row says so. An unchanged
//!   market writes nothing. Each row's received time is the read's (the gatewayhub's clock on this box).
//! * **One append per tick with something new**, under `market-rules-recorded:{now_ms}`: a tick written twice is
//!   one batch. A failed append forgets the reads, so the next tick reads and writes them again.
//! * **The memory is SEEDED from the store** at the first tick (the rows received within [`RULES_SEED_WINDOW`]),
//!   so a restart does not store again what it stored. shortcut: a market first stored more than
//!   [`RULES_SEED_WINDOW`] before a restart is stored once more, unchanged and without `amends`, and a change
//!   read right after such a restart carries no `amends`; a store read by condition id would close it, if one
//!   is ever measured to matter.
//! * A read that fails is said once per token and asked again at the next tick; a token no open market lists
//!   writes nothing (asked again the next day).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_data::text_log::{POLYMARKET_RULES_RECORDER_COLLECTOR, TEXT_KIND_POLYMARKET_RULES};
use vike_data::{HistoryStore, TextRow};
use vike_gatewayhub_client::client::GatewayLink;
use vike_model::MS_PER_DAY;

/// How far back the first tick reads the store to learn what is already stored.
pub const RULES_SEED_WINDOW: Duration = Duration::from_millis(7 * vike_model::MS_PER_DAY_U64);

/// The most market reads one tick makes. The tick runs inside the recorder's subscription refresh and each read
/// may wait up to the resolve bound (30 s), so a wide family's first day (one read per market) or a slow Gamma
/// stalls the refresh by at most this many bounds; the tokens left over are read by the ticks after.
pub const RULES_READS_PER_TICK: usize = 4;

/// One token's market, as rules rows: `Ok(empty)` when no open market lists it.
pub type RulesLookup = Arc<dyn Fn(&str) -> Result<Vec<TextRow>, String> + Send + Sync>;

/// The production lookup: the gatewayhub's market-rules read of one token, given up after the family resolve's
/// own bound (`vike_gatewayhub_client::resolve::RESOLVE_REPLY_BOUND`), since a recorder tick waits for it.
#[must_use]
pub fn gateway_lookup(link: GatewayLink) -> RulesLookup {
    use vike_data::source::MarketRulesSource;
    Arc::new(move |token: &str| {
        let deadline =
            std::time::Instant::now() + vike_gatewayhub_client::resolve::RESOLVE_REPLY_BOUND;
        vike_gatewayhub_client::history::MarketRulesSourceGateway::new(link.clone())
            .with_stop(Arc::new(move || std::time::Instant::now() >= deadline))
            .of_token(token)
            .map_err(|e| e.to_string())
    })
}

/// What the writer knows: the stored wording of each text, and when each token was last read.
#[derive(Default)]
struct Kept {
    seeded: bool,
    /// The seed read failed and was said, so the next failure is not said again.
    seed_said: bool,
    /// text id → the revision stored last.
    stored: HashMap<String, String>,
    /// token → the UTC day its market was last read.
    read_on: HashMap<String, i64>,
    /// Tokens whose read failed and was said, so the next failure is not said again.
    said: HashSet<String>,
}

/// The rules writer. See the module doc.
pub struct TokenRules {
    store: Arc<dyn HistoryStore + Send + Sync>,
    lookup: RulesLookup,
    kept: Mutex<Kept>,
}

impl TokenRules {
    /// A writer into `store`, reading each token's market through `lookup`.
    #[must_use]
    pub fn new(store: Arc<dyn HistoryStore + Send + Sync>, lookup: RulesLookup) -> Self {
        Self { store, lookup, kept: Mutex::default() }
    }

    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One tick at `now_ms` over the tokens the recorder resolved; returns the rows stored. Store failures are
    /// logged: recording never stops on a rules row.
    pub fn record(&self, tokens: &BTreeSet<String>, now_ms: i64) -> usize {
        if !self.seed(now_ms) {
            return 0;
        }
        let today = now_ms.div_euclid(MS_PER_DAY);
        {
            // A token that rotated out is forgotten: the next window's tokens are new ones.
            let mut kept = self.kept();
            kept.read_on.retain(|t, _| tokens.contains(t));
            kept.said.retain(|t| tokens.contains(t));
        }
        let mut fresh: Vec<TextRow> = Vec::new();
        let mut reads = 0;
        for token in tokens {
            if self.kept().read_on.get(token) == Some(&today) {
                continue;
            }
            if reads == RULES_READS_PER_TICK {
                break;
            }
            reads += 1;
            let rows = match (self.lookup)(token) {
                Ok(rows) => rows,
                Err(why) => {
                    if self.kept().said.insert(token.clone()) {
                        tracing::warn!(token, %why, "recorder: a polymarket token's market rules could not be read; asked again next tick");
                    }
                    continue;
                }
            };
            let mut kept = self.kept();
            kept.said.remove(token);
            kept.read_on.insert(token.clone(), today);
            // A row of another kind would land in a series that does not name it: never stored.
            for row in rows.into_iter().filter(|r| r.kind == TEXT_KIND_POLYMARKET_RULES) {
                for sibling in row.subject.split(' ').filter(|t| !t.is_empty()) {
                    kept.read_on.insert(sibling.to_string(), today);
                }
                let id = row.text_id();
                if fresh.iter().any(|f| f.text_id() == id) {
                    continue;
                }
                match kept.stored.get(&id) {
                    Some(revision) if *revision == row.revision() => {}
                    Some(_) => fresh.push(TextRow { amends: row.source_id.clone(), ..row }),
                    None => fresh.push(row),
                }
            }
        }
        if fresh.is_empty() {
            return 0;
        }
        let key = format!("market-rules-recorded:{now_ms}");
        match self.store.append_texts(
            TEXT_KIND_POLYMARKET_RULES,
            POLYMARKET_RULES_RECORDER_COLLECTOR,
            &fresh,
            Some(&key),
        ) {
            Ok(written) => {
                let mut kept = self.kept();
                for row in &fresh {
                    kept.stored.insert(row.text_id(), row.revision());
                }
                written
            }
            Err(e) => {
                tracing::warn!(error = %e, rows = fresh.len(), "recorder: polymarket market rules could not be written; read and written again next tick");
                let mut kept = self.kept();
                for row in &fresh {
                    for token in row.subject.split(' ') {
                        kept.read_on.remove(token);
                    }
                }
                0
            }
        }
    }

    /// Learn what is stored from the store, once: `false` (said) while the store cannot be read.
    fn seed(&self, now_ms: i64) -> bool {
        if self.kept().seeded {
            return true;
        }
        let window_ms = i64::try_from(RULES_SEED_WINDOW.as_millis()).unwrap_or(i64::MAX);
        match self.store.texts_as_of(
            TEXT_KIND_POLYMARKET_RULES,
            POLYMARKET_RULES_RECORDER_COLLECTOR,
            now_ms,
            window_ms,
        ) {
            Ok(stored) => {
                let mut kept = self.kept();
                // Received-ascending: the last row of a text is the wording stored last.
                for row in &stored {
                    kept.stored.insert(row.text_id(), row.revision());
                }
                kept.seeded = true;
                true
            }
            Err(e) => {
                let mut kept = self.kept();
                if !kept.seed_said {
                    kept.seed_said = true;
                    tracing::warn!(error = %e, "recorder: the stored polymarket market rules could not be read; nothing is read or written until they can");
                }
                false
            }
        }
    }
}

#[path = "token_rules_tests.rs"]
#[cfg(test)]
mod token_rules_tests;
