//! `discovery` — **declarative market discovery** + a **rolling-window mount** for Polymarket.
//! No Python twin — new capability, additive and entirely caller-driven (this module spawns NO
//! thread, opens NO socket, and reads NO env var; nothing changes until a caller calls it).
//!
//! [`universe`](crate::universe) answers "which of the markets I fetched are worth quoting?" —
//! a liquidity ranking over one fetched page. This module answers the two questions that sit *in
//! front of* that:
//!
//! 1. **Which markets do I even fetch?** — [`MarketFilter`], a declarative fetch+accept pair, so a
//!    strategy states its universe as data ("slugs starting `btc-updown-`, not expired,
//!    ≥ $500 liquidity") instead of hand-rolling a Gamma paging loop. Ships four:
//!    [`SlugPrefixFilter`], [`TagFilter`], [`SearchFilter`], [`PredicateFilter`].
//! 2. **What about markets that don't exist yet?** — [`RollingWindowPlanner`], for the recurring
//!    short-lived series Polymarket runs continuously (the canonical case: *BTC up or down, 5-min*).
//!    Each window is a DIFFERENT market with a different `condition_id`/`token_id`s and a
//!    deterministic slug; a maker that wants to be quoting continuously must roll onto the next
//!    window before the current one expires. The planner computes the current + next N window slugs
//!    from a slug template and a time bucket, resolves them to `token_id`s, and hands the resulting
//!    desired set to [`UniverseManager::plan_tokens`] so the subscribe/unsubscribe diffing is the
//!    SAME rule the liquidity path uses (`universe.rs` is the one owner of that logic).
//!
//! ## Network seam
//!
//! Gamma access goes through the [`GammaSource`] trait, implemented for the real
//! [`GammaClient`] (which proxies via `egress::agent()` — Polymarket is geo-blocked). Every unit test
//! in this file drives a fixture source, so the whole selection + rolling core is testable offline,
//! mirroring `resolve.rs`'s `ResolveDeps` discipline.
//!
//! ## Expiry and the settlement hand-off (read this)
//!
//! When a window expires, its slugs stop being produced, so its token_ids simply fall out of the
//! desired set and the diff unsubscribes them — this module's ONLY notion of expiry. That is a
//! *market-data* concern and nothing more. **A still-settling position in an expired window is NOT
//! this module's business**: dropping a token from the streamed universe does not close a position,
//! and must not be read as having closed one. The local book is retired by
//! [`resolve`](crate::resolve) (the condition-resolution watchlist, which emits the terminal
//! settlement fill at payout) and the on-chain money by [`auto_redeem`](crate::auto_redeem). A
//! caller running both should let the resolve watchlist keep watching a token after this planner has
//! unsubscribed it — the watchlist is fed from `/positions`, not from the subscribed set, so the two
//! are already independent by construction and no wiring is needed to keep them so.
//!
//! ## Driving the tick (how `vike-run` would mount this)
//!
//! [`RollingWindowPlanner::tick`] is a plain synchronous function taking `now_ms` — there is no
//! internal timer, no thread, and no interior mutability, so the caller owns the cadence and the
//! clock (which is also what makes the boundary tests below deterministic). A `vike-run` mount is a
//! loop in the binary's own thread:
//!
//! Every placeholder in a slug template is **UTC** — there is no timezone knob, so a template must
//! spell a series whose slugs are UTC-stamped. A series named in a local zone (an `-et` suffix, say)
//! is NOT expressible here and must not be faked by relabelling UTC digits: a fixed offset would
//! also be wrong across DST for a year-round ET series. Adding a real `tz` to [`WindowSpec`] is a
//! follow-up that starts with a DST-correct zone rule, not an offset field.
//!
//! ```ignore
//! let mut planner = RollingWindowPlanner::new(WindowSpec::updown("btc", 300_000)); // btc-updown-5m-{unix}
//! let mut universe = UniverseManager::default();
//! let resolver = GammaSlugResolver::new(&GammaClient, FetchSpec::default());
//! loop {
//!     let now_ms = /* wall clock, epoch-ms */;
//!     match planner.tick(now_ms, &resolver, &mut universe) {
//!         Ok(t) => {
//!             for token in &t.diff.to_add    { feeds.subscribe_book(token)?; }
//!             for token in &t.diff.to_remove { feeds.unsubscribe(token)?; }
//!             // t.missing = windows Gamma has not listed yet — normal; retried next tick.
//!         }
//!         // a Gamma read failed: the universe is left UNCHANGED (no diff was committed),
//!         // so a transient outage never mass-unsubscribes a live book. Just retry.
//!         Err(e) => tracing::warn!(error = %e, "rolling-window tick failed"),
//!     }
//!     std::thread::sleep(Duration::from_secs(30)); // any cadence < bucket works
//! }
//! ```
//!
//! Re-ticking inside the same window is a no-op: slugs are unchanged, resolutions are cached, and
//! the committed universe already matches, so the diff comes back empty and the feeds are untouched.
//! `now_ms` is clamped monotonically inside the planner, so a backwards NTP step across a bucket
//! boundary cannot roll the universe back onto an expired window.
//!
//! ## One manager per source (or union explicitly)
//!
//! [`UniverseManager::plan_tokens`] diffs a target against the manager's WHOLE subscribed set, so a
//! planner and the liquidity-ranked path sharing one manager would each unsubscribe everything the
//! other added, every pass. Give each source its own manager, or union their targets
//! ([`RollingTick::target`]) and commit once through [`UniverseManager::plan_union`].

use std::collections::{BTreeMap, BTreeSet};

use crate::gamma::{GammaClient, GammaMarket};
use crate::universe::{UniverseDiff, UniverseManager};

// ---------------------------------------------------------------------------------------------
// Gamma source seam
// ---------------------------------------------------------------------------------------------

/// The read seam over the Gamma market directory — one page per call, exactly
/// [`GammaClient::list`]'s shape. Exists so every filter/planner in this module is unit-testable
/// against a fixture list with no network (same role `ResolveDeps` plays in `resolve.rs`).
pub trait GammaSource {
    /// Fetch one page of markets. `active_only` maps to Gamma's `active=true&closed=false`.
    fn list(
        &self,
        active_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GammaMarket>, String>;

    /// Look ONE market up by its exact slug, volume-independently. `Ok(None)` = not listed.
    ///
    /// This is a first-class seam method rather than a scan helper because the paged browse is
    /// ordered `volumeNum` DESC: a just-listed window of a recurring series has ~$0 volume and
    /// therefore sorts below ANY bounded crawl ceiling, so scanning can never find the exact case
    /// [`RollingWindowPlanner`] exists to serve. The default implementation is that (doomed) bounded
    /// scan, kept only so a source that genuinely has no by-slug route still compiles;
    /// [`GammaClient`] overrides it with Gamma's `slug=` query, which is one request and O(1).
    fn by_slug(&self, slug: &str, spec: FetchSpec) -> Result<Option<GammaMarket>, String> {
        let want = slug.to_lowercase();
        if spec.page_limit == 0 {
            return Ok(None);
        }
        for page in 0..spec.max_pages {
            let batch = self.list(spec.active_only, spec.page_limit, page * spec.page_limit)?;
            let short = batch.len() < spec.page_limit;
            if let Some(m) = batch.iter().find(|m| m.slug.to_lowercase() == want) {
                return Ok(Some(m.clone()));
            }
            if short {
                break;
            }
        }
        Ok(None)
    }
}

impl GammaSource for GammaClient {
    fn list(
        &self,
        active_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GammaMarket>, String> {
        GammaClient::list(active_only, limit, offset)
    }

    /// The real by-slug route: ONE `slug=` request, independent of the market's volume rank.
    fn by_slug(&self, slug: &str, spec: FetchSpec) -> Result<Option<GammaMarket>, String> {
        GammaClient::by_slug(slug, spec.active_only)
    }
}

/// How a [`MarketFilter`] pages the directory. Bounded on purpose: Gamma has thousands of markets
/// and a discovery pass must never become an unbounded crawl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchSpec {
    /// Restrict to `active && !closed` at the API level (the tradeable browse).
    pub active_only: bool,
    /// Markets requested per page.
    pub page_limit: usize,
    /// Hard cap on pages fetched in one pass (`page_limit * max_pages` is the crawl ceiling).
    pub max_pages: usize,
}

impl Default for FetchSpec {
    /// 5 × 100 = at most 500 markets scanned per discovery pass — a few seconds of Gamma reads.
    fn default() -> Self {
        Self { active_only: true, page_limit: 100, max_pages: 5 }
    }
}

// ---------------------------------------------------------------------------------------------
// MarketFilter
// ---------------------------------------------------------------------------------------------

/// A declarative universe rule: how to *fetch* candidates and which to *accept*.
///
/// Implementors normally supply only [`accept`](Self::accept) (+ optionally
/// [`fetch_spec`](Self::fetch_spec)); the provided [`fetch`](Self::fetch) pages `src` under that
/// spec and retains the accepted markets, stopping early on a short page. Override `fetch` only for
/// a rule that can push its predicate into the query string.
pub trait MarketFilter {
    /// Does this market belong to the universe? Pure — no I/O, no clock (a time-dependent rule
    /// takes its `now_ms` as filter state, see [`SlugPrefixFilter::not_expired_at`]).
    fn accept(&self, market: &GammaMarket) -> bool;

    /// Paging bounds for [`fetch`](Self::fetch). Defaults to [`FetchSpec::default`].
    fn fetch_spec(&self) -> FetchSpec {
        FetchSpec::default()
    }

    /// Page `src` and return the accepted markets, in directory order (Gamma orders by volume
    /// desc). Stops at the first short page or at `max_pages`, whichever comes first. Any page
    /// error aborts the whole pass — a partial universe is worse than no update, since the caller
    /// would diff against a truncated set and mass-unsubscribe live books.
    ///
    /// NOTE the volume-ordering blind spot this inherits: a rule whose targets are LOW-volume (a
    /// fresh window of a recurring series) will not find them inside `page_limit * max_pages`. That
    /// is what [`GammaSource::by_slug`] exists for; a filter pass is for ranking-scale discovery.
    ///
    /// `page_limit == 0` returns empty rather than re-requesting offset 0 `max_pages` times (a
    /// zero-size page is never "short", so the loop would otherwise spin to the cap for nothing).
    fn fetch(&self, src: &dyn GammaSource) -> Result<Vec<GammaMarket>, String> {
        let spec = self.fetch_spec();
        if spec.page_limit == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for page in 0..spec.max_pages {
            let batch = src.list(spec.active_only, spec.page_limit, page * spec.page_limit)?;
            let short = batch.len() < spec.page_limit;
            out.extend(batch.into_iter().filter(|m| self.accept(m)));
            if short {
                break;
            }
        }
        Ok(out)
    }
}

/// Accept markets whose `slug` starts with `prefix`, with optional expiry and depth floors. The
/// workhorse for a recurring series, whose windows share a stable slug stem
/// (`btc-updown-5m-…`).
#[derive(Debug, Clone, PartialEq)]
pub struct SlugPrefixFilter {
    /// Required `slug` prefix (matched case-insensitively — Gamma slugs are lowercase, but a
    /// hand-written config often is not).
    pub prefix: String,
    /// When `Some(now_ms)`, require a PARSEABLE `end_date` strictly after `now_ms`. A market whose
    /// `end_date` is missing or malformed is REJECTED under this gate (a window with no known
    /// expiry cannot be proven unexpired); leave it `None` to ignore expiry entirely.
    pub not_expired_at: Option<i64>,
    /// Minimum `liquidity` (`0.0` = no floor).
    pub min_liquidity: f64,
    /// Minimum `volume` (`0.0` = no floor).
    pub min_volume: f64,
    /// Paging bounds for the fetch pass.
    pub fetch: FetchSpec,
}

impl SlugPrefixFilter {
    /// A prefix rule with no expiry gate and no floors.
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            not_expired_at: None,
            min_liquidity: 0.0,
            min_volume: 0.0,
            fetch: FetchSpec::default(),
        }
    }

    /// Require an `end_date` strictly after `now_ms`.
    pub fn not_expired_at(mut self, now_ms: i64) -> Self {
        self.not_expired_at = Some(now_ms);
        self
    }

    /// Set the liquidity / volume floors.
    pub fn with_floors(mut self, min_liquidity: f64, min_volume: f64) -> Self {
        self.min_liquidity = min_liquidity;
        self.min_volume = min_volume;
        self
    }
}

impl MarketFilter for SlugPrefixFilter {
    fn accept(&self, m: &GammaMarket) -> bool {
        if !m.slug.to_lowercase().starts_with(&self.prefix.to_lowercase()) {
            return false;
        }
        if m.liquidity < self.min_liquidity || m.volume < self.min_volume {
            return false;
        }
        match self.not_expired_at {
            // unparseable/absent end_date cannot be proven unexpired -> reject under this gate.
            Some(now) => m.resolution_ts_ms().is_some_and(|end| end > now),
            None => true,
        }
    }

    fn fetch_spec(&self) -> FetchSpec {
        self.fetch
    }
}

/// Accept markets carrying the given keyword tags.
///
/// NOTE the honest scope: Gamma's own `tags` array is NOT part of the parsed
/// [`GammaMarket`] (see `gamma.rs` — the parse is pinned to the fields the trading path needs, and
/// widening that struct is a separate change). So this matches each keyword as a case-insensitive
/// substring of `slug` + `question`, which is how Polymarket slugs encode their category in
/// practice (`nba-…`, `…-election-…`). It is a keyword filter with tag ergonomics, not a query
/// against Gamma's taxonomy; a real taxonomy filter is a follow-up that starts by parsing `tags`.
#[derive(Debug, Clone, PartialEq)]
pub struct TagFilter {
    /// Keywords to look for (case-insensitive).
    pub tags: Vec<String>,
    /// `true` = every keyword must match (AND); `false` = any one suffices (OR, the default).
    pub require_all: bool,
    /// Paging bounds for the fetch pass.
    pub fetch: FetchSpec,
}

impl TagFilter {
    /// An OR-of-keywords filter.
    pub fn any(tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            tags: tags.into_iter().map(Into::into).collect(),
            require_all: false,
            fetch: FetchSpec::default(),
        }
    }

    /// An AND-of-keywords filter.
    pub fn all(tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { require_all: true, ..Self::any(tags) }
    }
}

impl MarketFilter for TagFilter {
    fn accept(&self, m: &GammaMarket) -> bool {
        // An empty tag list matches nothing — an empty universe is a visible mistake, whereas
        // "matches everything" would silently subscribe the whole directory.
        if self.tags.is_empty() {
            return false;
        }
        let hay = format!("{} {}", m.slug, m.question).to_lowercase();
        let mut hits = self.tags.iter().map(|t| hay.contains(&t.to_lowercase()));
        if self.require_all { hits.all(|h| h) } else { hits.any(|h| h) }
    }

    fn fetch_spec(&self) -> FetchSpec {
        self.fetch
    }
}

/// Accept markets matching a free-text query — the same case-insensitive substring rule over
/// `question` + `slug` that [`MarketCatalog::search`](crate::catalog::MarketCatalog::search) uses,
/// so an operator's picker search and a strategy's declared universe agree on what "matches" means.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchFilter {
    /// The free-text query (case-insensitive substring).
    pub query: String,
    /// Paging bounds for the fetch pass.
    pub fetch: FetchSpec,
}

impl SearchFilter {
    /// A query filter with default paging.
    pub fn new(query: impl Into<String>) -> Self {
        Self { query: query.into(), fetch: FetchSpec::default() }
    }
}

impl MarketFilter for SearchFilter {
    fn accept(&self, m: &GammaMarket) -> bool {
        // An empty query matches nothing (same reasoning as TagFilter's empty tag list).
        if self.query.is_empty() {
            return false;
        }
        let q = self.query.to_lowercase();
        m.question.to_lowercase().contains(&q) || m.slug.to_lowercase().contains(&q)
    }

    fn fetch_spec(&self) -> FetchSpec {
        self.fetch
    }
}

/// Accept whatever a boxed closure accepts — the escape hatch for a rule the three declarative
/// filters cannot express (e.g. "neg-risk markets with a 0.001 tick"). `Send + Sync` so a mount can
/// hold one across threads.
pub struct PredicateFilter {
    predicate: Box<dyn Fn(&GammaMarket) -> bool + Send + Sync>,
    /// Paging bounds for the fetch pass.
    pub fetch: FetchSpec,
}

impl PredicateFilter {
    /// Wrap `predicate` with default paging.
    pub fn new(predicate: impl Fn(&GammaMarket) -> bool + Send + Sync + 'static) -> Self {
        Self { predicate: Box::new(predicate), fetch: FetchSpec::default() }
    }
}

impl std::fmt::Debug for PredicateFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PredicateFilter").field("fetch", &self.fetch).finish_non_exhaustive()
    }
}

impl MarketFilter for PredicateFilter {
    fn accept(&self, m: &GammaMarket) -> bool {
        (self.predicate)(m)
    }

    fn fetch_spec(&self) -> FetchSpec {
        self.fetch
    }
}

// ---------------------------------------------------------------------------------------------
// Rolling windows
// ---------------------------------------------------------------------------------------------

/// A recurring short-lived series: how its slugs are spelled and how long each window lasts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSpec {
    /// Slug template with `{...}` placeholders substituted from each window's START instant, in
    /// **UTC**: `{yyyy}` (4-digit year), `{mm}`/`{dd}`/`{HH}`/`{MM}` (2-digit, zero-padded),
    /// `{unix}` (epoch seconds). Unknown placeholders are left verbatim.
    ///
    /// Example (the current Polymarket up/down series): `"btc-updown-5m-{unix}"` — see
    /// [`WindowSpec::updown`], which builds it from the asset label and `bucket_ms`.
    pub slug_template: String,
    /// Window length in ms. Also the bucket the clock is floored to, so window starts are the
    /// epoch-aligned multiples of this value (a 5-minute bucket starts at :00, :05, :10, …).
    pub bucket_ms: i64,
    /// How many FUTURE windows to hold alongside the current one. `1` (the default via
    /// [`WindowSpec::every_minutes`]) = "current + next", which is what a continuously-quoting maker
    /// needs: the next window's book is already streaming when the current one expires.
    pub windows_ahead: usize,
}

impl WindowSpec {
    /// An `n`-minute series holding current + next.
    pub fn every_minutes(n: i64, slug_template: impl Into<String>) -> Self {
        Self { slug_template: slug_template.into(), bucket_ms: n * 60_000, windows_ahead: 1 }
    }

    /// The recurring Polymarket **"up or down"** series for `asset`, spelled the way Gamma spells
    /// it TODAY (verified live): `{asset}-updown-{interval}-{unix}`, where `{unix}` is the window
    /// START in UTC epoch **seconds** (bucket-aligned, via [`render_slug`]'s `{unix}` token) and
    /// `{interval}` is the bucket's minute label ([`interval_label`]: `300000 → 5m`,
    /// `900000 → 15m`, `60000 → 1m`). `asset` is Gamma's lowercase asset label — `btc` / `eth`.
    /// Holds current + next window.
    ///
    /// This replaces the stale `bitcoin-up-or-down-{yyyy}-{mm}-{dd}-{HH}{MM}` calendar slug the
    /// series used before Gamma renamed it to the epoch-stamped form; the generic calendar
    /// placeholders remain available through [`every_minutes`](Self::every_minutes) for any other
    /// series that still spells its windows that way.
    pub fn updown(asset: &str, bucket_ms: i64) -> Self {
        Self {
            slug_template: format!("{asset}-updown-{}-{{unix}}", interval_label(bucket_ms)),
            bucket_ms,
            windows_ahead: 1,
        }
    }

    /// Set how many future windows to hold.
    pub fn with_windows_ahead(mut self, n: usize) -> Self {
        self.windows_ahead = n;
        self
    }

    /// The START instants (epoch-ms) of the current + `windows_ahead` future windows, ascending.
    ///
    /// The current window is `now_ms` floored to `bucket_ms`. Flooring is
    /// **[`i64::div_euclid`]**, not truncating division, so a pre-1970 (negative) `now_ms` floors
    /// DOWN like every other instant rather than toward zero — no discontinuity at the epoch.
    /// Returns empty when `bucket_ms <= 0` (a misconfigured spec selects nothing rather than
    /// dividing by zero).
    pub fn window_starts(&self, now_ms: i64) -> Vec<i64> {
        if self.bucket_ms <= 0 {
            return Vec::new();
        }
        let current = now_ms.div_euclid(self.bucket_ms) * self.bucket_ms;
        (0..=self.windows_ahead as i64).map(|k| current + k * self.bucket_ms).collect()
    }

    /// The slugs of the current + `windows_ahead` future windows, ascending by start instant.
    pub fn window_slugs(&self, now_ms: i64) -> Vec<String> {
        self.window_starts(now_ms)
            .into_iter()
            .map(|s| render_slug(&self.slug_template, s))
            .collect()
    }
}

/// Substitute the UTC calendar placeholders of `template` from `start_ms`. Dependency-free
/// (`vike_model::time::civil_from_days`); unknown `{...}` tokens are left verbatim so a typo shows
/// up in the unresolved slug instead of silently matching the wrong market.
pub fn render_slug(template: &str, start_ms: i64) -> String {
    let (y, mo, d, h, mi) = utc_parts(start_ms);
    template
        .replace("{yyyy}", &format!("{y:04}"))
        .replace("{mm}", &format!("{mo:02}"))
        .replace("{dd}", &format!("{d:02}"))
        .replace("{HH}", &format!("{h:02}"))
        .replace("{MM}", &format!("{mi:02}"))
        .replace("{unix}", &(start_ms.div_euclid(1000)).to_string())
}

/// The Polymarket up/down interval label for a bucket length in ms: the whole-minute count with an
/// `m` suffix, so `300000 → "5m"`, `900000 → "15m"`, `60000 → "1m"` — exactly the labels Gamma uses
/// in the `{asset}-updown-{interval}-{unix}` slug. A sub-minute bucket floors toward zero (`"0m"`),
/// which no live up/down series uses. Pure; used by [`WindowSpec::updown`].
pub fn interval_label(bucket_ms: i64) -> String {
    format!("{}m", bucket_ms / 60_000)
}

/// epoch-ms → UTC `(year, month, day, hour, minute)`. Euclidean division throughout, so pre-1970
/// instants decompose correctly (a plain `/`/`%` would give a negative hour-of-day).
fn utc_parts(ms: i64) -> (i64, u32, u32, i64, i64) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, mo, d) = vike_model::time::civil_from_days(days);
    (y, mo, d, sod / 3600, (sod % 3600) / 60)
}

/// Resolve a window slug to its tradeable outcome `token_id`s.
///
/// Separate from [`GammaSource`] because "look one slug up" and "page the directory" are different
/// costs with different natural implementations (a live mount pages once and indexes; a test hands
/// over a map). `Ok(vec![])` means "not listed yet / no tradeable tokens" and is NORMAL for a future
/// window — the planner reports it as [`RollingTick::missing`] and retries. `Err` means the lookup
/// itself failed and aborts the tick.
pub trait WindowResolver {
    /// Token ids for `slug`, or an empty vec when the market is not listed (yet).
    fn resolve(&self, slug: &str) -> Result<Vec<String>, String>;
}

/// The production [`WindowResolver`]: ONE [`GammaSource::by_slug`] lookup per call.
///
/// It must be a by-slug lookup, not a browse scan: Gamma's paged browse is ordered `volumeNum`
/// DESC, and a window that has just been listed has ~$0 volume, so it sorts below the bounded crawl
/// ceiling of thousands of markets — a scanning resolver would return "not listed" for every future
/// window forever, which is precisely the case this type exists to serve.
///
/// Cost: one request per *uncached* slug. [`RollingWindowPlanner`] caches every successful
/// resolution for as long as its window stays in scope, and throttles retries of an unlisted slug
/// (see [`RollingWindowPlanner::with_miss_retry_ms`]), so a steady mount does one request per new
/// window plus at most the throttled retries for windows Gamma has not listed yet. A deployment
/// watching many series at once should implement [`WindowResolver`] over its own shared index.
pub struct GammaSlugResolver<'a> {
    src: &'a dyn GammaSource,
    spec: FetchSpec,
}

impl<'a> GammaSlugResolver<'a> {
    /// Resolve against `src` with the given paging bounds.
    pub fn new(src: &'a dyn GammaSource, spec: FetchSpec) -> Self {
        Self { src, spec }
    }
}

impl WindowResolver for GammaSlugResolver<'_> {
    fn resolve(&self, slug: &str) -> Result<Vec<String>, String> {
        Ok(self.src.by_slug(slug, self.spec)?.map(|m| m.token_ids).unwrap_or_default())
    }
}

/// What one [`RollingWindowPlanner::tick`] decided.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RollingTick {
    /// The window slugs in scope this tick (current first, then the look-ahead windows).
    pub slugs: Vec<String>,
    /// Slugs that resolved to at least one token id, in `slugs` order.
    pub resolved: Vec<String>,
    /// Slugs the resolver could not find — a future window Gamma has not listed yet. NORMAL and
    /// non-fatal; it is simply absent from the desired set and retried on the next tick.
    pub missing: Vec<String>,
    /// The full desired token set this tick — every resolved window's token_ids, unioned. This is
    /// what was diffed to produce [`diff`](Self::diff); it is exposed so a mount running MORE than
    /// one universe source can union targets and commit once via
    /// [`UniverseManager::plan_union`] instead of letting two sources fight over one manager.
    pub target: BTreeSet<String>,
    /// The subscribe/unsubscribe delta the caller must apply to its feeds. Already committed to the
    /// [`UniverseManager`] by [`tick`](RollingWindowPlanner::tick) (not by
    /// [`plan`](RollingWindowPlanner::plan)).
    ///
    /// Only meaningful when this planner owns the manager it was diffed against — with a SHARED
    /// manager use [`target`](Self::target) + `plan_union` and ignore this field.
    pub diff: UniverseDiff,
}

/// Rolls a recurring series' universe forward in time.
///
/// Holds the [`WindowSpec`] and a slug→token_ids cache; owns NO clock, NO thread, and NO feed
/// handles — see the module docs for the `vike-run` drive loop.
///
/// **Each universe source must own its own [`UniverseManager`].** [`plan`](Self::plan) diffs its
/// target against the manager's ENTIRE subscribed set, so pointing this planner and the
/// liquidity-ranked [`refresh`](UniverseManager::refresh) path at ONE manager makes each pass
/// unsubscribe everything the other added — a permanent subscribe/unsubscribe storm on live books.
/// A mount that wants both sources into one manager must union their targets first and commit once:
/// take [`RollingTick::target`] and the liquidity path's target and call
/// [`UniverseManager::plan_union`].
#[derive(Debug)]
pub struct RollingWindowPlanner {
    spec: WindowSpec,
    /// slug → token_ids, so a re-tick inside the same window does zero network I/O.
    cache: BTreeMap<String, Vec<String>>,
    /// The highest `now_ms` ever ticked. Every later tick is clamped to at least this, so a
    /// backwards wall-clock step (NTP correction) cannot roll the window set BACKWARDS —
    /// re-subscribing an expired window, unsubscribing the freshly-added future one, and re-paying
    /// its resolution. Monotonicity is the planner's job because the mount is documented to feed
    /// raw wall clock.
    last_now_ms: Option<i64>,
    /// slug → the clamped `now_ms` at which its last failed resolution attempt was made. Drives
    /// [`miss_retry_ms`](Self::with_miss_retry_ms).
    miss_at: BTreeMap<String, i64>,
    /// Minimum ms between re-resolution attempts of an UNLISTED slug. `0` (default) = retry every
    /// tick, byte-identical to no throttle.
    miss_retry_ms: i64,
}

impl RollingWindowPlanner {
    /// A planner for `spec` with an empty cache and no miss throttle.
    pub fn new(spec: WindowSpec) -> Self {
        Self {
            spec,
            cache: BTreeMap::new(),
            last_now_ms: None,
            miss_at: BTreeMap::new(),
            miss_retry_ms: 0,
        }
    }

    /// Throttle re-resolution of a slug that came back UNLISTED to at most once per `ms`.
    ///
    /// A miss is deliberately never cached as empty (that would freeze a window out for its whole
    /// life), so by default every tick re-asks for every not-yet-listed window. That is one cheap
    /// by-slug request per miss, but at a 30s cadence against a rate-limited, proxied, geo-blocked
    /// endpoint it is still pure waste for a series that lists late — or for a MISCONFIGURED
    /// template whose slugs will never exist. `0` (the default) disables the throttle entirely.
    pub fn with_miss_retry_ms(mut self, ms: i64) -> Self {
        self.miss_retry_ms = ms;
        self
    }

    /// The last (clamped) `now_ms` this planner ticked at — diagnostics.
    pub fn last_now_ms(&self) -> Option<i64> {
        self.last_now_ms
    }

    /// The series spec.
    pub fn spec(&self) -> &WindowSpec {
        &self.spec
    }

    /// The slugs currently cached (ascending) — diagnostics.
    pub fn cached_slugs(&self) -> impl Iterator<Item = &str> {
        self.cache.keys().map(String::as_str)
    }

    /// Compute this tick's windows, resolve any not already cached, and DIFF the resulting desired
    /// token set against `universe` — WITHOUT committing it to the universe. Use to inspect before
    /// applying.
    ///
    /// Scope of "without committing": the `universe` is untouched. The PLANNER's own state is not —
    /// it clamps and records `now_ms`, memoizes freshly-resolved slugs, drops out-of-scope cache
    /// entries, and records misses for the retry throttle. Those are pure resolution bookkeeping (no
    /// subscription is implied), but a caller that plans and then declines to apply has still moved
    /// the planner's clock floor forward.
    ///
    /// `now_ms` is clamped to the highest value previously seen, so a backwards clock step never
    /// rolls the universe back to an expired window (see [`last_now_ms`](Self::last_now_ms)).
    ///
    /// Resolution errors abort the whole tick (`Err`): the universe is left untouched, so a
    /// transient Gamma outage can never be read as "every window vanished" and mass-unsubscribe.
    pub fn plan(
        &mut self,
        now_ms: i64,
        resolver: &dyn WindowResolver,
        universe: &UniverseManager,
    ) -> Result<RollingTick, String> {
        // Monotonic clamp BEFORE anything derives from the clock.
        let now_ms = match self.last_now_ms {
            Some(prev) if prev > now_ms => prev,
            _ => now_ms,
        };
        let slugs = self.spec.window_slugs(now_ms);

        // Resolve first (fallible), and only then mutate the cache / build the target: a mid-loop
        // failure must leave the planner exactly as it was.
        let mut fresh: Vec<(String, Vec<String>)> = Vec::new();
        for slug in &slugs {
            if self.cache.contains_key(slug) {
                continue;
            }
            // Throttled: a slug that missed recently is not re-asked until miss_retry_ms elapses.
            if self.miss_retry_ms > 0
                && let Some(at) = self.miss_at.get(slug)
                && now_ms.saturating_sub(*at) < self.miss_retry_ms
            {
                continue;
            }
            fresh.push((slug.clone(), resolver.resolve(slug)?));
        }
        for (slug, tokens) in fresh {
            // An unlisted window is NOT cached as empty — it must be retried (once the throttle
            // allows), since Gamma may list it at any point during the window's life. Caching the
            // miss as a resolution would freeze the window out for its whole lifetime.
            if tokens.is_empty() {
                self.miss_at.insert(slug, now_ms);
            } else {
                self.miss_at.remove(&slug);
                self.cache.insert(slug, tokens);
            }
        }
        self.last_now_ms = Some(now_ms);

        let mut resolved = Vec::new();
        let mut missing = Vec::new();
        let mut target: BTreeSet<String> = BTreeSet::new();
        for slug in &slugs {
            match self.cache.get(slug) {
                Some(tokens) => {
                    resolved.push(slug.clone());
                    target.extend(tokens.iter().cloned());
                }
                None => missing.push(slug.clone()),
            }
        }

        // Drop cache entries for windows that have rolled out of scope — bounded memory, and the
        // expired window's tokens leave the desired set (they are absent from `target` above), so
        // the diff unsubscribes them. Settling positions in that window are `resolve.rs`'s job.
        let in_scope: BTreeSet<&String> = slugs.iter().collect();
        self.cache.retain(|slug, _| in_scope.contains(slug));
        self.miss_at.retain(|slug, _| in_scope.contains(slug));

        let diff = universe.plan_tokens(target.clone());
        Ok(RollingTick { slugs, resolved, missing, target, diff })
    }

    /// [`plan`](Self::plan) + commit the diff to `universe` — the caller-driven re-evaluation tick.
    /// The returned [`RollingTick::diff`] is what to apply to the feeds; on `Err` nothing was
    /// committed.
    pub fn tick(
        &mut self,
        now_ms: i64,
        resolver: &dyn WindowResolver,
        universe: &mut UniverseManager,
    ) -> Result<RollingTick, String> {
        let tick = self.plan(now_ms, resolver, universe)?;
        universe.commit(&tick.diff);
        Ok(tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(slug: &str, question: &str, liq: f64, vol: f64, end: &str, toks: &[&str]) -> GammaMarket {
        GammaMarket {
            id: slug.into(),
            question: question.into(),
            condition_id: format!("0x{slug}"),
            slug: slug.into(),
            end_date: end.into(),
            volume: vol,
            liquidity: liq,
            active: true,
            closed: false,
            neg_risk: false,
            tick_size: 0.01,
            outcomes: vec!["Yes".into(), "No".into()],
            token_ids: toks.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// A fixture [`GammaSource`] over a fixed list, paged exactly like the real client.
    struct FixtureSource {
        markets: Vec<GammaMarket>,
        calls: std::cell::Cell<usize>,
    }

    impl FixtureSource {
        fn new(markets: Vec<GammaMarket>) -> Self {
            Self { markets, calls: std::cell::Cell::new(0) }
        }
    }

    impl GammaSource for FixtureSource {
        fn list(
            &self,
            active_only: bool,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<GammaMarket>, String> {
            self.calls.set(self.calls.get() + 1);
            Ok(self
                .markets
                .iter()
                .filter(|m| !active_only || (m.active && !m.closed))
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }
    }

    struct FailingSource;
    impl GammaSource for FailingSource {
        fn list(&self, _: bool, _: usize, _: usize) -> Result<Vec<GammaMarket>, String> {
            Err("gamma 503".into())
        }
    }

    /// A fixture source shaped like the REAL Gamma directory: `list` pages a `volumeNum`-DESC
    /// ordering (so a fresh ~$0-volume window sorts last, below any bounded crawl), while `by_slug`
    /// is the O(1) exact lookup `GammaClient::by_slug` performs. This is the fixture the original
    /// suite lacked — with insertion-ordered 4-row fixtures the volume blind spot is invisible.
    struct RankedSource {
        markets: Vec<GammaMarket>,
        pages: std::cell::Cell<usize>,
        slug_lookups: std::cell::Cell<usize>,
    }

    impl RankedSource {
        fn new(mut markets: Vec<GammaMarket>) -> Self {
            markets.sort_by(|a, b| b.volume.partial_cmp(&a.volume).unwrap());
            Self { markets, pages: std::cell::Cell::new(0), slug_lookups: std::cell::Cell::new(0) }
        }
    }

    impl GammaSource for RankedSource {
        fn list(
            &self,
            _active_only: bool,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<GammaMarket>, String> {
            self.pages.set(self.pages.get() + 1);
            Ok(self.markets.iter().skip(offset).take(limit).cloned().collect())
        }

        fn by_slug(&self, slug: &str, _spec: FetchSpec) -> Result<Option<GammaMarket>, String> {
            self.slug_lookups.set(self.slug_lookups.get() + 1);
            let want = slug.to_lowercase();
            Ok(self.markets.iter().find(|m| m.slug.to_lowercase() == want).cloned())
        }
    }

    /// A big directory: `n` high-volume markets plus one fresh zero-volume window at the bottom.
    fn ranked_directory(n: usize, fresh_slug: &str) -> Vec<GammaMarket> {
        let mut v: Vec<GammaMarket> = (0..n)
            .map(|i| {
                mk(
                    &format!("popular-{i}"),
                    "popular?",
                    10_000.0,
                    1_000_000.0 - i as f64,
                    "2026-12-31T00:00:00Z",
                    &["p"],
                )
            })
            .collect();
        v.push(mk(fresh_slug, "BTC up or down?", 0.0, 0.0, "2026-07-18T12:10:00Z", &["f1", "f2"]));
        v
    }

    fn sample() -> Vec<GammaMarket> {
        vec![
            mk(
                "btc-up-or-down-2026-07-18-1200",
                "BTC up or down?",
                5_000.0,
                900.0,
                "2026-07-18T12:05:00Z",
                &["b1", "b2"],
            ),
            mk(
                "btc-up-or-down-2026-07-18-1205",
                "BTC up or down?",
                100.0,
                0.0,
                "2026-07-18T12:10:00Z",
                &["c1", "c2"],
            ),
            mk(
                "nba-lakers-win-tonight",
                "Will the Lakers win?",
                20_000.0,
                50_000.0,
                "2026-07-19T00:00:00Z",
                &["n1", "n2"],
            ),
            mk("us-election-2028-winner", "Who wins in 2028?", 1.0, 2.0, "", &["e1"]),
        ]
    }

    // ---- filters ------------------------------------------------------------------------------

    #[test]
    fn slug_prefix_filter_matches_prefix_case_insensitively() {
        let f = SlugPrefixFilter::new("BTC-UP-OR-DOWN-");
        let src = FixtureSource::new(sample());
        let got = f.fetch(&src).unwrap();
        assert_eq!(
            got.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
            vec!["btc-up-or-down-2026-07-18-1200", "btc-up-or-down-2026-07-18-1205"]
        );
    }

    #[test]
    fn slug_prefix_filter_applies_liquidity_and_volume_floors() {
        let f = SlugPrefixFilter::new("btc-up-or-down-").with_floors(1_000.0, 500.0);
        let src = FixtureSource::new(sample());
        let got = f.fetch(&src).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].slug, "btc-up-or-down-2026-07-18-1200");
    }

    #[test]
    fn not_expired_gate_drops_past_and_unparseable_end_dates() {
        // 2026-07-18T12:06:00Z — the first window (ends 12:05) is expired, the second (12:10) is not.
        let now = mk("s", "q", 0.0, 0.0, "2026-07-18T12:06:00Z", &[]).resolution_ts_ms().unwrap();
        let f = SlugPrefixFilter::new("btc-up-or-down-").not_expired_at(now);
        let src = FixtureSource::new(sample());
        let got = f.fetch(&src).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].slug, "btc-up-or-down-2026-07-18-1205");

        // an absent/malformed end_date can't be proven unexpired -> rejected under the gate,
        // accepted without it.
        let gate = SlugPrefixFilter::new("us-election").not_expired_at(now);
        assert!(!gate.accept(&sample()[3]));
        assert!(SlugPrefixFilter::new("us-election").accept(&sample()[3]));
    }

    #[test]
    fn tag_filter_any_and_all() {
        let any = TagFilter::any(["nba", "election"]);
        let all = TagFilter::all(["btc", "down"]);
        let ms = sample();
        assert!(any.accept(&ms[2]) && any.accept(&ms[3]));
        assert!(!any.accept(&ms[0]));
        assert!(all.accept(&ms[0]));
        assert!(!TagFilter::all(["btc", "lakers"]).accept(&ms[0]));
        // empty tag list matches nothing (a visible mistake, not a full-directory subscribe)
        assert!(!TagFilter::any(Vec::<String>::new()).accept(&ms[0]));
    }

    #[test]
    fn tag_filter_matches_question_text_too() {
        // "lakers" is in the slug; "Will the" only in the question — both hit the same haystack.
        let ms = sample();
        assert!(TagFilter::any(["will the"]).accept(&ms[2]));
    }

    #[test]
    fn search_filter_matches_question_or_slug_case_insensitively() {
        let ms = sample();
        assert!(SearchFilter::new("LAKERS").accept(&ms[2]));
        assert!(SearchFilter::new("who wins").accept(&ms[3]));
        assert!(!SearchFilter::new("nothing here").accept(&ms[2]));
        // empty query matches nothing
        assert!(!SearchFilter::new("").accept(&ms[2]));
    }

    #[test]
    fn predicate_filter_runs_the_closure() {
        let f = PredicateFilter::new(|m: &GammaMarket| m.liquidity > 10_000.0);
        let src = FixtureSource::new(sample());
        let got = f.fetch(&src).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].slug, "nba-lakers-win-tonight");
    }

    #[test]
    fn fetch_pages_until_short_page_and_respects_max_pages() {
        let mut f = SlugPrefixFilter::new("");
        f.fetch = FetchSpec { active_only: true, page_limit: 2, max_pages: 5 };
        let src = FixtureSource::new(sample());
        let got = f.fetch(&src).unwrap();
        assert_eq!(got.len(), 4);
        // 4 markets / page_limit 2 -> pages of 2,2,0: stops on the first short (empty) page.
        assert_eq!(src.calls.get(), 3);

        // max_pages caps the crawl
        let mut capped = SlugPrefixFilter::new("");
        capped.fetch = FetchSpec { active_only: true, page_limit: 2, max_pages: 1 };
        let src2 = FixtureSource::new(sample());
        assert_eq!(capped.fetch(&src2).unwrap().len(), 2);
        assert_eq!(src2.calls.get(), 1);
    }

    #[test]
    fn fetch_error_aborts_the_pass() {
        let f = SlugPrefixFilter::new("btc");
        assert_eq!(f.fetch(&FailingSource).unwrap_err(), "gamma 503");
    }

    // ---- window slug computation --------------------------------------------------------------

    const TPL: &str = "bitcoin-up-or-down-{yyyy}-{mm}-{dd}-{HH}{MM}";

    #[test]
    fn window_starts_floor_to_the_bucket_and_look_ahead() {
        let spec = WindowSpec::every_minutes(5, TPL);
        // 12:03:30 -> current window starts 12:00, next 12:05.
        let base = 1_784_376_000_000; // 2026-07-18T12:00:00Z (cross-checked below)
        let starts = spec.window_starts(base + 3 * 60_000 + 30_000);
        assert_eq!(starts, vec![base, base + 300_000]);
        // windows_ahead=2 -> three windows
        let s3 = spec.clone().with_windows_ahead(2).window_starts(base);
        assert_eq!(s3, vec![base, base + 300_000, base + 600_000]);
        // exactly on a boundary: that instant IS the new window's start
        assert_eq!(spec.window_starts(base + 300_000), vec![base + 300_000, base + 600_000]);
        // one ms before the boundary still belongs to the previous window
        assert_eq!(spec.window_starts(base + 299_999), vec![base, base + 300_000]);
    }

    #[test]
    fn window_slugs_render_utc_calendar_fields() {
        let spec = WindowSpec::every_minutes(5, TPL);
        let base = 1_784_376_000_000; // 2026-07-18T12:00:00Z
        assert_eq!(
            spec.window_slugs(base + 1),
            vec![
                "bitcoin-up-or-down-2026-07-18-1200".to_string(),
                "bitcoin-up-or-down-2026-07-18-1205".to_string(),
            ]
        );
        // the anchor itself, verified against gamma.rs's independently-tested date parser
        assert_eq!(
            mk("s", "q", 0.0, 0.0, "2026-07-18T12:00:00Z", &[]).resolution_ts_ms(),
            Some(base)
        );
    }

    #[test]
    fn window_slugs_cross_utc_midnight_and_month_end() {
        let spec = WindowSpec::every_minutes(5, TPL);
        // 2026-07-31T23:57:00Z -> current 23:55 (Jul 31), next 00:00 (Aug 1): day, month AND the
        // 23->00 hour all roll in one step.
        let t = mk("s", "q", 0.0, 0.0, "2026-07-31T23:57:00Z", &[]).resolution_ts_ms().unwrap();
        assert_eq!(
            spec.window_slugs(t),
            vec![
                "bitcoin-up-or-down-2026-07-31-2355".to_string(),
                "bitcoin-up-or-down-2026-08-01-0000".to_string(),
            ]
        );
        // and a year boundary
        let ny = mk("s", "q", 0.0, 0.0, "2026-12-31T23:58:00Z", &[]).resolution_ts_ms().unwrap();
        assert_eq!(
            spec.window_slugs(ny),
            vec![
                "bitcoin-up-or-down-2026-12-31-2355".to_string(),
                "bitcoin-up-or-down-2027-01-01-0000".to_string(),
            ]
        );
    }

    #[test]
    fn window_starts_floor_downward_before_the_epoch() {
        let spec = WindowSpec::every_minutes(5, TPL);
        // -1 ms is 1969-12-31T23:59:59.999Z: euclidean flooring puts it in the 23:55 window, NOT
        // in 00:00 (which truncating division would have given).
        assert_eq!(spec.window_starts(-1), vec![-300_000, 0]);
        assert_eq!(
            spec.window_slugs(-1),
            vec![
                "bitcoin-up-or-down-1969-12-31-2355".to_string(),
                "bitcoin-up-or-down-1970-01-01-0000".to_string(),
            ]
        );
    }

    #[test]
    fn non_positive_bucket_selects_nothing_instead_of_dividing_by_zero() {
        let spec = WindowSpec { slug_template: TPL.into(), bucket_ms: 0, windows_ahead: 1 };
        assert!(spec.window_starts(1_784_376_000_000).is_empty());
        assert!(spec.window_slugs(1_784_376_000_000).is_empty());
    }

    #[test]
    fn render_slug_supports_unix_and_leaves_unknown_placeholders() {
        let base = 1_784_376_000_000;
        assert_eq!(render_slug("w-{unix}", base), "w-1784376000");
        assert_eq!(render_slug("w-{nope}-{HH}", base), "w-{nope}-12");
    }

    #[test]
    fn interval_label_maps_the_documented_buckets() {
        assert_eq!(interval_label(300_000), "5m");
        assert_eq!(interval_label(900_000), "15m");
        assert_eq!(interval_label(60_000), "1m");
    }

    /// The current Polymarket up/down series: `{asset}-updown-{interval}-{unix}`, `{unix}` = the
    /// window START in UTC epoch seconds, bucket-aligned.
    #[test]
    fn updown_series_renders_the_epoch_stamped_slug_and_floors() {
        // 2026-07-21T12:00:00Z = epoch 1_784_635_200 s, already 300s-aligned.
        const T: i64 = 1_784_635_200_000;
        let spec = WindowSpec::updown("btc", 300_000);

        // aligned start -> the exact epoch-stamped slug; plus the next (current + windows_ahead=1).
        assert_eq!(
            spec.window_slugs(T),
            vec!["btc-updown-5m-1784635200".to_string(), "btc-updown-5m-1784635500".to_string()]
        );
        // a non-aligned instant (137s into the window) floors to the 300s boundary.
        assert_eq!(spec.window_slugs(T + 137_000)[0], "btc-updown-5m-1784635200");
        // one ms before the boundary still belongs to the previous window.
        assert_eq!(spec.window_slugs(T + 299_999)[0], "btc-updown-5m-1784635200");

        // eth asset label + the 15m / 1m interval buckets.
        assert_eq!(
            WindowSpec::updown("eth", 300_000).window_slugs(T)[0],
            "eth-updown-5m-1784635200"
        );
        assert_eq!(
            WindowSpec::updown("btc", 900_000).window_slugs(T)[0],
            "btc-updown-15m-1784635200"
        );
        assert_eq!(
            WindowSpec::updown("btc", 60_000).window_slugs(T)[0],
            "btc-updown-1m-1784635200"
        );
    }

    // ---- planner ------------------------------------------------------------------------------

    /// A [`WindowResolver`] over a fixed slug→tokens map, counting lookups.
    struct MockResolver {
        map: BTreeMap<String, Vec<String>>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl MockResolver {
        fn new(pairs: &[(&str, &[&str])]) -> Self {
            Self {
                map: pairs
                    .iter()
                    .map(|(s, t)| ((*s).to_string(), t.iter().map(|x| x.to_string()).collect()))
                    .collect(),
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl WindowResolver for MockResolver {
        fn resolve(&self, slug: &str) -> Result<Vec<String>, String> {
            self.calls.borrow_mut().push(slug.to_string());
            Ok(self.map.get(slug).cloned().unwrap_or_default())
        }
    }

    struct FailingResolver;
    impl WindowResolver for FailingResolver {
        fn resolve(&self, _: &str) -> Result<Vec<String>, String> {
            Err("gamma 503".into())
        }
    }

    const BASE: i64 = 1_784_376_000_000; // 2026-07-18T12:00:00Z
    const W1200: &str = "bitcoin-up-or-down-2026-07-18-1200";
    const W1205: &str = "bitcoin-up-or-down-2026-07-18-1205";
    const W1210: &str = "bitcoin-up-or-down-2026-07-18-1210";

    fn planner() -> RollingWindowPlanner {
        RollingWindowPlanner::new(WindowSpec::every_minutes(5, TPL))
    }

    #[test]
    fn planner_first_tick_subscribes_current_and_next_windows() {
        let mut p = planner();
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1", "a2"]), (W1205, &["b1"])]);

        let t = p.tick(BASE, &r, &mut u).unwrap();
        assert_eq!(t.slugs, vec![W1200, W1205]);
        assert_eq!(t.resolved, vec![W1200, W1205]);
        assert!(t.missing.is_empty());
        assert_eq!(t.diff.to_add, vec!["a1", "a2", "b1"]); // sorted-set order
        assert!(t.diff.to_remove.is_empty());
        assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["a1", "a2", "b1"]);
    }

    #[test]
    fn planner_re_tick_in_the_same_window_is_an_empty_diff_and_no_refetch() {
        let mut p = planner();
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);

        p.tick(BASE, &r, &mut u).unwrap();
        let before = r.calls.borrow().len();
        // later in the SAME bucket -> identical slugs, cached resolutions, no change.
        let t2 = p.tick(BASE + 299_999, &r, &mut u).unwrap();
        assert!(t2.diff.is_empty(), "idempotent re-evaluation must not churn the feeds");
        assert_eq!(r.calls.borrow().len(), before, "cache hit: no resolver traffic");
        assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["a1", "b1"]);
    }

    #[test]
    fn planner_rolls_forward_dropping_the_expired_window() {
        let mut p = planner();
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"]), (W1210, &["c1"])]);

        p.tick(BASE, &r, &mut u).unwrap();
        // cross into the next bucket: 12:00 expires, 12:10 joins.
        let t = p.tick(BASE + 300_000, &r, &mut u).unwrap();
        assert_eq!(t.slugs, vec![W1205, W1210]);
        assert_eq!(t.diff.to_add, vec!["c1"]);
        assert_eq!(t.diff.to_remove, vec!["a1"], "the expired window leaves the streamed set");
        assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["b1", "c1"]);
        // the rolled-off window is dropped from the cache too (bounded memory)
        assert_eq!(p.cached_slugs().collect::<Vec<_>>(), vec![W1205, W1210]);
    }

    #[test]
    fn planner_reports_an_unlisted_window_as_missing_and_retries_it() {
        let mut p = planner();
        let mut u = UniverseManager::default();
        // 12:05 is not listed yet.
        let r1 = MockResolver::new(&[(W1200, &["a1"])]);
        let t1 = p.tick(BASE, &r1, &mut u).unwrap();
        assert_eq!(t1.resolved, vec![W1200]);
        assert_eq!(t1.missing, vec![W1205]);
        assert_eq!(t1.diff.to_add, vec!["a1"]);

        // it appears on a later tick within the same window -> picked up, no unsubscribe churn.
        let r2 = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
        let t2 = p.tick(BASE + 60_000, &r2, &mut u).unwrap();
        assert!(t2.missing.is_empty());
        assert_eq!(t2.diff.to_add, vec!["b1"]);
        assert!(t2.diff.to_remove.is_empty());
        assert_eq!(r2.calls.borrow().as_slice(), [W1205], "only the uncached slug is looked up");
    }

    #[test]
    fn planner_plan_does_not_commit() {
        let mut p = planner();
        let u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
        let t = p.plan(BASE, &r, &u).unwrap();
        assert_eq!(t.diff.to_add, vec!["a1", "b1"]);
        assert_eq!(u.subscribed().count(), 0, "plan must leave the universe untouched");
    }

    #[test]
    fn planner_resolver_failure_leaves_the_universe_untouched() {
        let mut p = planner();
        let mut u = UniverseManager::default();
        let ok = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
        p.tick(BASE, &ok, &mut u).unwrap();

        // next bucket, but Gamma is down: the tick fails and NOTHING is unsubscribed.
        assert_eq!(p.tick(BASE + 300_000, &FailingResolver, &mut u).unwrap_err(), "gamma 503");
        assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["a1", "b1"]);
        assert_eq!(p.cached_slugs().collect::<Vec<_>>(), vec![W1200, W1205], "cache unchanged");
    }

    #[test]
    fn gamma_slug_resolver_finds_the_market_by_exact_slug() {
        let src = FixtureSource::new(sample());
        let r = GammaSlugResolver::new(
            &src,
            FetchSpec { active_only: true, page_limit: 2, max_pages: 5 },
        );
        assert_eq!(r.resolve("BTC-UP-OR-DOWN-2026-07-18-1205").unwrap(), vec!["c1", "c2"]);
        assert!(r.resolve("no-such-window").unwrap().is_empty());
        // and an error propagates rather than reading as "unlisted"
        let bad = GammaSlugResolver::new(&FailingSource, FetchSpec::default());
        assert_eq!(bad.resolve("x").unwrap_err(), "gamma 503");
    }

    /// THE regression for the volume-ordering blind spot: a fresh window sits below a crawl ceiling
    /// of 500 in a volume-DESC directory, so only a by-slug lookup can find it. Before the fix
    /// `GammaSlugResolver` scanned pages and returned empty here — i.e. it could never resolve the
    /// exact case it exists for.
    #[test]
    fn gamma_slug_resolver_finds_a_fresh_low_volume_window_below_the_crawl_ceiling() {
        let src = RankedSource::new(ranked_directory(900, W1210));
        let spec = FetchSpec::default(); // 100 * 5 = 500 markets scanned; the window is #901.
        let r = GammaSlugResolver::new(&src, spec);

        assert_eq!(r.resolve(W1210).unwrap(), vec!["f1", "f2"]);
        assert_eq!(src.slug_lookups.get(), 1, "one by-slug request, not a crawl");
        assert_eq!(src.pages.get(), 0, "the volume-ordered browse must not be paged at all");

        // sanity: the market really is unreachable by scanning under this spec.
        let scanned: Vec<String> = (0..spec.max_pages)
            .flat_map(|p| src.list(true, spec.page_limit, p * spec.page_limit).unwrap().into_iter())
            .map(|m| m.slug)
            .collect();
        assert_eq!(scanned.len(), 500);
        assert!(!scanned.iter().any(|s| s == W1210), "fresh window sorts below the ceiling");
    }

    /// The default `by_slug` (the scan fallback for a source with no by-slug route) still works for
    /// a market that IS inside the ceiling, and honours `page_limit == 0` without spinning.
    #[test]
    fn default_by_slug_scan_fallback_and_zero_page_limit() {
        let src = FixtureSource::new(sample());
        let spec = FetchSpec { active_only: true, page_limit: 2, max_pages: 5 };
        assert_eq!(
            src.by_slug("BTC-UP-OR-DOWN-2026-07-18-1205", spec).unwrap().unwrap().token_ids,
            vec!["c1", "c2"]
        );
        assert!(src.by_slug("no-such-window", spec).unwrap().is_none());

        let zero = FetchSpec { active_only: true, page_limit: 0, max_pages: 5 };
        let src2 = FixtureSource::new(sample());
        assert!(src2.by_slug("btc-up-or-down-2026-07-18-1205", zero).unwrap().is_none());
        assert_eq!(
            src2.calls.get(),
            0,
            "page_limit 0 must not re-request offset 0 max_pages times"
        );
    }

    /// `MarketFilter::fetch` with `page_limit == 0` returns empty instead of looping to `max_pages`
    /// re-requesting a zero-size page (a zero-size page is never "short").
    #[test]
    fn fetch_with_zero_page_limit_is_an_immediate_empty() {
        let mut f = SlugPrefixFilter::new("");
        f.fetch = FetchSpec { active_only: true, page_limit: 0, max_pages: 5 };
        let src = FixtureSource::new(sample());
        assert!(f.fetch(&src).unwrap().is_empty());
        assert_eq!(src.calls.get(), 0);
    }

    /// A backwards wall-clock step (NTP correction) across a bucket boundary must NOT roll the
    /// universe back: no re-subscribe of the expired window, no unsubscribe of the future one, and
    /// no re-resolution traffic.
    #[test]
    fn backwards_clock_step_does_not_roll_the_universe_back() {
        let mut p = planner();
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"]), (W1210, &["c1"])]);

        p.tick(BASE, &r, &mut u).unwrap();
        // cross the 12:05 boundary: 12:00 rolls off, 12:10 joins.
        p.tick(BASE + 300_000, &r, &mut u).unwrap();
        assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["b1", "c1"]);
        let calls_before = r.calls.borrow().len();

        // clock steps back 200ms, to just BEFORE the boundary we already crossed.
        let t = p.tick(BASE + 299_800, &r, &mut u).unwrap();
        assert!(t.diff.is_empty(), "a backwards clock step must not churn the feeds");
        assert_eq!(t.slugs, vec![W1205, W1210], "windows stay clamped forward");
        assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["b1", "c1"]);
        assert_eq!(r.calls.borrow().len(), calls_before, "no re-resolution of the expired window");
        assert_eq!(p.last_now_ms(), Some(BASE + 300_000));

        // and the clock catching back up past the clamp still rolls forward normally.
        let t2 = p.tick(BASE + 600_000, &r, &mut u).unwrap();
        assert_eq!(t2.slugs[0], W1210);
        assert_eq!(t2.diff.to_remove, vec!["b1"]);
    }

    /// The miss-retry throttle: an unlisted window is re-asked at most once per interval, and is
    /// still picked up as soon as the throttle lets a retry through.
    #[test]
    fn miss_retry_throttle_bounds_resolver_traffic_for_an_unlisted_window() {
        let mut p = RollingWindowPlanner::new(WindowSpec::every_minutes(5, TPL))
            .with_miss_retry_ms(120_000);
        let mut u = UniverseManager::default();
        let r1 = MockResolver::new(&[(W1200, &["a1"])]); // 12:05 not listed yet

        p.tick(BASE, &r1, &mut u).unwrap();
        assert_eq!(r1.calls.borrow().len(), 2); // both slugs asked once
        // 30s later: throttled, no new lookup, still reported missing.
        let t = p.tick(BASE + 30_000, &r1, &mut u).unwrap();
        assert_eq!(t.missing, vec![W1205]);
        assert_eq!(r1.calls.borrow().len(), 2, "throttled: no re-ask inside the interval");
        // 2min later: retried.
        p.tick(BASE + 120_000, &r1, &mut u).unwrap();
        assert_eq!(r1.calls.borrow().as_slice()[2], W1205);

        // default (no throttle) re-asks every tick — byte-identical to the pre-fix behavior.
        let mut p2 = planner();
        let mut u2 = UniverseManager::default();
        let r2 = MockResolver::new(&[(W1200, &["a1"])]);
        p2.tick(BASE, &r2, &mut u2).unwrap();
        p2.tick(BASE + 30_000, &r2, &mut u2).unwrap();
        assert_eq!(r2.calls.borrow().len(), 3, "unthrottled default retries every tick");
    }

    /// `windows_ahead = 0` is the degenerate current-window-only case.
    #[test]
    fn windows_ahead_zero_holds_only_the_current_window() {
        let spec = WindowSpec::every_minutes(5, TPL).with_windows_ahead(0);
        assert_eq!(spec.window_starts(BASE + 1), vec![BASE]);
        let mut p = RollingWindowPlanner::new(spec);
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
        let t = p.tick(BASE, &r, &mut u).unwrap();
        assert_eq!(t.slugs, vec![W1200]);
        assert_eq!(t.diff.to_add, vec!["a1"], "the next window is NOT pre-subscribed");
    }

    /// A large look-ahead whose future windows are all unlisted: only the current window is
    /// subscribed, the rest are reported missing (never an error, never a partial universe).
    #[test]
    fn large_look_ahead_with_every_future_window_unlisted() {
        let spec = WindowSpec::every_minutes(5, TPL).with_windows_ahead(4);
        let mut p = RollingWindowPlanner::new(spec);
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1"])]);
        let t = p.tick(BASE, &r, &mut u).unwrap();
        assert_eq!(t.resolved, vec![W1200]);
        assert_eq!(t.missing.len(), 4);
        assert_eq!(t.diff.to_add, vec!["a1"]);
        assert_eq!(t.target, ["a1".to_string()].into_iter().collect::<BTreeSet<_>>());
    }

    /// Two sources over ONE manager: diffing each target separately churns (which is why the docs
    /// no longer suggest it), while `plan_union` over both targets is stable.
    #[test]
    fn two_sources_share_one_manager_only_via_plan_union() {
        let mut p = planner();
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["w1"]), (W1205, &["w2"])]);

        // the liquidity path's target, and the planner's.
        let liquidity: BTreeSet<String> = ["l1".to_string(), "l2".to_string()].into();
        let window_target = p.plan(BASE, &r, &u).unwrap().target;

        // union: one commit, both sources' tokens subscribed.
        let d = u.plan_union([&liquidity, &window_target]);
        assert_eq!(d.to_add, vec!["l1", "l2", "w1", "w2"]);
        assert!(d.to_remove.is_empty());
        u.commit(&d);

        // re-planning both against the union is a clean no-op — no churn.
        let d2 = u.plan_union([&liquidity, &window_target]);
        assert!(d2.is_empty(), "a shared manager must not churn when both sources are unchanged");

        // whereas diffing ONE source alone against the shared manager would evict the other's
        // tokens — pinned here so the anti-pattern stays visibly wrong.
        let solo = u.plan_tokens(window_target.clone());
        assert_eq!(solo.to_remove, vec!["l1", "l2"]);
    }

    /// The pm-resolution hand-off, pinned rather than asserted in prose: when a window expires and
    /// its token is unsubscribed, the settlement watchlist must STILL be watching that token — the
    /// position is settling and its payout has not been emitted yet. (`polymarket`-gated with the
    /// settlement cluster it drives — the ONE exec-plane reference in this feed-plane module's
    /// tests; a `--features feeds` `cargo test` compiles this file without it.)
    #[cfg(feature = "polymarket")]
    #[test]
    fn expired_window_unsubscribe_leaves_a_settling_position_watched() {
        use crate::positions::Position;
        use crate::resolve::ResolveWatchlist;

        let mut p = planner();
        let mut u = UniverseManager::default();
        let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"]), (W1210, &["c1"])]);
        p.tick(BASE, &r, &mut u).unwrap();

        // we hold the 12:00 window's outcome token, awaiting settlement.
        let mut watch = ResolveWatchlist::new();
        let held = Position {
            condition_id: "0xw1200".into(),
            asset: "a1".into(),
            size: 25.0,
            redeemable: false, // not resolved yet — still settling
            neg_risk: false,
            outcome_index: Some(0),
            title: "BTC up or down?".into(),
            cur_price: None,
        };
        watch.upsert_from_positions(std::slice::from_ref(&held));

        // the window expires and the planner unsubscribes its token from the streamed universe...
        let t = p.tick(BASE + 300_000, &r, &mut u).unwrap();
        assert_eq!(t.diff.to_remove, vec!["a1"]);
        assert!(!u.subscribed().any(|s| s == "a1"), "market data for a1 is gone");

        // ...and the settlement watchlist is UNAFFECTED: it is fed from /positions, not from the
        // subscribed set, so the payout leg is still armed.
        assert_eq!(watch.len(), 1);
        assert_eq!(watch.get("a1").map(|e| e.qty), Some(25.0));
        // it only leaves the watchlist when /positions reports it flat (redeemed/sold).
        watch.upsert_from_positions(std::slice::from_ref(&held));
        assert_eq!(watch.len(), 1, "an unsubscribe is not a position event");
        assert_eq!(watch.prune_flat(&[]), vec!["a1"]);
    }

    #[test]
    fn planner_end_to_end_over_the_gamma_resolver() {
        // the two btc windows in `sample()` are exactly the 12:00 and 12:05 windows of a
        // "btc-up-or-down-{yyyy}-{mm}-{dd}-{HH}{MM}" series.
        let spec = WindowSpec::every_minutes(5, "btc-up-or-down-{yyyy}-{mm}-{dd}-{HH}{MM}");
        let mut p = RollingWindowPlanner::new(spec);
        let mut u = UniverseManager::default();
        let src = FixtureSource::new(sample());
        let r = GammaSlugResolver::new(&src, FetchSpec::default());

        let t = p.tick(BASE, &r, &mut u).unwrap();
        assert_eq!(t.diff.to_add, vec!["b1", "b2", "c1", "c2"]);
        assert!(t.missing.is_empty());
    }
}
