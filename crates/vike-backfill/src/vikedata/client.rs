//! `data.vike.io` cohort-metrics transport: the wire vocabulary (which ladder, which grading), the
//! page URL, and ONE authenticated GET. No paging here — the cursor WALK is
//! [`crate::vikedata::ingest`]'s, exactly as `crates/vike-backfill/src/tardis/client.rs` fetches one
//! day and `crates/vike-backfill/src/tardis/ingest.rs` iterates them.
//!
//! Auth is an `X-API-KEY` header — this vendor has no `Authorization` spelling at all, which is why
//! `crates/vike-backfill/src/http.rs`'s `GetOptions` grew a `headers` escape hatch rather than this
//! module growing a second `ureq` agent (`deny.toml`'s `[bans].deny` is what makes "reuse the one
//! stack" a merge gate rather than a preference).
//!
//! ⚠ The key is a PARAMETER on every function here. This is a library: it reads no environment and
//! opens no credential store, and `crates/vike-backfill/src/bin/vikedata_backfill.rs` is the one
//! place `VIKE_API_KEY` / `VIKE_API_BASE` are looked up, out of a single `std::env::vars()` sweep —
//! the same shape `crates/vike-backfill/src/bin/tardis_backfill.rs` and
//! `crates/vike-backfill/src/bin/databento_backfill.rs` already use.

use crate::error::CollectError;

/// The default API root. `--api-base` / `VIKE_API_BASE` override it; a trailing slash is trimmed by
/// [`cohort_metrics_url`], so both spellings of a base work.
pub const VIKEDATA_BASE: &str = "https://data.vike.io/v1";

/// The auth header this vendor uses (the archive lane spells the same header
/// `crates/vike-backfill/src/events_api.rs`'s `API_KEY_HEADER`, cased `X-API-Key`; the metrics API
/// accepts either casing — HTTP header names are case-insensitive — and this is the casing
/// `crates/vike-research/src/sources/api.rs`'s `CohortClient` had been sending in production
/// before this module took the fetch over; see the module doc on that file's citations).
pub const API_KEY_HEADER: &str = "X-API-KEY";

/// Vendor prefix for error messages (the sibling ingest modules' `CTX` convention).
pub const CTX: &str = "vikedata";

/// The paging ceiling. BOUNDED rather than a `loop`: a cursor bug would otherwise spin against a
/// METERED endpoint until the budget is gone. Same value as its predecessor
/// `crates/vike-research/src/sources/api.rs`'s `MAX_PAGES`.
pub const MAX_PAGES: usize = 20;

/// Hours requested per page. Same value as its predecessor
/// `crates/vike-research/src/sources/api.rs`'s
/// `HOURS_PER_PAGE` — one page covers ~83 days of a single-label ladder, so a trailing 60-day read
/// is a small number of requests and the walk's window break (see
/// [`crate::vikedata::ingest::WalkStop`]) is what keeps an ANCHORED one that small too.
pub const HOURS_PER_PAGE: u32 = 2_000;

/// Which cohort ladder to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Account-value rungs (`4xWhale` … `Shrimp`).
    Size,
    /// Realized-PnL percentile rungs (`3xSmart` … `3xRekt`), plus the unrankable bucket. The ONLY
    /// axis that sends `labelBasis` and the only one whose basis guard runs — see
    /// [`crate::vikedata::parse::guard_label_basis`].
    Pnl,
    /// Position-notional buckets (`above_2_5m` … `below_1k`). Server-side the tier label is ALWAYS
    /// point-in-time: omitting `labelBasis` and sending `point_in_time` are both accepted while
    /// `labelBasis=current` is a 400, so this client OMITS it — the always-valid spelling and the
    /// same request shape as [`Axis::Size`].
    Tier,
}

impl Axis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Pnl => "pnl",
            Self::Tier => "tier",
        }
    }

    /// Parse the CLI/wire spelling. `None` for anything else — a caller that guessed gets an error
    /// naming the three, never a silent fall-through to `size`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "size" => Some(Self::Size),
            "pnl" => Some(Self::Pnl),
            "tier" => Some(Self::Tier),
            _ => None,
        }
    }
}

/// Which PnL grading the pnl axis is read with.
///
/// ⚠ **The name says what is RANKED; it cannot say when the label was computed.** That is
/// `labelBasis`'s question, answered separately, and [`Grading::Realized`] in particular is honest
/// or lookahead depending on it alone — which is why this client always sends
/// `labelBasis=point_in_time` on the pnl axis and hard-fails a response that does not echo it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grading {
    /// The percentile ladder over cumulative REALIZED PnL — the server's default, so it is sent as
    /// NO parameter at all and a realized request stays byte-identical to what it was before this
    /// parameter existed.
    #[default]
    Realized,
    /// The same ladder graded point-in-time across the whole window. ⚠ A FROZEN artefact upstream:
    /// a window reaching the present comes back SHORT rather than wrong.
    RealizedPit,
    /// Hour H's rank by the unrealized PnL of a wallet's open positions. ⚠ It grades POSITIONING,
    /// not skill.
    Unrealized,
}

impl Grading {
    /// The wire spelling. [`Self::Realized`] is `None`: sending it would change the request bytes
    /// of every run recorded before the parameter existed.
    pub fn param(self) -> Option<&'static str> {
        match self {
            Self::Realized => None,
            Self::RealizedPit => Some("realized-pit"),
            Self::Unrealized => Some("unrealized"),
        }
    }

    /// What the response's `grading` field must say. Never `None` — the server echoes the RESOLVED
    /// grading, so even a default request comes back named.
    pub fn echoed(self) -> &'static str {
        match self {
            Self::Realized => "realized",
            Self::RealizedPit => "realized-pit",
            Self::Unrealized => "unrealized",
        }
    }

    /// Parse the CLI spelling — the `echoed` strings, so an operator types what a stored row says.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "realized" => Some(Self::Realized),
            "realized-pit" => Some(Self::RealizedPit),
            "unrealized" => Some(Self::Unrealized),
            _ => None,
        }
    }
}

/// A grading is a property of the PNL ladder alone: size and tier rank no PnL.
///
/// ⚠ **This REFUSES where the predecessor `crates/vike-research/src/sources/api.rs`'s
/// `fetch_axis_graded` merely
/// declined to send the parameter**, and the difference matters here in a way it did not there.
/// That client dropped the parameter for a non-pnl axis, so a `--grading unrealized --axis size`
/// request silently read the realized ladder; a study then reported a number for the wrong
/// question. This collector would do worse: `grading` is a stored COLUMN and a commit-key segment
/// (`crates/vike-data/src/cohort_rec.rs`'s `CohortFetch`), so the same slip writes rows that NAME a
/// grading the wire never served — and the key that names it is spent, so the honest fetch of that
/// window becomes a silent no-op forever after. Refusing costs one retyped flag.
pub fn check_grading_applies(axis: Axis, grading: Grading) -> Result<(), CollectError> {
    if axis != Axis::Pnl && grading != Grading::Realized {
        return Err(CollectError::Fetch(format!(
            "{CTX}: grading={} is a property of the pnl ladder — the {} axis ranks no PnL, so this \
             request would be served as `realized` while every stored row (and the commit key) \
             claimed otherwise. Drop --grading, or read --axis pnl.",
            grading.echoed(),
            axis.as_str()
        )));
    }
    Ok(())
}

/// `{base}/{exchange}/coins/{asset}/cohort-metrics` — the endpoint, without a query.
///
/// A trailing slash on `base` is trimmed so `https://data.vike.io/v1` and `…/v1/` both work.
pub fn cohort_metrics_url(base: &str, exchange: &str, asset: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/{exchange}/coins/{asset}/cohort-metrics")
}

/// The full page URL for ONE request of the cursor walk.
///
/// The query is pinned by a test rather than described, because every rule that matters about this
/// vendor is a rule about what goes ON THE WIRE:
///
/// * `start` is ALWAYS sent and ALWAYS hour-floored (the caller floors it —
///   [`crate::vikedata::floor_to_hour`]). The no-`start` fallback was once a flat 7 days regardless
///   of `hours`, returning 167 hours with `nextCursor: null` — a 94%-incomplete series that
///   reported itself complete.
/// * `cursor` is always present, EMPTY on the first request — the shape
///   its predecessor `crates/vike-research/src/sources/api.rs`'s `fetch_axis_at` sent, kept so the
///   request bytes match what production has been issuing.
/// * `labelBasis=point_in_time` rides the PNL axis only (see [`Axis::Tier`] for why not tier).
/// * `grading` is omitted for [`Grading::Realized`] (see [`Grading::param`]).
pub fn page_url(
    base: &str,
    exchange: &str,
    asset: &str,
    axis: Axis,
    grading: Grading,
    start_secs: i64,
    cursor: &str,
) -> String {
    let mut url = cohort_metrics_url(base, exchange, asset);
    url.push_str("?axis=");
    url.push_str(axis.as_str());
    url.push_str("&hours=");
    url.push_str(&HOURS_PER_PAGE.to_string());
    url.push_str("&start=");
    url.push_str(&encode_query_value(&fmt_start(start_secs)));
    url.push_str("&cursor=");
    url.push_str(&encode_query_value(cursor));
    if axis == Axis::Pnl {
        url.push_str("&labelBasis=point_in_time");
        if let Some(g) = grading.param() {
            url.push_str("&grading=");
            url.push_str(&encode_query_value(g));
        }
    }
    url
}

/// One authenticated GET of one page. Returns the raw JSON body — decoding is
/// [`crate::vikedata::parse::parse_page`]'s, so the transport can be read without the schema and
/// the schema tested without a socket.
///
/// A non-2xx is an `Err` carrying the status and the body head (this vendor puts the reason there).
/// ⚠ Unlike `crates/vike-backfill/src/tardis/client.rs`'s `fetch_day`, a 404 is NOT `Ok(None)`:
/// a missing DAY-file is an ordinary hole in a per-day archive, while a 404 here means the
/// asset/exchange path does not exist at all, and skipping it would report an empty backfill as a
/// successful one.
///
/// Every argument is a separate parameter rather than a request struct because each one is a
/// separate rule this vendor has — the allow below is the price of keeping them named at the call
/// site, where `crates/vike-backfill/src/vikedata/ingest.rs`'s `fetch_cohort` closes over them.
#[allow(clippy::too_many_arguments)]
pub fn fetch_page(
    api_key: &str,
    base: &str,
    exchange: &str,
    asset: &str,
    axis: Axis,
    grading: Grading,
    start_secs: i64,
    cursor: &str,
) -> Result<String, CollectError> {
    let url = page_url(base, exchange, asset, axis, grading, start_secs, cursor);
    let headers = [(API_KEY_HEADER, api_key), ("Accept", "application/json")];
    let opts = crate::http::GetOptions { headers: &headers, ..Default::default() };
    crate::http::get_to_string(&url, &opts, CTX)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a unix second — the `start` spelling this endpoint takes.
///
/// Chrono-free, on `vike_model::time::civil_from_days`: `crates/vike-model/src/time.rs` is this
/// workspace's one home for calendar math and `crates/vike-model/src/time.rs`'s `parse_date_label`
/// records that consolidating there is what dropped vike-backfill's own datetime dependency. A
/// second one is not re-added for six lines of formatting.
pub fn fmt_start(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = vike_model::time::civil_from_days(days);
    let (hh, mm, ss) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Percent-encode one query VALUE.
///
/// Deliberately conservative rather than clever: everything outside `A-Za-z0-9-._~:` is encoded.
/// `:` is kept because RFC 3986 admits it in a query and every `start` this client sends carries
/// two — encoding them would make a logged URL unreadable for no gain. The value that actually
/// needs this is the CURSOR, which is an opaque server string: it is round-tripped verbatim into
/// the next request today, and an `&` or `=` appearing in one would otherwise splice a parameter
/// into the query rather than being carried in it.
fn encode_query_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_page_of_a_size_read_sends_start_hours_and_an_empty_cursor() {
        let url = page_url(
            VIKEDATA_BASE,
            "hyperliquid",
            "BTC",
            Axis::Size,
            Grading::Realized,
            1_754_744_400,
            "",
        );
        assert_eq!(
            url,
            "https://data.vike.io/v1/hyperliquid/coins/BTC/cohort-metrics\
             ?axis=size&hours=2000&start=2025-08-09T13:00:00Z&cursor=",
            "the size axis sends NO labelBasis and NO grading"
        );
    }

    #[test]
    fn the_pnl_axis_always_sends_the_point_in_time_label_basis() {
        let url = page_url(
            VIKEDATA_BASE,
            "hyperliquid",
            "SOL",
            Axis::Pnl,
            Grading::Realized,
            1_754_744_400,
            "",
        );
        assert!(url.ends_with("&labelBasis=point_in_time"), "{url}");
        assert!(!url.contains("grading="), "the default grading is sent as NO parameter: {url}");
    }

    #[test]
    fn a_non_default_grading_rides_the_pnl_axis_only() {
        let pnl = page_url(
            VIKEDATA_BASE,
            "hyperliquid",
            "SOL",
            Axis::Pnl,
            Grading::Unrealized,
            1_754_744_400,
            "",
        );
        assert!(pnl.ends_with("&labelBasis=point_in_time&grading=unrealized"), "{pnl}");
        // ...and the tier axis sends neither, whatever it is handed — `check_grading_applies` is
        // what stops a caller getting here with one (the test below).
        let tier = page_url(
            VIKEDATA_BASE,
            "hyperliquid",
            "SOL",
            Axis::Tier,
            Grading::Unrealized,
            1_754_744_400,
            "",
        );
        assert!(!tier.contains("labelBasis") && !tier.contains("grading"), "{tier}");
    }

    #[test]
    fn a_grading_on_a_ladder_that_ranks_no_pnl_is_refused_rather_than_dropped() {
        // The strengthening over the ported client. A dropped parameter would write rows whose
        // `grading` column — and whose commit key — name a grading the wire never served.
        for axis in [Axis::Size, Axis::Tier] {
            for g in [Grading::RealizedPit, Grading::Unrealized] {
                let err = check_grading_applies(axis, g).unwrap_err().to_string();
                assert!(err.contains(g.echoed()), "{err}");
                assert!(err.contains(axis.as_str()), "{err}");
            }
            assert!(check_grading_applies(axis, Grading::Realized).is_ok());
        }
        for g in [Grading::Realized, Grading::RealizedPit, Grading::Unrealized] {
            assert!(check_grading_applies(Axis::Pnl, g).is_ok());
        }
    }

    #[test]
    fn a_cursor_is_percent_encoded_so_it_cannot_splice_a_parameter_into_the_query() {
        let url = page_url(
            VIKEDATA_BASE,
            "hyperliquid",
            "BTC",
            Axis::Size,
            Grading::Realized,
            0,
            "1000:5&axis=pnl",
        );
        assert!(url.ends_with("&cursor=1000:5%26axis%3Dpnl"), "{url}");
        assert_eq!(url.matches("axis=").count(), 1, "the spliced axis must not be a parameter");
    }

    #[test]
    fn a_base_url_works_with_or_without_its_trailing_slash() {
        assert_eq!(
            cohort_metrics_url("https://data.vike.io/v1/", "hyperliquid", "BTC"),
            cohort_metrics_url("https://data.vike.io/v1", "hyperliquid", "BTC")
        );
    }

    #[test]
    fn fmt_start_renders_the_endpoints_own_spelling() {
        assert_eq!(fmt_start(0), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_start(1_754_744_400), "2025-08-09T13:00:00Z");
        // A leap day, and a pre-epoch instant — `div_euclid`/`rem_euclid` keep the clock forward.
        assert_eq!(fmt_start(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(fmt_start(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn the_axis_and_grading_spellings_round_trip_and_reject_a_guess() {
        for a in [Axis::Size, Axis::Pnl, Axis::Tier] {
            assert_eq!(Axis::parse(a.as_str()), Some(a));
        }
        for g in [Grading::Realized, Grading::RealizedPit, Grading::Unrealized] {
            assert_eq!(Grading::parse(g.echoed()), Some(g));
        }
        assert_eq!(Axis::parse("SIZE"), None);
        assert_eq!(Grading::parse("realized-PIT"), None);
        assert_eq!(Grading::parse(""), None);
    }
}
