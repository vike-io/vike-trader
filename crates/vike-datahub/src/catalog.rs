//! **The venue-catalog lane: the provider table, and the bounds only the SERVER can have.**
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` classifies
//! [`Request::VenueCatalog`](vike_datahub_client::proto::Request::VenueCatalog) as
//! `VerbScope::Read`, and the ground is a question its predecessor did not have to ask: **is it a
//! write at all?** It is not — nothing here reaches the served store — so 0058's four-part rule is
//! inapplicable rather than satisfied, and what governs is 0052's cost rule: **the client names a
//! venue and every other term of the cost is a server constant.** This module is where those
//! constants live. Its siblings in `crates/vike-datahub-client/src/catalog.rs` are the ones BOTH
//! ends need (the venue validator, the listing cap, the outcome enum); the ones here are
//! deliberately invisible to a client, because a client that knew a token bucket's state could only
//! ever mis-predict it.
//!
//! # What it bounds, and which objection each bound answers
//!
//! | bound | answers |
//! |---|---|
//! | [`CatalogTable`]'s membership | "a client can make the server spend the OPERATOR'S CREDENTIALS" |
//! | [`CATALOG_VENUE_BURST`] / [`CATALOG_VENUE_REFILL`] | "an observe client can spend the box's venue budget" |
//! | [`CATALOG_TTL`], or the box's `config.datahub_catalog_ttl_secs` row | "a picker re-asks every time it opens" — proved server-side, not only by a well-behaved client |
//! | `vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS` | "one venue can return an unbounded frame" |
//! | [`CatalogLane`] existing at all | "the operator never consented" — an unarmed daemon builds none |
//!
//! # ⚠ The table's MEMBERSHIP is the load-bearing bound, not the bucket
//!
//! 0062's decision 3: **only a provider that reads a PUBLIC endpoint may be in this table.** Three
//! roster venues (alpaca, oanda, ctrader) can be listed only by authenticating as the operator, and
//! a rate limit bounds spending a public budget while nothing bounds spending an identity. So they
//! are excluded by CONSTRUCTION — `real_catalog_table` is the one function that builds entries, and
//! [`CatalogTable::new`] refuses a venue `vike_catalog::catalog_availability` does not call public.
//! That refusal is in the constructor rather than at the request door deliberately: a door check
//! can be reordered past, and a table that cannot HOLD the venue has no ordering to get wrong.
//!
//! # ⚠ The providers are the gatewayhub's now; this lane keeps the bounds
//!
//! `docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`: this daemon
//! opens no venue connection, so the eight providers moved to `vike-gatewayhub` (its `rest/catalog.rs`)
//! and a table entry here is a [`vike_gatewayhub_client::catalog::RemoteCatalog`] that asks the
//! daemon for the universe. Everything in the table above stays HERE, because every bound on it is
//! one only the SERVER of this verb can have: the listing cap (`list_via`), the TTL memo and the
//! per-venue bucket ([`CatalogLane`]), and the public-only membership ([`CatalogTable::new`]). The
//! daemon sends the provider's FULL answer and this side truncates, exactly where the provider seam
//! did before.
//!
//! # ⚠ The arming is what makes the SCOPE argument honest, so it is not a convenience
//!
//! An unarmed daemon constructs no `CatalogLane`, and `crate::server`'s `venue_catalog_verb` then
//! answers a SUCCESSFUL `CatalogOutcome::NotArmed` having called no venue. 0062's decision 4 gives
//! the three reasons the switch is kept even though decision 1 removes the obligation to have one —
//! the first being that polymarket's provider walks up to 40 pages where the chart-seed lane's
//! worst request was two GETs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vike_catalog::catalog_availability;
#[cfg(feature = "catalog-serve")]
use vike_gatewayhub_client::catalog::{CATALOG_VENUES, RemoteCatalog};
#[cfg(feature = "catalog-serve")]
use vike_gatewayhub_client::client::GatewayLink;
use vike_model::Instrument;

use crate::token_bucket::Bucket;

/// Fetches this lane will let through to ONE venue back-to-back, before the refill rate binds.
///
/// **Two, not eight**, and the difference from `crate::seed::SEED_VENUE_BURST` is deliberate. A
/// chart seed's burst is sized for an operator restoring a workspace where several charts come up
/// empty at once — many series, one venue each. A catalog refresh is ONE act per venue: the second
/// token exists for a retry after a failure or a mis-click, and there is no layout that legitimately
/// needs a third inside a minute.
pub const CATALOG_VENUE_BURST: u32 = 2;

/// How often ONE token returns to a venue's bucket — the SATURATION bound, and the number the
/// order-signing daemon's reservation is computed from.
///
/// **The arithmetic, against MEASURED budgets in `crates/vike-model/src/rate_limits.rs`** (binance
/// spot publishes 6000 weight/min, `fapi` 2400/min). ⚠ That file measures budgets and `/klines`
/// weights; it carries NO measured weight for `exchangeInfo`, so the two figures below are
/// binance's PUBLISHED weights and are labelled as such rather than dressed up as measurements.
/// The ceiling is low enough that the distinction cannot change the verdict.
///
/// 60 s -> **1 fetch/min/venue** saturated. Binance's catalog is one spot `exchangeInfo`
/// (published weight 20) plus one `fapi` `exchangeInfo` (published weight 1):
///
/// * against `fapi`: 1 x 1 = **1 weight/min = 0.04%** of 2,400;
/// * against spot: 1 x 20 = 20 of 6,000 = **0.33%**.
///
/// ⚠ **What that costs the order-signing daemon, as a number.** `crate::md::MD_LINGER` is the
/// authority for the market-data plane's share of the `fapi` budget and `crate::seed::
/// SEED_VENUE_REFILL` for what the chart-seed lane widened it to; neither figure is restated here,
/// because a number spelled in three places goes stale in two of them. The floor they leave the
/// order-signing daemon is **64.2%**, and this lane adds **1 weight/min** on top — so the floor
/// moves to **64.17%**, below the resolution of the figure it is subtracted from.
///
/// Three properties do the real work, and the arithmetic is the least of them:
///
/// 1. **The buckets are PER VENUE.** okx's per-option-family fan-out and polymarket's 40-page walk
///    spend okx's and polymarket's budgets, neither of which the order-signing daemon competes for,
///    and neither of which can starve binance's.
/// 2. **The steady state is bounded BELOW saturation by [`CATALOG_TTL`].** A memo hit calls no
///    venue, so a venue's true steady state is four fetches a day, not one a minute. Reaching
///    saturation needs a client that asks, waits out the whole TTL, and asks again, forever.
/// 3. **The two expensive venues are bounded by their OWN providers**, whose page caps are compiled
///    constants in the bridges (`GAMMA_MAX_PAGES` = 40). No request reaches them.
pub const CATALOG_VENUE_REFILL: Duration = Duration::from_secs(60);

/// How long a fetched catalog answers from memory before the venue is asked again.
///
/// Six hours. An instrument universe is one of the slowest-moving things a venue publishes — a new
/// listing is news, not a tick — so this is generous rather than aggressive, and the cost of being
/// stale is a symbol missing from a picker for at most one working session.
///
/// ⚠ **It is the bound that makes the STEADY state differ from the SATURATION ceiling**, which is
/// the property [`CATALOG_VENUE_REFILL`]'s reservation leans on: four fetches per venue per day,
/// against a bucket that would permit 1,440.
///
/// This is the DEFAULT. A box may write the `config.datahub_catalog_ttl_secs` row to change it
/// ([`catalog_ttl`] resolves the row), within [`CATALOG_TTL_FLOOR`] and a week
/// (`vike_config::config::DATAHUB_CATALOG_TTL_SECS`, which restates both ends and is held to them
/// by `crates/vike-datahub/src/catalog_tests.rs`'s
/// `the_ttl_row_range_is_this_lanes_floor_and_holds_the_default`). Only the steady state moves: how
/// fast a venue can be asked at all stays [`CATALOG_VENUE_REFILL`]'s.
pub const CATALOG_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// The shortest memo a row may ask for: the shortest the lane's own budget arithmetic below can
/// stand behind. Derived from [`CATALOG_VENUE_BURST`] rather than typed, so the assertion that
/// guards [`CATALOG_TTL`] and the floor `vike-config` refuses below cannot be two numbers.
pub const CATALOG_TTL_FLOOR: Duration = Duration::from_secs(CATALOG_VENUE_BURST as u64 * 4 * 60);

// The burst must be small enough that draining it cannot itself out-cost the steady state this lane
// advertises. Expressed as the inequality that reduces to: a full burst is `CATALOG_VENUE_BURST`
// fetches, and it must not exceed what the TTL would permit in a whole day. The `crate::seed` /
// `crate::md` idiom — an inequality, so a deliberate tweak compiles and a broken claim does not.
// `CATALOG_TTL_FLOOR` is that inequality's left side in seconds, which is what a row is held to.
const _: () = assert!(
    CATALOG_TTL.as_secs() >= CATALOG_TTL_FLOOR.as_secs(),
    "CATALOG_VENUE_BURST is large enough relative to CATALOG_TTL that a single burst out-costs the \
     lane's own advertised daily steady state. Re-run `docs/decisions/0062`'s reservation \
     arithmetic, do not widen this assertion."
);

/// The TTL a mount hands [`CatalogLane::new`]: the `config.datahub_catalog_ttl_secs` row, or
/// [`CATALOG_TTL`] when the box wrote none — so a box without the row keeps the six hours it always
/// had. The row's range is `vike-config`'s to refuse; this only reads what it let through.
pub fn catalog_ttl(row_secs: Option<u32>) -> Duration {
    row_secs.map_or(CATALOG_TTL, |secs| Duration::from_secs(u64::from(secs)))
}

// The refill must keep a SATURATED lane a negligible share of the tightest budget this workspace
// has MEASURED (binance `fapi`, 2,400 weight/min). A binance catalog costs 1 weight against `fapi`
// per fetch, so 60 s/refill fetches per minute must stay at or under 1% of 2,400 = 24/min, i.e. a
// refill of at least 2.5 s. The bound is deliberately far looser than the 60 s chosen, so lowering
// the constant is possible without a compile failure until it would genuinely start competing with
// the order-signing daemon — at which point this stops compiling and the author re-runs
// `CATALOG_VENUE_REFILL`'s arithmetic and 0062's reservation paragraph instead of quietly
// falsifying both.
const _: () = assert!(
    CATALOG_VENUE_REFILL.as_millis() >= 2_500,
    "CATALOG_VENUE_REFILL below 2.5 s pushes this lane past 1% of binance fapi's MEASURED 2,400 \
     weight/min budget, which is the share `docs/decisions/0062` reserved against the \
     order-signing daemon on the same public IP. Re-run that record's arithmetic, do not widen \
     this assertion."
);

/// One venue's provider: no arguments, because **the request names no term of this call**. Errors
/// are stringified because the wire carries them as text either way.
///
/// ⚠ The zero-argument signature is not an accident of convenience — it is 0062's decision 2 made
/// structural. A collector taking `(symbol, interval, start, end)` is a verb whose client names four
/// things; this one cannot grow a client-named dimension without changing its own type, which is the
/// first bullet of that record's reopen list.
pub type CatalogFn = Box<dyn Fn() -> Result<Vec<Instrument>, String> + Send + Sync>;

/// The venue → provider dispatch table `serve_with_catalog` mounts.
pub struct CatalogTable {
    entries: Vec<(String, CatalogFn)>,
}

impl CatalogTable {
    /// A table from explicit `(venue, provider)` entries — the test seam.
    ///
    /// ⚠ **It REFUSES a venue that is not publicly enumerable**, silently dropping it rather than
    /// panicking, and that is 0062's decision 3 enforced where it cannot be reordered past: a
    /// credentialed venue (alpaca/oanda/ctrader) cannot be in this table even by mistake, so no
    /// request door needs to remember to check. A dropped entry is reported by
    /// [`Self::refused_entries`] so a mount can log what it declined rather than differing silently
    /// from its own manifest.
    ///
    /// Production code goes through `real_catalog_table` instead, so a fake can never be mounted
    /// by accident: this constructor takes closures, and the only closures naming real providers
    /// live there.
    pub fn new(entries: Vec<(String, CatalogFn)>) -> Self {
        let (kept, refused): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|(v, _)| catalog_availability(v).is_public());
        for (venue, _) in &refused {
            tracing::error!(
                venue = %venue,
                "vike-datahub catalog: REFUSING a provider entry — `vike_catalog::\
                 catalog_availability` does not classify this venue as publicly enumerable, and a \
                 server will not spend the operator's venue credentials on a client's request \
                 (docs/decisions/0062, decision 3). The venue will answer NeedsCredentials or \
                 NotServed; nothing was mounted for it."
            );
        }
        Self { entries: kept }
    }

    /// The venues this table can list, in declared order — the `NotServed` refusal's "supported"
    /// set, and the `Welcome` advertisement's evidence.
    pub fn supported(&self) -> Vec<&str> {
        self.entries.iter().map(|(venue, _)| venue.as_str()).collect()
    }

    /// The provider for `venue`, or `None` (→ a `NotServed` naming [`Self::supported`]).
    pub fn get(&self, venue: &str) -> Option<&CatalogFn> {
        self.entries.iter().find(|(v, _)| v == venue).map(|(_, f)| f)
    }

    /// Whether [`Self::new`] dropped anything — for the mount's own log line. Always `false` for
    /// `real_catalog_table`, whose entries are all public by construction; non-empty only where a
    /// caller tried to mount something decision 3 forbids.
    pub fn refused_entries(entries: &[(String, CatalogFn)]) -> Vec<String> {
        entries
            .iter()
            .filter(|(v, _)| !catalog_availability(v).is_public())
            .map(|(v, _)| v.clone())
            .collect()
    }
}

impl std::fmt::Debug for CatalogTable {
    /// Venue names only — the closures have nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogTable").field("supported", &self.supported()).finish()
    }
}

/// The PRODUCTION table: **eight** venue catalogs, each answered by the gatewayhub.
///
/// ⚠ **No bridge is named here, and none is linked for this lane's sake.** The roster is
/// `vike_gatewayhub_client::catalog::CATALOG_VENUES` — binance, bybit, okx, aster, hyperliquid,
/// polymarket, dukascopy, fxcm, in the order the in-process table listed them, which is observable
/// (it is what a `NotServed` refusal prints) — and every entry opens a `RemoteCatalog` on `link` for
/// each refresh the lane admits. The providers themselves are `vike-gatewayhub`'s
/// (`crates/vike-gatewayhub/src/rest/catalog.rs`'s `CatalogProviders`, which holds its own table equal
/// to that constant); this function adds nothing to what a venue may be asked, and
/// [`CatalogTable::new`] still drops any entry `vike_catalog::catalog_availability` does not call
/// public, so decision 0062's rule 3 is held on both sides of the hop.
///
/// ⚠ **Six of the fourteen roster venues are absent, in three classes**, and only one of them is a
/// scope question (0062's consequences enumerate all three): alpaca / oanda / ctrader are
/// credentialed and refused by decision 3; ig / ibkr have no bulk list at any price; and deribit is
/// publicly enumerable but is the ONE venue the DESKTOP still routes `Direct`
/// (`vike_catalog::catalog_source_for`), so it is left out deliberately rather than pending — a
/// separate finding this move does not touch.
///
/// ⚠ **`hyperliquid_hip3` does NOT reach the daemon, and it is the one gap of the move.** It is the
/// process's resolved `flags.hyperliquid_hip3` (decision 0095), which used to be handed to
/// `HyperliquidCatalog::with_hip3` so the picker's universe and the mount's symbology agreed about
/// HIP-3 builder-dex markets. The catalog verbs of the wave-1 wire carry no such field, so the
/// daemon lists hyperliquid under ITS OWN resolved flag. On the CI box both daemons read one project
/// folder and the two agree; a box whose datahub and gatewayhub read different folders can
/// disagree, so a flag that is ON here is announced once, at build, rather than silently dropped.
///
/// A new venue's entry is not added here: the `new-venue` note on the daemon's
/// `CatalogProviders::real` is the authority, and this table follows `CATALOG_VENUES`.
#[cfg(feature = "catalog-serve")]
pub fn real_catalog_table(link: &GatewayLink, hyperliquid_hip3: bool) -> CatalogTable {
    if hyperliquid_hip3 {
        tracing::info!(
            "vike-datahub catalog: flags.hyperliquid_hip3 is ON in this process, but the venue \
             catalog is listed by the gatewayhub at {}, which resolves ITS OWN \
             flags.hyperliquid_hip3: the HIP-3 universe the picker sees is that daemon's (the \
             catalog verbs carry no per-request flag)",
            link.addr()
        );
    }
    CatalogTable::new(
        CATALOG_VENUES
            .into_iter()
            .map(|venue| {
                let link = link.clone();
                let entry: CatalogFn = Box::new(move || {
                    let remote =
                        RemoteCatalog::open(link.clone(), venue).map_err(|e| e.to_string())?;
                    list_via(&remote)
                });
                (venue.to_string(), entry)
            })
            .collect(),
    )
}

/// One provider's `list_instruments`, with its `CatalogError` flattened to the wire's string.
///
/// ⚠ It also applies `vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS` — the cap is enforced
/// HERE, at the provider seam,
/// rather than at the response door, so every entry pays it identically and a future entry cannot
/// forget to. The provider is a `RemoteCatalog` now and the daemon sends its FULL answer (it is
/// paged), so the cap is applied to exactly the list the in-process provider returned.
#[cfg(feature = "catalog-serve")]
fn list_via(p: &dyn vike_catalog::CatalogProvider) -> Result<Vec<Instrument>, String> {
    let mut list = p.list_instruments().map_err(|e| e.to_string())?;
    list.truncate(vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS);
    Ok(list)
}

/// What [`CatalogLane::admit`] decided about one request, before any venue is touched.
/// ⚠ `PartialEq` but NOT `Eq`: [`Self::Cached`] carries `vike_model::Instrument`s, whose
/// `SymbolProperties` tick/lot fields are `f64`. Every comparison the tests need is against a
/// literal variant, which `PartialEq` answers.
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogAdmission {
    /// Fetch it. A token was spent.
    Fetch,
    /// A fresh memo answered. No token spent, no venue call.
    ///
    /// ⚠ **This arm is what proves "one fetch per TTL" server-side**, independently of whether the
    /// client caches. Two legs, neither substituting for the other: the client keeps its own
    /// `CatalogCache`, and a client that ignores it still costs the venue nothing.
    Cached(Vec<Instrument>),
    /// Refused, with the operator-facing reason. A `Response::Error`, because unlike an unarmed
    /// lane this IS a request the server declined rather than a configuration it is reporting.
    Refused(String),
}

/// The armed venue-catalog lane: the per-venue buckets and the TTL memo.
///
/// Built by `crate::datahub_cli` whenever `vike_catalog::venue_catalog_gate` says the lane serves,
/// so its mere EXISTENCE is the arming — there is no `enabled: bool` to get out of step with the
/// advertisement. `crate::server`'s `served_features` keys `FEATURE_VENUE_CATALOG` on
/// `Option::is_some` for exactly that reason, the same "advertised per mounted thing" rule
/// `FEATURE_BACKFILL` and `FEATURE_SEED_SERIES` already follow.
///
/// ⚠ **That gate DEFAULTS TO SERVING since `docs/decisions/0066`, where this doc used to say "only
/// when the operator set `VIKE_DATAHUB_VENUE_CATALOG=1`".** The switch is now the refusal —
/// `vike_config::Flags::venue_catalog_off` — and `None` here means the operator WROTE that refusal
/// rather than that they never asked. Nothing about the lane's own bounds moved with the default:
/// the per-venue buckets, the TTL and the bridges' own pagers are what price the cost, and they are
/// the same constants they were.
///
/// ⚠ **The lane OWNS the table**, rather than the two being threaded separately, and that is not
/// tidiness: the arming and the provider set must be ONE thing at the request door, because
/// `served_features` advertises on the lane's existence and a lane advertising a capability it has
/// no providers for would be the "can serve" / "will serve" split this crate's other two
/// advertisements exist to avoid. A build without `catalog-serve` gets a lane with an EMPTY table,
/// and every venue then answers a `NotServed` naming an empty supported set — which is honest, and
/// is what the operator needs to see to learn the build is wrong.
#[derive(Debug)]
pub struct CatalogLane {
    table: CatalogTable,
    /// How long a fetched list answers from memory: [`CATALOG_TTL`], or the box's row
    /// ([`catalog_ttl`]). Fixed at construction, like the table.
    ttl: Duration,
    state: Mutex<LaneState>,
}

#[derive(Debug)]
struct LaneState {
    /// venue -> bucket. One entry per venue actually asked for, so an unused venue costs nothing.
    buckets: HashMap<String, Bucket>,
    /// venue -> (fetched_at, instruments). **One entry per VENUE, never per request** — which is
    /// what keeps this residue bounded by the table's length rather than by client behaviour.
    memo: HashMap<String, (Instant, Vec<Instrument>)>,
}

impl CatalogLane {
    /// A lane over `table` that memoizes a fetched list for `ttl` — [`CATALOG_TTL`] unless the
    /// box wrote the `config.datahub_catalog_ttl_secs` row (see [`catalog_ttl`]). The TTL is a
    /// PARAMETER because this module reads no settings; `ttl` is the one term of the lane's cost
    /// an operator may move, and only within the range `vike-config` refuses outside of.
    pub fn new(table: CatalogTable, ttl: Duration) -> Self {
        Self {
            table,
            ttl,
            state: Mutex::new(LaneState { buckets: HashMap::new(), memo: HashMap::new() }),
        }
    }

    /// The memo's TTL this lane was built with.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The provider table this lane dispatches through — the `NotServed` refusal's supported set,
    /// and what the mount logs at startup.
    pub fn table(&self) -> &CatalogTable {
        &self.table
    }

    /// Decide one request against the lane's own bounds. `now` is injected so the refill and the
    /// TTL are testable without sleeping.
    ///
    /// Order matters and is cheapest-and-most-specific first: the MEMO before the bucket, so a
    /// fresh answer is free rather than spending a token it does not need; and the bucket last,
    /// because it is the only check with a side effect a refusal should not have paid for.
    pub fn admit(&self, venue: &str, now: Instant) -> CatalogAdmission {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, list)) = st.memo.get(venue)
            && now.saturating_duration_since(*at) < self.ttl
        {
            return CatalogAdmission::Cached(list.clone());
        }
        let bucket = st
            .buckets
            .entry(venue.to_string())
            .or_insert_with(|| Bucket::fresh(now, CATALOG_VENUE_BURST, CATALOG_VENUE_REFILL));
        if !bucket.take(now) {
            return CatalogAdmission::Refused(format!(
                "catalog: the `{venue}` fetch budget is spent — this lane admits \
                 {CATALOG_VENUE_BURST} back-to-back and then one per {} s, and that rate is what \
                 keeps it a negligible share of the venue-API budget this box shares with its \
                 order-signing daemon. Nothing was fetched; try this venue again shortly.",
                CATALOG_VENUE_REFILL.as_secs()
            ));
        }
        CatalogAdmission::Fetch
    }

    /// Record a freshly fetched list so the next ask inside the lane's TTL ([`Self::ttl`]) is free.
    ///
    /// ⚠ Called ONLY after a successful fetch. A failed fetch deliberately memoizes nothing: a
    /// venue that is briefly down must be retryable inside the TTL, and caching the failure would
    /// turn a blip into six hours of refusal.
    pub fn remember(&self, venue: &str, instruments: Vec<Instrument>, now: Instant) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.memo.insert(venue.to_string(), (now, instruments));
    }

    /// Venues currently memoized — for the startup/diagnostic line, never for a decision.
    pub fn cached_venues(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).memo.len()
    }
}

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
