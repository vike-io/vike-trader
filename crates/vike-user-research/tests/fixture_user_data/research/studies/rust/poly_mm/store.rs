//! `ContextStore`: the context-backed, memoizing `HistStore` a study presents to the seam.

use std::collections::HashMap;
use std::sync::Mutex;

use vike_data::{
    CohortRow, DataError, ExecFillRow, ExecOrderRow, HistStore, PerpMetricRow, TsRange,
};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};
use vike_user_research::StudyContext;

// The binary's `CachedHistStore` with its backing swapped from a store to the StudyContext's read
// verbs. Why a study must PRESENT a store, and the three narrowings: the module doc's "memoizing
// wrapper" section.

/// Verb discriminant folded into the cache key — plain `u8` codes rather than a keyed enum so the
/// key tuple stays `Hash`/`Eq` with zero extra derive plumbing.
const VERB_QUOTES: u8 = 0;
const VERB_TRADES: u8 = 1;
const VERB_BOOKS: u8 = 2;
const VERB_BARS: u8 = 3;

/// One memoized read verb's decoded result. The binary's fifth variant, `Properties`, is gone with
/// its verb: this adapter serves no symbol properties, so there is nothing to cache.
#[derive(Clone)]
enum CachedRows {
    Quotes(Vec<QuoteTick>),
    Trades(Vec<TradeTick>),
    Books(Vec<BookUpdate>),
    Bars(Vec<Bar>),
}

/// The memoization cache key: `(verb, venue, symbol, from, to)` — `load_bars`' extra `interval`
/// axis is folded into the symbol slot (`"{symbol}\0{interval}"`) rather than widening the tuple,
/// since every other verb has no such axis. A named alias (clippy's `type_complexity` gate).
type CacheKey = (u8, String, String, i64, i64);

/// The store a study PRESENTS to [`vike_user_research::StudySim::run_one`]: every read forwarded to
/// the [`StudyContext`] it was built from, memoized per `(verb, venue, symbol, range)`.
/// It owns a CLONE of the context (the seam's `Arc<dyn HistStore + Send + Sync>` is `'static`);
/// the clone is cheap: two `Arc`s, a `TsRange` and a `PathBuf`.
pub(super) struct ContextStore {
    ctx: StudyContext,
    cache: Mutex<HashMap<CacheKey, CachedRows>>,
}

impl ContextStore {
    pub(super) fn new(ctx: StudyContext) -> Self {
        Self { ctx, cache: Mutex::new(HashMap::new()) }
    }

    /// One read's cache key. `TsRange`'s open bounds resolve to the widest representable `i64`
    /// pair, so an unbounded scan still gets a stable key.
    fn key(verb: u8, venue: &str, symbol: String, range: TsRange) -> CacheKey {
        let (from, to) = (range.start.unwrap_or(i64::MIN), range.end.unwrap_or(i64::MAX));
        (verb, venue.to_string(), symbol, from, to)
    }

    /// Shared memoize-or-fetch: look up `key` in the cache; on miss, call `fetch`, cache the
    /// wrapped rows, and return the freshly fetched value.
    fn memoize<T: Clone>(
        &self,
        key: CacheKey,
        wrap: impl Fn(T) -> CachedRows,
        unwrap: impl Fn(&CachedRows) -> Option<T>,
        fetch: impl FnOnce() -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        if let Some(cached) = self.cache.lock().unwrap().get(&key).and_then(&unwrap) {
            return Ok(cached);
        }
        let value = fetch()?;
        self.cache.lock().unwrap().insert(key, wrap(value.clone()));
        Ok(value)
    }

    /// The refusal every unreachable verb shares. `DataError` has no "refused by contract"
    /// variant; `Query` is the closer of its two, so the message, not the variant, carries why.
    fn unreachable(verb: &str) -> DataError {
        DataError::Query(format!(
            "{verb}: a study's store is the StudyContext read verbs, which expose no {verb} — \
             see crates/vike-user-research/src/contract.rs. This is a REFUSAL, not an empty \
             result: nothing here looked."
        ))
    }

    /// The write half's refusal, kept separate so the message names the rule rather than a gap.
    fn read_only(verb: &str) -> DataError {
        DataError::Query(format!(
            "{verb}: a study's store is READ-ONLY — \
             docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md. Filling a gap is \
             an ingest command's job, and reporting a write that did not happen as `Ok(0)` would \
             hide it."
        ))
    }
}

/// Memoize-or-fetch one read verb under `key`: `$variant` names the `CachedRows` arm the rows are
/// cached under AND the arm a hit is read back from, so the pair cannot disagree.
macro_rules! memo {
    ($store:expr, $variant:ident, $key:expr, $fetch:expr) => {
        $store.memoize(
            $key,
            CachedRows::$variant,
            |c| if let CachedRows::$variant(v) = c { Some(v.clone()) } else { None },
            || $fetch,
        )
    };
}

/// Trait verbs this store REFUSES, one per line: the verb's name, its parameter types as the trait
/// spells them, and its `Ok` type. The body is `Err(Self::$why("<verb>"))` with `$why` either
/// `unreachable` or `read_only`, so the message names the verb exactly as written here.
macro_rules! refuse {
    ($why:ident: $($(#[$doc:meta])* fn $verb:ident($($ty:ty),*) -> $ok:ty;)*) => {
        $(
            $(#[$doc])*
            fn $verb(&self, $(_: $ty),*) -> Result<$ok, DataError> {
                Err(Self::$why(stringify!($verb)))
            }
        )*
    };
}

impl HistStore for ContextStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        let key = Self::key(VERB_BARS, venue, format!("{symbol}\0{interval}"), range);
        memo!(self, Bars, key, self.ctx.bars(venue, symbol, interval, range))
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        let key = Self::key(VERB_QUOTES, venue, symbol.to_string(), range);
        memo!(self, Quotes, key, self.ctx.quotes(venue, symbol, range))
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        let key = Self::key(VERB_TRADES, venue, symbol.to_string(), range);
        memo!(self, Trades, key, self.ctx.trades(venue, symbol, range))
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let key = Self::key(VERB_BOOKS, venue, symbol.to_string(), range);
        memo!(self, Books, key, self.ctx.book_updates(venue, symbol, range))
    }

    /// FORWARDED rather than inherited, and NOT memoized (this study never reads depth). The
    /// trait's "this store serves no depth lane" is true of a LEAF and false of a type fronting
    /// one: whether depth exists is the real store's fact, and `StudyContext::depth` passes that
    /// store's refusal through unchanged.
    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.ctx.depth(venue, symbol, range)
    }

    /// FORWARDED, where the binary's wrapper left this on the trait default. The context has the
    /// verb, so inheriting "this store holds no cohort panel" would be a false statement about a
    /// store that may well hold one.
    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        self.ctx.cohort(venue, asset, range)
    }

    /// FORWARDED for the same reason as [`ContextStore::scan_cohort`].
    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.ctx.perp_metrics(venue, symbol, range)
    }

    // ---- reads with no context verb behind them: REFUSE, never answer empty ----

    refuse! { unreachable:
        /// ⚠ The one that can break a run: the trait's `properties_as_of` default derives from
        /// this verb, so a simulator that pre-fetches an instrument grid gets this refusal. The
        /// cure is a `properties` verb on `StudyContext`, not a softer body here.
        fn scan_symbol_properties(&str, &str, TsRange) -> Vec<(i64, SymbolProperties)>;
        fn scan_equity(&str, &str, TsRange) -> Vec<EquitySample>;
        fn scan_exec_fills(&str, &str) -> Vec<ExecFillRow>;
        fn scan_exec_orders(&str, &str) -> Vec<ExecOrderRow>;
    }

    // ---- the write half: required methods with nothing to write through ----
    // Parameters: (venue, symbol, [interval,] rows | range, commit_key).

    refuse! { read_only:
        fn append_bars(&str, &str, &str, &[Bar], Option<&str>) -> usize;
        fn append_quotes(&str, &str, &[QuoteTick], Option<&str>) -> usize;
        fn append_trades(&str, &str, &[TradeTick], Option<&str>) -> usize;
        fn append_book_updates(&str, &str, &[BookUpdate], Option<&str>) -> usize;
        fn append_symbol_properties(&str, &str, &[(i64, SymbolProperties)], Option<&str>) -> usize;
        fn append_equity(&str, &str, &[EquitySample], Option<&str>) -> usize;
        fn append_exec_fills(&str, &str, &[ExecFillRow], Option<&str>) -> usize;
        fn append_exec_orders(&str, &str, &[ExecOrderRow], Option<&str>) -> usize;
        fn resample_quotes_to_bars(&str, &str, &str, TsRange, Option<&str>) -> usize;
        fn resample_trades_to_bars(&str, &str, &str, TsRange, Option<&str>) -> usize;
    }

    // `list_series` / `inventory` / `series_gaps` / `coverage_report` / the funding, chain and
    // depth-append verbs stay on the trait's defaults. For the two ENUMERATION verbs that REVERSES
    // the binary wrapper's choice (module doc, narrowing 3): "cannot enumerate" was false for a
    // type fronting a store that can, and is TRUE for one fronting a contract with no catalog verb.
}
