//! The scripted REST lane: a `RestLane` that answers every verb from a script a test wrote, and records
//! every request it received.
//!
//! [`RestLaneScript`] is the venue half of a gatewayhub with no venue behind it. A test says what each
//! lane answers (`with_klines`, `with_funding`, `with_ticks`, the catalog, the option index, the
//! options provider, a rolling family, a credential presence word) and afterwards reads back what the
//! lane was ASKED (`fetches`, `calls`), so a datahub test can assert exactly what it requested over the
//! hop: the venue, the symbol and the window, and whether the credentialed lane was the one used.
//!
//! # What a script answers
//!
//! * **A fetch answer is an [`Answer`]**: rows, a `SourceError::Fetch` (the venue was asked and
//!   failed) or a `SourceError::Refused` (the request cannot be expressed), optionally SLOW. A slow
//!   answer waits in short slices and gives up the moment the fetch is cancelled
//!   (`FetchContext::is_cancelled`), as a real lane does at a page boundary; [`RestLaneScript::cancels_seen`]
//!   counts those, so a test can prove the cancel reached the lane.
//! * **The rows are exactly the rows configured, whatever the window.** A real source filters to
//!   `[start_ms, end_ms]`; this one does not, so the answer is predictable and the WINDOW is
//!   asserted through the recorder instead. Likewise an options chain ignores `strike_window`.
//! * **A verb with no script fails in words naming it** (`scripted rest lane: no script for ...`),
//!   never with an empty answer: an empty `Vec` is "the venue had no rows", which the lane contract
//!   forbids a failure to look like.
//! * **`on_fetch` is the escape hatch**: a closure that answers EVERY fetch itself, ahead of the
//!   per-venue scripts, for the case a table cannot say (fail once, then succeed; park until
//!   cancelled; read the connection scope).
//!
//! # The advertisement
//!
//! `facts()` is DERIVED from the scripts, the way the real lane reads its facts off what it dispatches:
//! scripting klines for `binance` advertises `binance` on the klines lane, a describe script
//! advertises its venue on the catalog lane, and so on in the order scripted. The server reads the
//! facts ONCE when it starts, so script BEFORE `LoopbackGateway::start`. `with_facts` replaces the
//! derivation (to advertise a lane nothing answers, or to advertise less than is scripted).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_catalog::CatalogMode;
use vike_data::option_trade_log::OptionTradeRow;
use vike_data::source::SourceError;
use vike_gatewayhub_client::proto::{
    BudgetCounter, FetchRows, FetchWhat, OptionIndexKind, OptionsCall, OptionsFeedSpec,
    OptionsListing, OptionsReply,
};
use vike_marketdata::FundingRatePoint;
use vike_marketdata::{Bar, OptionChain, OptionChainExpiry, TickQuote};
use vike_model::CredentialPresence;
use vike_model::{AssetClass, Instrument};
use vike_utility::mutex::lock;

use crate::rest::{
    CatalogTraits, FeedOptionsHandle, FeedOptionsSink, FetchContext, RestLane, RestRosters,
};
use crate::session::ConnectionMemos;

/// How often a slow answer looks at the cancel flag.
const SLOW_SLICE: Duration = Duration::from_millis(5);

/// The failure text of a slow answer that saw its fetch cancelled. The server answers `FetchCancelled`
/// whatever a lane returns once the context says cancelled, so this is for a log line and a test.
const CANCELLED_TEXT: &str = "scripted rest lane: the fetch was cancelled";

/// What a scripted fetch answers: rows or a `SourceError`, after an optional delay.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    outcome: Result<T, SourceError>,
    delay: Duration,
}

impl<T> Answer<T> {
    /// These rows.
    pub fn rows(rows: T) -> Self {
        Self { outcome: Ok(rows), delay: Duration::ZERO }
    }

    /// `SourceError::Fetch(text)`: the venue was asked and failed (free text, or a refusal with its id).
    pub fn fetch_error(text: impl Into<vike_utility::errors::CarriedRefusal>) -> Self {
        Self::error(SourceError::Fetch(text.into()))
    }

    /// `SourceError::Refused(text)`: the request cannot be expressed and the venue was not asked.
    pub fn refused(text: impl Into<vike_utility::errors::CarriedRefusal>) -> Self {
        Self::error(SourceError::Refused(text.into()))
    }

    /// This failure.
    pub fn error(error: SourceError) -> Self {
        Self { outcome: Err(error), delay: Duration::ZERO }
    }

    /// The same answer, given only after `delay`, and abandoned (a fetch failure naming the cancel) the
    /// moment the fetch is cancelled meanwhile.
    #[must_use]
    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn map<U>(self, f: impl FnOnce(T) -> U) -> Answer<U> {
        Answer { outcome: self.outcome.map(f), delay: self.delay }
    }
}

impl Answer<FetchRows> {
    /// Wait out the delay (watching the cancel flag), then hand out the outcome.
    fn resolve(&self, ctx: &FetchContext, cancels: &AtomicUsize) -> Result<FetchRows, SourceError> {
        let until = Instant::now() + self.delay;
        loop {
            if ctx.is_cancelled() && !self.delay.is_zero() {
                cancels.fetch_add(1, Ordering::SeqCst);
                return Err(SourceError::Fetch(CANCELLED_TEXT.into()));
            }
            let now = Instant::now();
            if now >= until {
                break;
            }
            std::thread::sleep(SLOW_SLICE.min(until - now));
        }
        self.outcome.clone()
    }
}

/// The closure `on_fetch` takes: the connection scope, the request, whether it was the credentialed
/// verb, and the cancel context.
pub type FetchFn = dyn Fn(&ConnectionMemos, &FetchWhat, bool, &FetchContext) -> Result<FetchRows, SourceError>
    + Send
    + Sync;

/// One fetch request as the lane received it.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchCall {
    /// What was asked: the lane, the venue, the symbol and the window.
    pub what: FetchWhat,
    /// Whether it came as `FetchCredentialed` (the lane that spends the daemon's credential).
    pub credentialed: bool,
}

/// One request the lane received, in arrival order.
#[derive(Debug, Clone, PartialEq)]
pub enum RestCall {
    /// A fetch (every lane of the paged family).
    Fetch(FetchCall),
    /// `CatalogDescribe` for this venue.
    CatalogDescribe {
        /// The venue.
        venue: String,
    },
    /// One options-provider call.
    Options(OptionsCall),
    /// An options feed was opened.
    OpenFeed(OptionsFeedSpec),
    /// A rolling-family resolve.
    ResolveFamily {
        /// The venue.
        venue: String,
        /// The family name.
        family: String,
        /// The instant the caller asked the symbols for.
        now_ms: i64,
    },
    /// A credential presence question for this venue.
    Presence {
        /// The venue.
        venue: String,
    },
}

/// An options feed a test can push rows into: the sink the server handed over, the spec it was opened
/// with, and whether the server shut it down.
pub struct FeedOptionsScript {
    /// Push rows through this.
    pub sink: Arc<dyn FeedOptionsSink>,
    /// The spec the feed was opened with.
    pub spec: OptionsFeedSpec,
    shut_down: Arc<AtomicBool>,
}

impl FeedOptionsScript {
    /// Whether the server shut this feed down (its socket closed).
    pub fn is_shut_down(&self) -> bool {
        self.shut_down.load(Ordering::SeqCst)
    }

    /// Block (up to five seconds) until the server has shut this feed down. Panics otherwise: a feed
    /// left running for a socket that closed would spend a venue connection for nobody.
    pub fn wait_shut_down(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.is_shut_down() {
            assert!(Instant::now() < deadline, "a closed feed socket left its lane running");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct ScriptedFeedHandle(Arc<AtomicBool>);

impl FeedOptionsHandle for ScriptedFeedHandle {
    fn shutdown(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// How a rolling family answers.
#[derive(Clone)]
enum FamilyScript {
    Fixed(Result<Vec<String>, String>),
    By(Arc<dyn Fn(i64) -> Result<Vec<String>, String> + Send + Sync>),
}

/// An insertion-ordered keyed list: re-scripting a key replaces it in place, so the advertisement keeps
/// the order the venues were first scripted in.
fn put<K: PartialEq, V>(table: &mut Vec<(K, V)>, key: K, value: V) {
    match table.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => table.push((key, value)),
    }
}

fn find<K, V>(table: &[(K, V)], hit: impl Fn(&K) -> bool) -> Option<&V> {
    table.iter().find(|(k, _)| hit(k)).map(|(_, v)| v)
}

fn keys<V>(table: &[(String, V)]) -> Vec<String> {
    table.iter().map(|(k, _)| k.clone()).collect()
}

#[derive(Default)]
struct Scripts {
    facts: Option<RestRosters>,
    options_served: bool,
    klines: Vec<(String, Answer<FetchRows>)>,
    credentialed: Vec<(String, Answer<FetchRows>)>,
    funding: Vec<(String, Answer<FetchRows>)>,
    ticks: Vec<(String, Answer<FetchRows>)>,
    catalog_list: Vec<(String, Answer<FetchRows>)>,
    catalog_search: Vec<(String, Answer<FetchRows>)>,
    option_index: Vec<(OptionIndexKind, Answer<FetchRows>)>,
    /// The option-trades fetch, keyed by the currency it names (the lane names no venue).
    option_trades: Vec<(String, Answer<FetchRows>)>,
    /// Both market-rules lanes of a venue, keyed by the venue.
    market_rules: Vec<(String, Answer<FetchRows>)>,
    describe: Vec<(String, Result<CatalogTraits, String>)>,
    underlyings: Option<Result<Vec<String>, String>>,
    listing: Option<Result<OptionsListing, String>>,
    expiries: Vec<(String, Result<Vec<OptionChainExpiry>, String>)>,
    chains: Vec<((String, String), Result<OptionChain, String>)>,
    families: Vec<((String, String), FamilyScript)>,
    presence: Vec<(String, CredentialPresence)>,
    budget: Vec<BudgetCounter>,
    raw_fetch: Option<Arc<FetchFn>>,
}

impl Scripts {
    fn derived_facts(&self) -> RestRosters {
        let mut catalog = keys(&self.describe);
        for venue in keys(&self.catalog_list).into_iter().chain(keys(&self.catalog_search)) {
            if !catalog.contains(&venue) {
                catalog.push(venue);
            }
        }
        let mut family: Vec<String> = Vec::new();
        for ((venue, _), _) in &self.families {
            if !family.contains(venue) {
                family.push(venue.clone());
            }
        }
        RestRosters {
            klines: keys(&self.klines),
            funding: keys(&self.funding),
            ticks: keys(&self.ticks),
            catalog,
            credentialed: keys(&self.credentialed),
            options: self.options_served
                || !self.option_index.is_empty()
                || !self.option_trades.is_empty()
                || self.underlyings.is_some()
                || self.listing.is_some()
                || !self.expiries.is_empty()
                || !self.chains.is_empty(),
            family,
            presence: true,
            market_rules: keys(&self.market_rules),
        }
    }

    /// The scripted answer for `what` (cloned out, so the lock is not held while it sleeps), or the
    /// words naming what has no script.
    fn answer_for(
        &self,
        what: &FetchWhat,
        credentialed: bool,
    ) -> Result<Answer<FetchRows>, String> {
        let (table, key): (&[(String, Answer<FetchRows>)], &str) = match what {
            FetchWhat::Klines { venue, .. } if credentialed => {
                (self.credentialed.as_slice(), venue.as_str())
            }
            FetchWhat::Klines { venue, .. } => (self.klines.as_slice(), venue.as_str()),
            FetchWhat::Funding { venue, .. } => (self.funding.as_slice(), venue.as_str()),
            FetchWhat::Ticks { venue, .. } => (self.ticks.as_slice(), venue.as_str()),
            FetchWhat::CatalogList { venue } => (self.catalog_list.as_slice(), venue.as_str()),
            FetchWhat::CatalogSearch { venue, .. } => {
                (self.catalog_search.as_slice(), venue.as_str())
            }
            FetchWhat::OptionTrades { currency, .. } => {
                (self.option_trades.as_slice(), currency.as_str())
            }
            FetchWhat::OptionIndex { kind } => {
                return find(&self.option_index, |k| k == kind).cloned().ok_or_else(|| {
                    format!("scripted rest lane: no script for an option_index fetch of {kind:?}")
                });
            }
            FetchWhat::ClosedMarketRules { venue, .. }
            | FetchWhat::SeriesMarketRules { venue, .. }
            | FetchWhat::MarketRulesOfToken { venue, .. } => {
                (self.market_rules.as_slice(), venue.as_str())
            }
        };
        find(table, |k| k == key).cloned().ok_or_else(|| {
            format!(
                "scripted rest lane: no script for a {}{} fetch of {key}",
                if credentialed { "credentialed " } else { "" },
                what.lane()
            )
        })
    }
}

struct Inner {
    scripts: Mutex<Scripts>,
    calls: Mutex<Vec<RestCall>>,
    feeds: Mutex<Vec<Arc<FeedOptionsScript>>>,
    feed_error: Mutex<Option<String>>,
    cancels: AtomicUsize,
}

/// The scripted REST lane. Cheap to clone: every clone is a handle on the same lane, so a test keeps
/// one to script and inspect after the server took another, and the `with_*` setters return a handle so
/// they chain.
#[derive(Clone)]
pub struct RestLaneScript {
    inner: Arc<Inner>,
}

impl Default for RestLaneScript {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RestLaneScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedRest").field("facts", &self.facts()).finish_non_exhaustive()
    }
}

impl RestLaneScript {
    /// A lane with nothing scripted: it serves only the status and presence words.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                scripts: Mutex::new(Scripts::default()),
                calls: Mutex::new(Vec::new()),
                feeds: Mutex::new(Vec::new()),
                feed_error: Mutex::new(None),
                cancels: AtomicUsize::new(0),
            }),
        }
    }

    fn script(&self, edit: impl FnOnce(&mut Scripts)) -> Self {
        edit(&mut lock(&self.inner.scripts));
        self.clone()
    }

    fn record(&self, call: RestCall) {
        lock(&self.inner.calls).push(call);
    }

    // ---- scripts ----------------------------------------------------------------------------------

    /// Replace the DERIVED advertisement with exactly `facts`.
    pub fn with_facts(&self, facts: RestRosters) -> Self {
        self.script(|s| s.facts = Some(facts))
    }

    /// Advertise the options surface even though no REST call is scripted (a test of the feeds alone).
    pub fn with_options_served(&self) -> Self {
        self.script(|s| s.options_served = true)
    }

    /// Script `venue`'s keyless kline fetch.
    pub fn with_klines(&self, venue: &str, answer: Answer<Vec<Bar>>) -> Self {
        self.script(|s| put(&mut s.klines, venue.into(), answer.map(FetchRows::Bars)))
    }

    /// Script `venue`'s CREDENTIALED kline fetch (`FetchCredentialed`).
    pub fn with_credentialed_klines(&self, venue: &str, answer: Answer<Vec<Bar>>) -> Self {
        self.script(|s| put(&mut s.credentialed, venue.into(), answer.map(FetchRows::Bars)))
    }

    /// Script `venue`'s funding fetch.
    pub fn with_funding(&self, venue: &str, answer: Answer<Vec<FundingRatePoint>>) -> Self {
        self.script(|s| put(&mut s.funding, venue.into(), answer.map(FetchRows::Funding)))
    }

    /// Script `venue`'s tick fetch (`dukascopy` is the one real venue).
    pub fn with_ticks(&self, venue: &str, answer: Answer<Vec<TickQuote>>) -> Self {
        self.script(|s| put(&mut s.ticks, venue.into(), answer.map(FetchRows::Quotes)))
    }

    /// Script the option-trades fetch of `currency` (the options surface's lane: scripting it advertises the
    /// options surface).
    pub fn with_option_trades(&self, currency: &str, answer: Answer<Vec<OptionTradeRow>>) -> Self {
        self.script(|s| {
            put(&mut s.option_trades, currency.into(), answer.map(FetchRows::OptionTrades))
        })
    }

    /// Script `venue`'s market-rules answer (both lanes: the archive window and the token lookup).
    pub fn with_market_rules(&self, venue: &str, answer: Answer<Vec<vike_data::TextRow>>) -> Self {
        self.script(|s| put(&mut s.market_rules, venue.into(), answer.map(FetchRows::Texts)))
    }

    /// Script `venue`'s catalog describe answer (`Err` is the lane's words, passed through).
    pub fn with_describe(&self, venue: &str, traits: Result<CatalogTraits, String>) -> Self {
        self.script(|s| put(&mut s.describe, venue.into(), traits))
    }

    /// Script `venue`'s whole instrument listing.
    pub fn with_catalog_list(&self, venue: &str, answer: Answer<Vec<Instrument>>) -> Self {
        self.script(|s| put(&mut s.catalog_list, venue.into(), answer.map(FetchRows::Instruments)))
    }

    /// Script `venue`'s query-backed search answer.
    pub fn with_catalog_search(&self, venue: &str, answer: Answer<Vec<Instrument>>) -> Self {
        self.script(|s| {
            put(&mut s.catalog_search, venue.into(), answer.map(FetchRows::Instruments));
        })
    }

    /// Script a whole catalog venue at once: its traits and its listing.
    pub fn with_catalog(
        &self,
        venue: &str,
        asset_classes: Vec<AssetClass>,
        mode: CatalogMode,
        instruments: Vec<Instrument>,
    ) -> Self {
        self.with_describe(venue, Ok(CatalogTraits { asset_classes, mode }))
            .with_catalog_list(venue, Answer::rows(instruments))
    }

    /// Script one of the two option-index listings (`Option`, `Future`).
    pub fn with_option_index(
        &self,
        kind: OptionIndexKind,
        answer: Answer<Vec<Instrument>>,
    ) -> Self {
        self.script(|s| {
            let rows = answer.map(FetchRows::Instruments);
            put(&mut s.option_index, kind, rows);
        })
    }

    /// Script `list_underlyings`.
    pub fn with_options_underlyings(&self, answer: Result<Vec<String>, String>) -> Self {
        self.script(|s| s.underlyings = Some(answer))
    }

    /// Script the option listing (`OptionsCall::Listing`, a refresh or not: the call is recorded, so a
    /// test reads which it was from `options_calls`). A later script replaces it, so a test can change
    /// what the venue lists between two asks.
    pub fn with_options_listing(&self, answer: Result<OptionsListing, String>) -> Self {
        self.script(|s| s.listing = Some(answer))
    }

    /// Script `list_expiries(underlying)`.
    pub fn with_options_expiries(
        &self,
        underlying: &str,
        answer: Result<Vec<OptionChainExpiry>, String>,
    ) -> Self {
        self.script(|s| put(&mut s.expiries, underlying.into(), answer))
    }

    /// Script `fetch_chain(underlying, expiry, ..)` for one expiry; the strike window is not applied.
    pub fn with_options_chain(
        &self,
        underlying: &str,
        expiry: &str,
        answer: Result<OptionChain, String>,
    ) -> Self {
        self.script(|s| put(&mut s.chains, (underlying.into(), expiry.into()), answer))
    }

    /// Script a rolling family's resolve: the same answer whatever the instant.
    pub fn with_family(
        &self,
        venue: &str,
        family: &str,
        answer: Result<Vec<String>, String>,
    ) -> Self {
        self.script(|s| {
            put(&mut s.families, (venue.into(), family.into()), FamilyScript::Fixed(answer));
        })
    }

    /// Script a rolling family's resolve as a function of the instant asked (`now_ms`), for a test of
    /// the rotation.
    pub fn with_family_by(
        &self,
        venue: &str,
        family: &str,
        answer: impl Fn(i64) -> Result<Vec<String>, String> + Send + Sync + 'static,
    ) -> Self {
        self.script(|s| {
            put(&mut s.families, (venue.into(), family.into()), FamilyScript::By(Arc::new(answer)));
        })
    }

    /// Script the presence word for `venue` (every other venue answers `NotNeeded`).
    pub fn with_presence(&self, venue: &str, word: CredentialPresence) -> Self {
        self.script(|s| put(&mut s.presence, venue.into(), word))
    }

    /// Script the budget rows the status report carries.
    pub fn with_budget(&self, rows: Vec<BudgetCounter>) -> Self {
        self.script(|s| s.budget = rows)
    }

    /// Answer EVERY fetch from `script`, ahead of the per-venue scripts (module doc).
    pub fn on_fetch(
        &self,
        script: impl Fn(
            &ConnectionMemos,
            &FetchWhat,
            bool,
            &FetchContext,
        ) -> Result<FetchRows, SourceError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.script(|s| s.raw_fetch = Some(Arc::new(script)))
    }

    /// Make the NEXT options feed open fail with `text`.
    pub fn fail_next_feed(&self, text: &str) {
        *lock(&self.inner.feed_error) = Some(text.to_string());
    }

    // ---- the recorder -----------------------------------------------------------------------------

    /// Every request received, in arrival order.
    pub fn calls(&self) -> Vec<RestCall> {
        lock(&self.inner.calls).clone()
    }

    /// The fetches received, in arrival order: what was asked, and whether credentialed.
    pub fn fetches(&self) -> Vec<FetchCall> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                RestCall::Fetch(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    /// The options-provider calls received, in arrival order.
    pub fn options_calls(&self) -> Vec<OptionsCall> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                RestCall::Options(o) => Some(o),
                _ => None,
            })
            .collect()
    }

    /// How many slow answers gave up because their fetch was cancelled.
    pub fn cancels_seen(&self) -> usize {
        self.inner.cancels.load(Ordering::SeqCst)
    }

    /// Block (up to five seconds) until at least `n` slow answers have seen a cancel. Panics otherwise:
    /// a cancel that never reaches the lane leaves a venue call running for a caller that left.
    pub fn wait_for_cancels(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.cancels_seen() < n {
            assert!(
                Instant::now() < deadline,
                "the lane saw {} cancels, not {n}: a cancel never reached it",
                self.cancels_seen()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// How many options feeds have been opened.
    pub fn feeds_opened(&self) -> usize {
        lock(&self.inner.feeds).len()
    }

    /// The `n`th options feed, waiting up to five seconds for it to open.
    pub fn feed(&self, n: usize) -> Arc<FeedOptionsScript> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(f) = lock(&self.inner.feeds).get(n) {
                return Arc::clone(f);
            }
            assert!(Instant::now() < deadline, "scripted rest lane: options feed {n} never opened");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl RestLane for RestLaneScript {
    fn facts(&self) -> RestRosters {
        let scripts = lock(&self.inner.scripts);
        scripts.facts.clone().unwrap_or_else(|| scripts.derived_facts())
    }

    fn fetch(
        &self,
        scope: &ConnectionMemos,
        what: &FetchWhat,
        credentialed: bool,
        ctx: &FetchContext,
    ) -> Result<FetchRows, SourceError> {
        self.record(RestCall::Fetch(FetchCall { what: what.clone(), credentialed }));
        let (raw, answer) = {
            let scripts = lock(&self.inner.scripts);
            (scripts.raw_fetch.clone(), scripts.answer_for(what, credentialed))
        };
        if let Some(raw) = raw {
            return raw(scope, what, credentialed, ctx);
        }
        match answer {
            Ok(answer) => answer.resolve(ctx, &self.inner.cancels),
            Err(words) => Err(SourceError::Fetch(words.into())),
        }
    }

    fn catalog_describe(
        &self,
        _scope: &ConnectionMemos,
        venue: &str,
    ) -> Result<CatalogTraits, String> {
        self.record(RestCall::CatalogDescribe { venue: venue.to_string() });
        let scripts = lock(&self.inner.scripts);
        find(&scripts.describe, |k| k == venue)
            .cloned()
            .unwrap_or_else(|| Err(format!("scripted rest lane: no describe script for {venue}")))
    }

    fn options(
        &self,
        _scope: &ConnectionMemos,
        call: &OptionsCall,
    ) -> Result<OptionsReply, String> {
        self.record(RestCall::Options(call.clone()));
        let scripts = lock(&self.inner.scripts);
        fn missing<T>(what: &str) -> Result<T, String> {
            Err(format!("scripted rest lane: no options script for {what}"))
        }
        match call {
            OptionsCall::Underlyings => scripts
                .underlyings
                .clone()
                .unwrap_or_else(|| missing("the underlyings"))
                .map(OptionsReply::Underlyings),
            OptionsCall::Listing { .. } => scripts
                .listing
                .clone()
                .unwrap_or_else(|| missing("the listing"))
                .map(OptionsReply::Listing),
            OptionsCall::Expiries { underlying } => find(&scripts.expiries, |k| k == underlying)
                .cloned()
                .unwrap_or_else(|| missing(&format!("the expiries of {underlying}")))
                .map(OptionsReply::Expiries),
            OptionsCall::Chain { underlying, expiry, .. } => {
                find(&scripts.chains, |k| k.0 == *underlying && k.1 == *expiry)
                    .cloned()
                    .unwrap_or_else(|| missing(&format!("the {underlying} chain of {expiry}")))
                    .map(|chain| OptionsReply::Chain(Box::new(chain)))
            }
        }
    }

    fn open_options_feed(
        &self,
        _scope: &ConnectionMemos,
        spec: &OptionsFeedSpec,
        sink: Arc<dyn FeedOptionsSink>,
    ) -> Result<Box<dyn FeedOptionsHandle>, String> {
        self.record(RestCall::OpenFeed(spec.clone()));
        if let Some(text) = lock(&self.inner.feed_error).take() {
            return Err(text);
        }
        let shut_down = Arc::new(AtomicBool::new(false));
        lock(&self.inner.feeds).push(Arc::new(FeedOptionsScript {
            sink,
            spec: spec.clone(),
            shut_down: Arc::clone(&shut_down),
        }));
        Ok(Box::new(ScriptedFeedHandle(shut_down)))
    }

    fn resolve_family(
        &self,
        _scope: &ConnectionMemos,
        venue: &str,
        family: &str,
        now_ms: i64,
    ) -> Result<Vec<String>, String> {
        self.record(RestCall::ResolveFamily {
            venue: venue.to_string(),
            family: family.to_string(),
            now_ms,
        });
        let script = {
            let scripts = lock(&self.inner.scripts);
            find(&scripts.families, |k| k.0 == venue && k.1 == family).cloned()
        };
        match script {
            Some(FamilyScript::Fixed(answer)) => answer,
            Some(FamilyScript::By(answer)) => answer(now_ms),
            None => Err(format!("scripted rest lane: no family script for {venue} {family}")),
        }
    }

    fn credential_presence(&self, venue: &str) -> CredentialPresence {
        self.record(RestCall::Presence { venue: venue.to_string() });
        let scripts = lock(&self.inner.scripts);
        find(&scripts.presence, |k| k == venue).copied().unwrap_or(CredentialPresence::NotNeeded)
    }

    fn budget_counters(&self) -> Vec<BudgetCounter> {
        lock(&self.inner.scripts).budget.clone()
    }
}
