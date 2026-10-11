//! The `SeedSeries` verb family: the CHART-GAP SEED — one bounded window of klines for a series a
//! chart cannot paint — and the class re-check that guards its door. A WRITE served to an OBSERVE
//! connection, which `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` argues and every
//! gate below is a term of; `handle_request` (the parent module) routes the request here with the
//! mounted collector table, the armed lane and the served store.

use super::*;

/// **Gate 2b's whole body: a class claim this server cannot HONOUR is refused by name.**
/// `Some(message)` is the refusal; `None` means the claim is one the route will obey.
///
/// The third leg of `vike_datahub_client::FEATURE_SEED_CLASS`, and the only one a client
/// cannot skip. Two refusals, and they answer different questions:
///
/// **(a) UNADDRESSABLE** — `vike_catalog::addressing_for(venue)` says this venue's data path cannot
/// address that class at all (`Option` at bybit, `Equity` anywhere in crypto, anything at all at
/// `fxcm`, anything at an unknown venue, whose row REFUSES by construction). The same table, and
/// the same question, `crates/bridges/bybit/src/data.rs`'s and
/// `crates/bridges/binance/src/data.rs`'s `route_target` each ask as their own first arm — asked
/// here too because this door is reached by an OBSERVE client and those two are not on the path
/// (see `seed_series_verb`'s ⚠ on the four class-less seams below this one).
///
/// **(b) UNHONOURABLE ON THIS SPELLING** — the claim is addressable but would have to CHANGE the
/// route, and nothing between this door and the collector carries it. At a
/// `vike_catalog::Naming::PerpSuffix` venue the workspace's own spelling IS the claim: a bare
/// symbol names the spot listing and a `vike_catalog::PERP_SUFFIX` one names the perpetual. So a
/// `CryptoPerp` claim on a bare symbol, or a spot claim on a suffixed one, is a request this server
/// would answer from the OTHER book while reporting rows written — 0061's measured bug, one layer
/// below the one the capability string closes. Refusing it names the spelling that works, which is
/// an ACT rather than a complaint.
///
/// ⚠ **(b) is the load-bearing half and it is also the one a reader will want to relax.** It looks
/// redundant beside the bridges' own `contradicting_claim_refusal`, and it is not: those refuse a
/// claim that contradicts the suffix, while this refuses a claim the suffix does not ALREADY make.
/// A bare `BTCUSDT` claimed `CryptoPerp` passes binance's `route_target` happily — it is exactly
/// how that function reaches `fapi` — and would be correct the moment the class reaches the bridge.
/// Until it does, honouring it here would write the perpetual tape under the SPOT series key, since
/// `SeedLane::admit`'s ledger and the `store.load_bars` read-back below both key on
/// `(venue, symbol, interval)` and 0061's store-key verdict keeps that key the SYMBOL. **So this
/// refusal is what lets the ledger key stay a triple**, and relaxing it without threading the class
/// to the collector re-opens both defects at once.
///
/// ⚠ **What it does NOT catch, declared rather than implied.** At a `Naming::VenueNative` venue the
/// symbol already names its own product, so any addressable class passes (b) — including a WRONG
/// one. `BTC-USDT-SWAP` claimed as `AssetClass::Option` is addressable at okx (its row carries
/// `Option`) and is nonsense, and this door cannot see that without importing okx's symbology,
/// which `vike-catalog`'s addressing table deliberately is not. The claim is inert there — it
/// changes no route and writes no row differently — so the residual is a claim that is ignored
/// rather than a book that is wrong. Closing it is the same work as (b)'s relaxation: thread the
/// class to the bridge, where the venue's own parser can answer.
fn refuse_unhonourable_class(
    venue: &str,
    symbol: &str,
    class: vike_model::AssetClass,
) -> Option<String> {
    let row = vike_catalog::addressing_for(venue);
    // (a)
    if !row.addresses(class) {
        return Some(format!(
            "venue `{venue}`'s kline path addresses {:?}, and {class:?} is not one of them. \
             Nothing was fetched. An unknown venue addresses NOTHING here by construction — \
             `vike_catalog::addressing_for`'s fallback refuses rather than answering permissive, \
             which is the whole of docs/decisions/0061's Phase 1.",
            row.classes
        ));
    }
    // (b) — only a venue whose SPELLING carries the product can have a claim that contradicts it.
    // A `VenueNative` or `NoDerivative` venue's symbol already names its own book, so an addressable
    // claim there is inert rather than unhonourable (see this function's last ⚠).
    if !matches!(row.naming, vike_catalog::Naming::PerpSuffix) {
        return None;
    }
    let (_, suffixed) = vike_catalog::split_perp(symbol);
    let wants_perp =
        matches!(class, vike_model::AssetClass::CryptoPerp | vike_model::AssetClass::CryptoFuture);
    if wants_perp == suffixed {
        return None;
    }
    Some(if wants_perp {
        format!(
            "a {class:?} claim on `{venue}` needs the `{}` spelling, and this symbol carries none. \
             Nothing was fetched and nothing was written. At this venue the SPELLING is the claim: \
             a bare symbol names the spot listing, and that is the series key a seed writes under \
             and the one a chart then reads back. Honouring the claim on a bare symbol would store \
             the perpetual's tape under the spot series — the wrong-book defect \
             docs/decisions/0061 exists to close. Ask for the same symbol with `{}` appended.",
            vike_catalog::PERP_SUFFIX,
            vike_catalog::PERP_SUFFIX
        )
    } else {
        format!(
            "this symbol carries `{}` — which says PERPETUAL — and the caller claimed {class:?}. \
             Two claims that disagree, so neither is obeyed and nothing was fetched. Drop the \
             suffix or drop the claim. (The bridges' own `route_target` refuses the identical \
             pair; it is repeated at this door because an OBSERVE client reaches the door and not \
             the bridge.)",
            vike_catalog::PERP_SUFFIX
        )
    })
}

/// **The CHART-GAP SEED verb** — one bounded window of klines for a series a chart cannot paint.
///
/// ⚠ **This is a WRITE served to an OBSERVE connection**, against
/// `docs/decisions/0052`'s forward ruling, and
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` is the argument. Every line below
/// is a term of that argument rather than defensive tidying — read the record before relaxing one.
///
/// # The order of the gates, and why each sits where it does
///
/// 1. **ARMED?** The refusal that is about the SERVER rather than about the request comes first —
///    the `delete_series_verb` idiom. ⚠ **And it is not a refusal**: an unarmed lane answers a
///    SUCCESSFUL [`SeedDone`] with `armed: false`, having touched nothing. That is 0058's reach
///    property 3, the leg that makes an Observe classification honest rather than convenient: an
///    Observe connection's authority is to name the series its chart is open on, and whether that
///    becomes a write is a fact about the server's configuration. Returning an error here would
///    make the verb mean "do this", which is the thing it must not mean.
/// 2. **INTERVAL, then SYMBOL** — [`validate_seed_interval`] / [`validate_seed_symbol`], the SAME
///    functions the client predicts with. ⚠ **These two are a security boundary, not validation
///    hygiene.** `crates/bridges/binance/src/family/klines.rs`'s `klines_url` interpolates both
///    fields into a REST query string with no allowlist and no encoding (bybit and okx validate in
///    their own code tables; binance does not, and the per-venue interval table that would own this
///    is deferred). Until this verb, everything reaching that line came from an operator's argv or a
///    `VerbScope::Write` client. They run BEFORE the venue lookup so a bad interval is refused
///    identically whichever venue was named, and before the lane is consulted so a malformed request
///    cannot spend a token.
/// 3. **BUILD** — no mounted table is the `backfill_verb` refusal, verbatim in spirit: a lane armed
///    on a build with no collectors can serve nothing.
/// 4. **VENUE** — the server's own table, and its refusal names the supported set.
/// 5. **THE LANE** — [`SeedLane::admit`]: the ledger (a repeat is free, which is the SERVER-side leg
///    of "exactly one fetch per series"), the lifetime series cap, then the per-venue token bucket.
///    Last, because it is the only check with a side effect that a later refusal should not have
///    paid for.
///
/// Then the collector runs INLINE over a window the SERVER computed
/// ([`seed_range`]), exactly as `backfill_verb` runs one over a window the CLIENT named — same
/// table, same write-through, same read-back proof. The whole difference between the two verbs is
/// who chose that window, which is the whole difference between Control and Observe here.
///
/// # ⚠ Gate 2b — THE CLASS RE-CHECK, and why it is a gate rather than a routing input
///
/// `docs/decisions/0061` Phase 3 lets the request name what KIND of instrument `symbol` is, and
/// `vike_datahub_client::FEATURE_SEED_CLASS` states the three legs that field needs. This is
/// the third and the only one that holds against a client that skipped the other two —
/// [`refuse_unhonourable_class`] is the whole of it, and it is deliberately a REFUSAL rather than a
/// route.
///
/// **The class reaches no bridge from here, and that is this phase's declared boundary.** Between
/// this door and a venue's `fetch_klines_range_classed` sit four seams that carry no class:
/// [`BackfillFn`], `vike_backfill::kline_source::backfill_kline_source`, the
/// `vike_data::source::KlineSource::fetch` trait method and each collector's own bridge call. So the
/// only claim this server can honour is one the UNCLASSED route would already obey — and a claim it
/// cannot honour must be REFUSED, never
/// dropped, because dropping it is precisely the wrong-book answer wearing a success that the
/// capability's own doc calls "the measured bug reproduced by its own fix".
pub(super) fn seed_series_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    class: Option<vike_model::AssetClass>,
    table: Option<&BackfillTable>,
    lane: Option<&SeedLane>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    // Gate 1 — see the doc: a SUCCESS, not a refusal, and deliberately so.
    let Some(lane) = lane else {
        return Response::SeriesSeeded(SeedDone {
            armed: false,
            repeated: false,
            rows_written: 0,
            range: None,
            first_ts: None,
            last_ts: None,
        });
    };
    // Gates 2 — before the venue lookup and before a token can be spent.
    if let Err(why) = validate_seed_interval(interval) {
        return Response::Error(format!("seed: {why}"));
    }
    if let Err(why) = validate_seed_symbol(symbol) {
        return Response::Error(format!("seed: {why}"));
    }
    // Gate 2b — THE CLASS RE-CHECK. Beside its two siblings and for the same reason: before the
    // venue lookup, so the answer does not depend on which build this is, and before the lane, so a
    // claim this server cannot honour cannot spend a token or enter the ledger.
    if let Some(class) = class
        && let Some(why) = refuse_unhonourable_class(venue, symbol, class)
    {
        return Response::Error(format!("seed: {why}"));
    }
    // Gate 3.
    let Some(table) = table else {
        return Response::Error(format!(
            "seed: this build has no collectors — rebuild vike-datahub with              `--features backfill-serve`. The `{FEATURE_SEED_SERIES}` capability is deliberately              absent from this server's Welcome.features."
        ));
    };
    // Gate 4 — the KLINE lane only: a chart open never starts a tick download or a funding fetch.
    let Some(collector) = table.get_for_seed(venue) else {
        return Response::Error(format!(
            "seed: venue `{venue}` has no collector in this build. Supported: [{}]",
            table.seed_supported().join(", ")
        ));
    };
    let Some((start, end)) = seed_range(interval, vike_model::now_ms()) else {
        // Unreachable behind gate 2 — the two read the same set — and answered rather than
        // `unwrap`ped because this is the one place a divergence between them would land.
        return Response::Error(format!(
            "seed: interval {interval:?} passed the permitted set but has no bar width, which              means `vike_datahub_client::seed`'s SEED_INTERVALS and `vike_model::time::interval_ms`              have diverged. Nothing was fetched."
        ));
    };
    // Gate 5.
    let repeated = match lane.admit(venue, symbol, interval, Instant::now()) {
        SeedAdmission::Fetch => false,
        SeedAdmission::Repeated => true,
        SeedAdmission::Refused(why) => return Response::Error(why),
    };
    let rows_written = if repeated {
        0
    } else {
        // A stop probe that never fires: the seed is one bounded window on the keyless kline lane,
        // which has no chunk boundary to stop at — and `backfill_verb`'s probe is that verb's alone.
        match collector(symbol, interval, start, end, &|| false) {
            Ok(n) => n as u64,
            Err(e) => return Response::Error(format!("seed {venue}/{symbol}@{interval}: {e}")),
        }
    };
    // The write-through proof, through the SERVED handle — the same read the client's follow-up
    // `LoadBars` does, so a `SeedDone` reporting bars is a promise that read will find them.
    //
    // ⚠ This stays a `load_bars`, and that is NOT the defect `backfill_verb`'s read-back had: the
    // window here is `seed_range`'s — the last `SEED_BARS` bars ending now, chosen by THIS server —
    // so the read is bounded for as long as that constant is, where `backfill_verb`'s window is
    // whatever the client asked for and goes through `HistStore::bar_edges` for that reason. This
    // lane reports only the ends too, so it could move as well; it was left alone because a read
    // that cannot hurt is not worth changing under a reply that must stay byte-identical. If
    // `SEED_BARS` ever outgrows a chart's width, move it.
    match store.load_bars(venue, symbol, interval, TsRange { start: Some(start), end: Some(end) }) {
        Ok(bars) => Response::SeriesSeeded(SeedDone {
            armed: true,
            repeated,
            rows_written,
            range: Some((start, end)),
            first_ts: bars.first().map(|b| b.ts),
            last_ts: bars.last().map(|b| b.ts),
        }),
        Err(e) => Response::Error(format!(
            "seed {venue}/{symbol}@{interval}: wrote {rows_written} rows but the read-back              failed: {e}"
        )),
    }
}
