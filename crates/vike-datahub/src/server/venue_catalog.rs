//! The `VenueCatalog` verb family: one venue's PUBLIC instrument list, answered from the mounted
//! catalog lane and never from the store. `handle_request` (the parent module) routes the request
//! here and passes the lane and nothing else — no `store` handle, which is the point of the verb
//! (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 1).

use super::*;

/// **Serve one venue's instrument list** — the [`Request::VenueCatalog`] handler.
///
/// ⚠ **It takes no `store` parameter, and that absence is the whole of
/// `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 1.**
/// Its two neighbours — `backfill_verb` and `seed_series_verb` — both write the served store and
/// both read it back; this one cannot, because it is handed no handle. That is why 0058's four-part
/// rule is inapplicable here rather than satisfied, and why the additive/idempotent leg of it is
/// declared VACUOUS in that record rather than claimed: there is no durable shared state for the
/// leg to grip.
///
/// # ⚠ Almost every outcome is a SUCCESS, which is the opposite of this file's usual shape
///
/// An unarmed lane, a credentialed venue, an un-enumerable venue and a venue this build does not
/// serve are all `Response::VenueCatalog` values. Only two things are [`Response::Error`]: a
/// MALFORMED venue string, and a provider that failed mid-fetch. The reason is that "which venues
/// can I refresh" is a question the Data Manager asks every time it opens, and a stream of errors
/// is the wrong shape for a routine answer — while an EMPTY LIST would be the wrong shape for a
/// refusal, because for `ig` and `ibkr` an empty list is the truthful answer to a different
/// question (0062's decision 5).
///
/// # The gate order, and why each step is where it is
///
/// 1. **SHAPE** ([`validate_catalog_venue`]) — the one genuinely exceptional input, refused before
///    anything is consulted.
/// 2. **ARMING** — a success, and deliberately before the venue's nature: an unarmed server should
///    say so about EVERY venue rather than leaking which ones it would have served.
/// 3. **THE VENUE'S OWN NATURE** (`vike_catalog::catalog_availability`) — consulted BEFORE the
///    mounted table, so `ig` is told it has no bulk list rather than that this build lacks a
///    provider. Both are true; only the first is useful, and only the first stays true after a
///    rebuild.
/// 4. **THE TABLE** — a `PublicBulk` venue with no mounted provider is a BUILD fact, and the
///    refusal names what is served.
/// 5. **THE LANE'S BOUNDS** — last, because the bucket is the only check with a side effect a
///    refusal should not have paid for. The memo is consulted inside `admit`, before the bucket, so
///    a fresh answer spends no token.
pub(super) fn venue_catalog_verb(venue: &str, lane: Option<&CatalogLane>) -> Response {
    // Gate 1.
    if let Err(why) = validate_catalog_venue(venue) {
        return Response::Error(format!("catalog: {why}"));
    }
    let listed =
        |outcome| Response::VenueCatalog(CatalogListing { venue: venue.to_string(), outcome });
    // Gate 2 — see the doc: a SUCCESS, not a refusal, and deliberately so.
    let Some(lane) = lane else {
        return listed(CatalogOutcome::NotArmed);
    };
    let supported = || lane.table().supported().iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Gate 3.
    match catalog_availability(venue) {
        CatalogAvailability::Credentialed => {
            return listed(CatalogOutcome::Refused(CatalogRefusal::NeedsCredentials));
        }
        CatalogAvailability::NoBulkList { why } => {
            return listed(CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: why.to_string(),
            }));
        }
        // A well-formed slug that names no roster venue. Reported as NOT SERVED rather than as a
        // venue property, because this server genuinely cannot serve it and saying anything about
        // its catalog would be inventing a fact about a venue that does not exist.
        CatalogAvailability::UnknownVenue => {
            return listed(CatalogOutcome::Refused(CatalogRefusal::NotServed {
                supported: supported(),
            }));
        }
        CatalogAvailability::PublicBulk => {}
    }
    // Gate 4.
    let Some(provider) = lane.table().get(venue) else {
        return listed(CatalogOutcome::Refused(CatalogRefusal::NotServed {
            supported: supported(),
        }));
    };
    // Gate 5.
    let now = Instant::now();
    match lane.admit(venue, now) {
        CatalogAdmission::Cached(instruments) => listed(CatalogOutcome::Listed {
            truncated: instruments.len() >= CATALOG_MAX_INSTRUMENTS,
            cached: true,
            instruments,
        }),
        CatalogAdmission::Refused(why) => Response::Error(why),
        CatalogAdmission::Fetch => match provider() {
            Ok(instruments) => {
                // ⚠ The cap is applied by the provider seam (`crate::catalog`'s `list_via`), so a
                // full-length list IS a truncated one. Computing the flag from the length rather
                // than carrying it through the memo keeps the cached and fresh arms answering
                // identically, which is the property `cached` exists to make visible.
                let truncated = instruments.len() >= CATALOG_MAX_INSTRUMENTS;
                lane.remember(venue, instruments.clone(), now);
                listed(CatalogOutcome::Listed { instruments, truncated, cached: false })
            }
            // ⚠ NOTHING is memoized on a failure — see `CatalogLane::remember`'s doc: caching a
            // blip would turn a venue being briefly down into six hours of refusal.
            Err(e) => Response::Error(format!("catalog {venue}: {e}")),
        },
    }
}
