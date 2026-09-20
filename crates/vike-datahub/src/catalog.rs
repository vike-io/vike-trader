//! **The venue-catalog lane: the provider table, and the bounds only the SERVER can have.**
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` classifies
//! [`Request::VenueCatalog`](vike_datahub_client::proto::Request::VenueCatalog) as
//! `VerbScope::Observe`, and the ground is a question its predecessor did not have to ask: **is it a
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
//! | [`CATALOG_TTL`] | "a picker re-asks every time it opens" — proved server-side, not only by a well-behaved client |
//! | `vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS` | "one venue can return an unbounded frame" |
//! | [`CatalogLane`] existing at all | "the operator never consented" — an unarmed daemon builds none |
//!
//! # ⚠ The table's MEMBERSHIP is the load-bearing bound, not the bucket
//!
//! 0062's decision 3: **only a provider that reads a PUBLIC endpoint may be in this table.** Three
//! roster venues (alpaca, oanda, ctrader) can be listed only by authenticating as the operator, and
//! a rate limit bounds spending a public budget while nothing bounds spending an identity. So they
//! are excluded by CONSTRUCTION — [`real_catalog_table`] is the one function that names entries, and
//! [`CatalogTable::new`] refuses a venue `vike_catalog::catalog_availability` does not call public.
//! That refusal is in the constructor rather than at the request door deliberately: a door check
//! can be reordered past, and a table that cannot HOLD the venue has no ordering to get wrong.
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

use vike_catalog::{Instrument, catalog_availability};

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
pub const CATALOG_TTL: Duration = Duration::from_secs(6 * 60 * 60);

// The burst must be small enough that draining it cannot itself out-cost the steady state this lane
// advertises. Expressed as the inequality that reduces to: a full burst is `CATALOG_VENUE_BURST`
// fetches, and it must not exceed what the TTL would permit in a whole day. The `crate::seed` /
// `crate::md` idiom — an inequality, so a deliberate tweak compiles and a broken claim does not.
const _: () = assert!(
    CATALOG_VENUE_BURST as u64 * 4 <= CATALOG_TTL.as_secs() / 60,
    "CATALOG_VENUE_BURST is large enough relative to CATALOG_TTL that a single burst out-costs the \
     lane's own advertised daily steady state. Re-run `docs/decisions/0062`'s reservation \
     arithmetic, do not widen this assertion."
);

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
    /// Production code goes through [`real_catalog_table`] instead, so a fake can never be mounted
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
    /// [`real_catalog_table`], whose entries are all public by construction; non-empty only where a
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

/// The PRODUCTION table: **eight** venue catalogs, each reading a PUBLIC endpoint or no endpoint
/// at all.
///
/// ⚠ **Six of the eight add no dependency.** `crates/vike-datahub/Cargo.toml` already carries
/// binance/bybit/okx/aster/hyperliquid/polymarket as feeds-half (`default-features = false`)
/// optional edges for `live-feeds`, and every one of those bridges declares `pub mod catalog;`
/// UNGATED — so a feeds build already compiles these providers. This function names them; the
/// `catalog-serve` feature is what turns the names on.
///
/// ⚠ **dukascopy and fxcm ARE new edges, and they are the CHEAPEST entries in the table.** Both
/// providers are pure folds over a `const` table — `crates/bridges/dukascopy/src/catalog.rs`'s
/// `FX_TABLE` (49 rows) and `crates/bridges/fxcm/src/catalog.rs`'s `FX_TABLE` (41) — so a listing
/// costs **zero venue requests, zero credential reads and no network**, which is what
/// `vike_catalog::catalog_availability` records for both. fxcm's compiles no C++ either: its
/// `build.rs` returns immediately without `CARGO_FEATURE_FXCM`, which no path from this crate
/// enables. They were absent from the first cut of this table purely because this crate did not
/// depend on those two bridges.
///
/// ⚠ **Their SPELLING differs from the six and will not compile if copied.** Both bridges declare
/// `mod catalog;` PRIVATE and re-export at the crate root (`vike_dukascopy::DukascopyCatalog`,
/// `vike_fxcm::FxcmCatalog`), where the other six go through `pub mod catalog;`. The crate-root
/// path is the canonical public one for this pair; widening their modules to match would be a
/// second name for one symbol, which the root `CLAUDE.md`'s no-shim rule is about.
///
/// ⚠ **Six of the fourteen roster venues are absent, in three classes**, and only one of them is
/// a scope question (0062's consequences enumerate all three):
///
/// * **alpaca / oanda / ctrader** — credentialed, refused by decision 3. Admitting one is a
///   reopener, and [`CatalogTable::new`] would drop it anyway.
/// * **ig / ibkr** — no bulk list at any price. Nothing to add.
/// * **deribit** — publicly enumerable, and the ONE venue the DESKTOP still links and routes
///   `Direct` (`vike_catalog::catalog_source_for`). A server entry for it would be a dependency
///   nothing asks for, so it is left out deliberately rather than pending.
// vike:new-venue:note do NOT add a `("{venue}", Box::new(|| list_via(&…)))` entry here yet. An entry in THIS table is what makes a SERVER fetch that venue's universe on an untrusted Observe client's request, and it is only correct once two facts hold that a generator cannot see: `vike_catalog::catalog_availability` classifies `{venue}` as `PublicBulk` (the scaffolded row there is the fail-closed `NoBulkList`, and `CatalogTable::new` would DROP a non-public entry with a `tracing::error!` — a row added blind is dead weight that reads as coverage), and this crate declares an edge to that bridge under `catalog-serve` in `crates/vike-datahub/Cargo.toml`. Add the entry in the PR that classifies the venue, with the request COST measured and written into the availability row — `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` decision 3 is what a wrong entry breaks.
#[cfg(feature = "catalog-serve")]
pub fn real_catalog_table() -> CatalogTable {
    CatalogTable::new(vec![
        ("binance".to_string(), Box::new(|| list_via(&vike_binance::catalog::BinanceCatalog))),
        ("bybit".to_string(), Box::new(|| list_via(&vike_bybit::catalog::BybitCatalog))),
        ("okx".to_string(), Box::new(|| list_via(&vike_okx::catalog::OkxCatalog))),
        ("aster".to_string(), Box::new(|| list_via(&vike_aster::catalog::AsterCatalog))),
        (
            "hyperliquid".to_string(),
            // ⚠ The one entry here that is NOT a unit struct — `HyperliquidCatalog` holds state and
            // has a `new()`. Its four siblings are unit structs and polymarket's is too, which is
            // exactly why the `catalog-serve` lane exists: nothing else compiles this line.
            Box::new(|| list_via(&vike_hyperliquid::catalog::HyperliquidCatalog::new())),
        ),
        (
            "polymarket".to_string(),
            Box::new(|| list_via(&vike_polymarket::catalog::PolymarketCatalog)),
        ),
        // ⚠ The two BUNDLED-STATIC providers, and note the CRATE-ROOT path: these two bridges
        // declare `mod catalog;` private and re-export the type, so `vike_dukascopy::catalog::…`
        // does not resolve. See this function's doc.
        ("dukascopy".to_string(), Box::new(|| list_via(&vike_dukascopy::DukascopyCatalog))),
        ("fxcm".to_string(), Box::new(|| list_via(&vike_fxcm::FxcmCatalog))),
    ])
}

/// One provider's `list_instruments`, with its `CatalogError` flattened to the wire's string.
///
/// ⚠ It also applies `vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS` — the cap is enforced
/// HERE, at the provider seam,
/// rather than at the response door, so every entry pays it identically and a future entry cannot
/// forget to.
#[cfg(feature = "catalog-serve")]
fn list_via(p: &dyn vike_catalog::CatalogProvider) -> Result<Vec<Instrument>, String> {
    let mut list = p.list_instruments().map_err(|e| e.to_string())?;
    list.truncate(vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS);
    Ok(list)
}

/// What [`CatalogLane::admit`] decided about one request, before any venue is touched.
/// ⚠ `PartialEq` but NOT `Eq`: [`Self::Cached`] carries `vike_catalog::Instrument`s, whose
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

/// The per-venue token bucket, as a plain value so the refill arithmetic is testable without a
/// clock.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: u32,
    last_refill: Instant,
}

impl Bucket {
    fn fresh(now: Instant) -> Self {
        Self { tokens: CATALOG_VENUE_BURST, last_refill: now }
    }

    /// Fold elapsed time into tokens, then spend one if there is one. `false` = refused.
    ///
    /// Integer division on purpose, with `last_refill` advanced by the WHOLE periods consumed
    /// rather than to `now`: advancing to `now` would discard the remainder every call, so a caller
    /// polling faster than the refill period would never accrue a token at all — the classic bucket
    /// bug, and the reason this is a method with a test rather than two lines at the call site.
    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let periods = elapsed.as_millis() / CATALOG_VENUE_REFILL.as_millis();
        if periods > 0 {
            let periods = u32::try_from(periods).unwrap_or(u32::MAX);
            self.tokens = self.tokens.saturating_add(periods).min(CATALOG_VENUE_BURST);
            self.last_refill += CATALOG_VENUE_REFILL * periods;
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

impl CatalogLane {
    pub fn new(table: CatalogTable) -> Self {
        Self {
            table,
            state: Mutex::new(LaneState { buckets: HashMap::new(), memo: HashMap::new() }),
        }
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
            && now.saturating_duration_since(*at) < CATALOG_TTL
        {
            return CatalogAdmission::Cached(list.clone());
        }
        let bucket = st.buckets.entry(venue.to_string()).or_insert_with(|| Bucket::fresh(now));
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

    /// Record a freshly fetched list so the next ask inside [`CATALOG_TTL`] is free.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> CatalogLane {
        // A lane whose table is empty: these tests exercise the BUCKET and the MEMO, which are
        // independent of which providers are mounted.
        CatalogLane::new(CatalogTable::new(Vec::new()))
    }

    fn inst(venue: &str, sym: &str) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: vike_model::AssetClass::CryptoSpot,
            base: "BTC".into(),
            quote: "USDT".into(),
            description: String::new(),
            properties: Default::default(),
            // ⚠ `None` is the ORDINARY state for both — see `vike_catalog::Instrument::contract_type`.
            contract_type: None,
            settle_asset: None,
        }
    }

    fn fake(venue: &str) -> (String, CatalogFn) {
        let v = venue.to_string();
        (venue.to_string(), Box::new(move || Ok(vec![inst(&v, "BTCUSDT")])))
    }

    #[test]
    fn a_credentialed_venue_cannot_be_mounted_even_when_a_caller_asks_for_it() {
        // ⚠ THE MUTATION PROOF for 0062's decision 3, and it drives the PRODUCTION constructor
        // rather than a harness: `CatalogTable::new` is what `real_catalog_table` calls. A caller
        // handing it alpaca gets a table that does not serve alpaca — so the refusal cannot be
        // reordered past, because there is no order to get wrong.
        let t = CatalogTable::new(vec![
            fake("binance"),
            fake("alpaca"),
            fake("oanda"),
            fake("ctrader"),
        ]);
        assert_eq!(t.supported(), vec!["binance"]);
        for v in ["alpaca", "oanda", "ctrader"] {
            assert!(t.get(v).is_none(), "{v} must not be reachable through a mounted table");
        }
    }

    #[test]
    fn a_venue_with_no_bulk_list_cannot_be_mounted_either() {
        // ig/ibkr are not credentialed — they have no list at all — and the same constructor rule
        // keeps them out, so the server can never answer `Listed { instruments: [] }` for them by
        // having mounted a provider that returns empty.
        let t = CatalogTable::new(vec![fake("ig"), fake("ibkr"), fake("okx")]);
        assert_eq!(t.supported(), vec!["okx"]);
    }

    #[test]
    fn an_unknown_venue_cannot_be_mounted() {
        let t = CatalogTable::new(vec![fake("kraken"), fake("bybit")]);
        assert_eq!(t.supported(), vec!["bybit"]);
    }

    #[test]
    fn a_fresh_memo_answers_without_spending_a_token() {
        // The server-side leg of "one fetch per TTL, not one per picker open". A client that ignores
        // its own cache and asks a thousand times gets one fetch and 999 free answers, and — the
        // part that matters — does not drain the venue bucket doing it.
        let l = lane();
        let t0 = Instant::now();
        assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
        l.remember("binance", vec![inst("binance", "BTCUSDT")], t0);
        for _ in 0..1_000 {
            match l.admit("binance", t0) {
                CatalogAdmission::Cached(list) => assert_eq!(list.len(), 1),
                other => panic!("a fresh memo must answer, got {other:?}"),
            }
        }
        // ...and the bucket still holds its remaining burst: a DIFFERENT venue is unaffected and
        // binance itself still has one token left of two.
        assert_eq!(l.admit("okx", t0), CatalogAdmission::Fetch);
        assert_eq!(l.cached_venues(), 1);
    }

    #[test]
    fn the_memo_expires_at_the_ttl_and_the_venue_is_asked_again() {
        let l = lane();
        let t0 = Instant::now();
        assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
        l.remember("binance", vec![inst("binance", "BTCUSDT")], t0);
        // One millisecond before the TTL: still cached.
        let almost = t0 + CATALOG_TTL - Duration::from_millis(1);
        assert!(matches!(l.admit("binance", almost), CatalogAdmission::Cached(_)));
        // At the TTL: the memo is stale, and the bucket has long since refilled.
        assert_eq!(l.admit("binance", t0 + CATALOG_TTL), CatalogAdmission::Fetch);
    }

    #[test]
    fn a_failed_fetch_memoizes_nothing_so_a_blip_is_not_six_hours_of_refusal() {
        // `remember` is called only on success — asserted by NOT calling it, which is the shape the
        // server's verb takes.
        let l = lane();
        let t0 = Instant::now();
        assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
        assert_eq!(l.cached_venues(), 0, "a fetch that failed must leave no memo");
        // The second token is still there for the retry.
        assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    }

    #[test]
    fn the_burst_is_exhausted_then_refills_one_token_per_period() {
        let l = lane();
        let t0 = Instant::now();
        for _ in 0..CATALOG_VENUE_BURST {
            assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
        }
        match l.admit("binance", t0) {
            CatalogAdmission::Refused(why) => {
                assert!(why.contains("budget is spent"), "{why}");
                assert!(why.contains("order-signing daemon"), "the reason is named: {why}");
            }
            other => panic!("the next fetch must be refused, got {other:?}"),
        }
        let t1 = t0 + CATALOG_VENUE_REFILL;
        assert_eq!(l.admit("binance", t1), CatalogAdmission::Fetch);
        assert!(matches!(l.admit("binance", t1), CatalogAdmission::Refused(_)));
    }

    #[test]
    fn polling_faster_than_the_refill_still_accrues_tokens() {
        // The bucket bug this `take` is written against: advancing `last_refill` to `now` on every
        // call would discard the remainder, so a caller polling every 10 s against a 60 s period
        // would never accrue anything.
        let l = lane();
        let t0 = Instant::now();
        for _ in 0..CATALOG_VENUE_BURST {
            assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
        }
        let mut got = None;
        for s in (10..=70).step_by(10) {
            if l.admit("binance", t0 + Duration::from_secs(s)) == CatalogAdmission::Fetch {
                got = Some(s);
                break;
            }
        }
        assert_eq!(got, Some(CATALOG_VENUE_REFILL.as_secs()), "a token must accrue on schedule");
    }

    #[test]
    fn the_buckets_are_per_venue_so_the_heavy_venues_cannot_starve_binance() {
        // The first of `CATALOG_VENUE_REFILL`'s three "real work" properties, held rather than
        // asserted in prose: polymarket's 40-page walk spends polymarket's budget.
        let l = lane();
        let t0 = Instant::now();
        for _ in 0..CATALOG_VENUE_BURST {
            assert_eq!(l.admit("polymarket", t0), CatalogAdmission::Fetch);
        }
        assert!(matches!(l.admit("polymarket", t0), CatalogAdmission::Refused(_)));
        assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
        assert_eq!(l.admit("okx", t0), CatalogAdmission::Fetch);
    }

    #[test]
    fn the_memo_is_one_entry_per_venue_not_per_request() {
        // The declared residue bound: this state is bounded by the TABLE's length, whatever a
        // client does. A thousand asks over three venues leave three entries.
        let l = lane();
        let t0 = Instant::now();
        for v in ["binance", "okx", "bybit"] {
            l.remember(v, vec![inst(v, "X")], t0);
        }
        for _ in 0..1_000 {
            for v in ["binance", "okx", "bybit"] {
                let _ = l.admit(v, t0);
            }
        }
        assert_eq!(l.cached_venues(), 3, "the memo must not grow with request count");
    }
}
