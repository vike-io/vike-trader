//! **Share splits from Yahoo, confirmed by FMP, filled at the operator's refresh** (the owner's rulings of 2026-11-16:
//! "nobody will do it manually", then «Б»: Yahoo gives the LIST, FMP confirms the TYPE where the key allows it, the
//! ratio rule decides the rest; `docs/decisions/0289-a-backtest-split-adjusts-a-share-series-from-the-operators-split-rows.md`'s
//! amendment). No venue the bridges talk to reports a split, and a vendor's candles (Dukascopy's) are not adjusted.
//!
//! At `Request::RefreshInstrumentLists` (`vike-cli data catalog refresh [--venue V]`, the Data Manager's Refresh), once
//! the kept lists are refreshed, [`with_yahoo_splits`] takes every series the history store holds of each refreshed
//! venue, finds its kept row (`vike_catalog::kept_row_of_store_name`: `AAPL.CFD` is `AAPL.US/USD`) and, for a share or a
//! CFD on a share, maps it to its Yahoo ticker from the row's facts alone ([`yahoo_ticker`]), then through the
//! operator's rename rows of that venue (`vike_secrets::stock_split::read_ticker_renames`: `FB` is `META`; data, never
//! compiled in) for Yahoo and FMP alike. A `404` for a share-class form (`BRKB`) asks Yahoo's spelling (`BRK-B`) once
//! more ([`class_form`]) and nothing further; the report names each series asked under another ticker. Each ticker is
//! asked ONCE per refresh: Yahoo for its list (`vike_backfill::eod::yahoo::fetch_splits`, at the `yahoo` `venue_url` row),
//! and FMP (`/stable/splits`, the box's `FMP_API_KEY` read the tool feeds' way) ONLY when a listed split moves a stored
//! bar (it is dated after the series' first bar), FMP's free tier being small. Each Yahoo split is then
//! ([`judge`]):
//!
//! * FMP answered and lists that date as a split → applied, source [`YAHOO_FMP`];
//! * FMP answered and does not list it as a split → NOT applied ("FMP does not confirm this as a split");
//! * FMP refused (`402`: a premium symbol), failed, has no key, or was not asked → the ratio rule ([`ratio_rule`]):
//!   applied, source [`YAHOO`], only when the reduced fraction has both sides at most 100; else NOT applied ("looks
//!   like a spin-off": CMCSA's `1067:1000`).
//!
//! The verdict becomes the series' vendor rows (`vike_secrets::stock_split::replace_sourced_splits`): an operator's row
//! of the same date wins and is never written or removed; vendor rows the verdict no longer carries are removed. A
//! series with no confident ticker gets NO split facts and one report line naming it; a ticker Yahoo does not answer
//! leaves its rows as they were. The report rides each venue's refresh answer
//! (`vike_datahub_client::catalog::SplitsRefresh`), one line per split that moves a stored bar.
//!
//! ⚠ **FMP's key is in its URL**: it reaches no log line or error text. The transport's errors name no URL (the tool
//! feeds' `ToolFeedHttpUreq`), and every FMP error text passes `Secret::redact` before it leaves [`fmp`].
//!
//! ⚠ **No timer, no boot step and no other verb reach this module**: the refresh verb is its one caller
//! (`stock_splits_tests.rs`'s `no_timer_fills_a_split`). The backtest reads the rows unchanged (decision 0289).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use vike_data::HistoryStore;
use vike_datahub_client::catalog::SplitsRefresh;
use vike_datahub_client::proto::Response;
use vike_model::{Contract, Instrument, StockSplit};

/// The `source` of a split applied by the ratio rule, and the vendor whose `venue_url` row gives Yahoo's address.
const YAHOO: &str = "yahoo";

/// The `source` of a split Yahoo lists and FMP confirms.
const YAHOO_FMP: &str = "yahoo+fmp";

/// The largest side of a split's reduced fraction the ratio rule applies (`100:1`, `1:100`).
const RATIO_RULE_MAX_SIDE: u64 = 100;

/// **The listing exchanges a kept row may state, as the venues spell them, and Yahoo's suffix for each.** A US
/// exchange is the plain ticker (`""`); a non-US one only where Yahoo's suffix is that exchange's and the venue's name
/// for the share is Yahoo's base. Any other exchange maps to nothing: a vendor grammar, like a URL path, never a
/// symbol. `NASDAQ`..`OTC` are alpaca's `exchange` words and IBKR's US exchanges; the others are IBKR's codes.
const EXCHANGE_SUFFIXES: &[(&str, &str)] = &[
    ("NASDAQ", ""),
    ("NYSE", ""),
    ("ARCA", ""),
    ("NYSEARCA", ""),
    ("AMEX", ""),
    ("BATS", ""),
    ("ISLAND", ""),
    ("IEX", ""),
    ("OTC", ""),
    ("LSE", ".L"),
    ("IBIS", ".DE"),
    ("SBF", ".PA"),
    ("AEB", ".AS"),
    ("EBS", ".SW"),
    ("BVME", ".MI"),
    ("BM", ".MC"),
];

/// One ticker's Yahoo answer: its splits `(date, numerator, denominator)`, or why there are none to write.
pub(crate) type YahooAnswer = Result<Vec<(String, f64, f64)>, YahooRefusal>;

/// Why Yahoo gave a ticker no list, in its words (the `Display`). [`Vendors::yahoo_splits`] asks a share-class
/// ticker's other spelling after [`YahooRefusal::Unlisted`] alone, decided on the variant and never on the words
/// (decision 0301): `vike_backfill::eod::yahoo::YahooSplitsError::Unlisted` is where the `404` is read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum YahooRefusal {
    /// Yahoo answered `404`: it lists no such ticker. Only the Yahoo client of `backfill-serve` reads one.
    #[cfg_attr(not(feature = "backfill-serve"), allow(dead_code, clippy::allow_attributes))]
    Unlisted(String),
    /// Any other reason there is no list.
    Failed(String),
}

/// A reason that names no `404` (a build without the Yahoo client, an unreadable `venue_url` row).
impl From<String> for YahooRefusal {
    fn from(why: String) -> Self {
        YahooRefusal::Failed(why)
    }
}

impl std::fmt::Display for YahooRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YahooRefusal::Unlisted(why) | YahooRefusal::Failed(why) => f.write_str(why),
        }
    }
}

/// One ticker's FMP answer: the `(date, splitType)` it lists, or why it gave none (refused, failed, no key).
pub(crate) type FmpAnswer = Result<Vec<(String, String)>, String>;

/// **The two vendors one refresh asks, each ticker at most once** (the answers are kept for the whole refresh).
pub(crate) struct Vendors<'a> {
    yahoo: &'a mut dyn FnMut(&str) -> YahooAnswer,
    fmp: &'a mut dyn FnMut(&str) -> FmpAnswer,
    yahoo_answers: HashMap<String, YahooAnswer>,
    fmp_answers: HashMap<String, FmpAnswer>,
}

impl<'a> Vendors<'a> {
    pub(crate) fn new(
        yahoo: &'a mut dyn FnMut(&str) -> YahooAnswer,
        fmp: &'a mut dyn FnMut(&str) -> FmpAnswer,
    ) -> Self {
        Self { yahoo, fmp, yahoo_answers: HashMap::new(), fmp_answers: HashMap::new() }
    }

    /// Yahoo's answer for `ticker` and the ticker that gave it: a `404` for a share-class form ([`class_form`]:
    /// `BRKB`) asks Yahoo's spelling (`BRK-B`) once more, and nothing further.
    fn yahoo_splits(&mut self, ticker: &str) -> (String, YahooAnswer) {
        let Vendors { yahoo, yahoo_answers, .. } = self;
        let mut ask =
            |t: &str| yahoo_answers.entry(t.to_string()).or_insert_with(|| yahoo(t)).clone();
        let first = ask(ticker);
        let retry = match (&first, class_form(ticker)) {
            // Only Yahoo's `404` (no such ticker) asks the other spelling; any other error asks no more.
            (Err(YahooRefusal::Unlisted(why)), Some(hyphened)) => Some((why.clone(), hyphened)),
            _ => None,
        };
        let Some((why, hyphened)) = retry else { return (ticker.to_string(), first) };
        match ask(&hyphened) {
            Ok(listed) => (hyphened, Ok(listed)),
            Err(again) => (
                ticker.to_string(),
                Err(YahooRefusal::Failed(format!("{why}; {hyphened}: {again}"))),
            ),
        }
    }
}

/// `BRKB` as Yahoo spells a share class, `BRK-B`: at least two capitals, then the class letter `A`, `B` or `C`.
/// `None` for any other ticker.
fn class_form(ticker: &str) -> Option<String> {
    let base = ticker.strip_suffix(['A', 'B', 'C'])?;
    (base.len() >= 2 && base.bytes().all(|b| b.is_ascii_uppercase()))
        .then(|| format!("{base}-{}", &ticker[base.len()..]))
}

/// **Fill the splits of every refreshed venue's stored share series**, the report on each venue's row of `answer`. A
/// refresh that answered an error, or a datahub with no settings store or no project folder, is returned as it is.
pub(crate) fn with_yahoo_splits(
    answer: Response,
    store: &dyn HistoryStore,
    settings: Option<&vike_secrets::CredentialStore>,
    instruments: Option<&Path>,
) -> Response {
    let Response::InstrumentListsRefreshed(mut rows) = answer else { return answer };
    let location = settings.and_then(vike_secrets::CredentialStore::location);
    let (Some(location), Some(dir)) = (location, instruments) else {
        return Response::InstrumentListsRefreshed(rows);
    };
    // Each series' first bar: a split dated after it moves a stored bar.
    let held = match store.inventory() {
        Ok(series) => series.into_iter().filter(|(id, _)| !id.symbol.is_empty()).fold(
            BTreeMap::<String, BTreeMap<String, i64>>::new(),
            |mut by_venue, (id, coverage)| {
                let first =
                    by_venue.entry(id.venue).or_default().entry(id.symbol).or_insert(i64::MAX);
                *first = (*first).min(coverage.first_timestamp);
                by_venue
            },
        ),
        Err(e) => {
            let why = format!("the store would not list its series: {e}");
            for row in &mut rows {
                row.splits =
                    Some(SplitsRefresh { failed: vec![why.clone()], ..SplitsRefresh::default() });
            }
            return Response::InstrumentListsRefreshed(rows);
        }
    };
    let settings_dir = location.settings_dir();
    let (mut yahoo, mut fmp) = (yahoo(settings_dir), fmp(settings));
    let mut vendors = Vendors::new(&mut yahoo, &mut fmp);
    for row in &mut rows {
        let Some(series) = held.get(&row.venue) else { continue };
        let kept = vike_catalog::read_kept_list(dir, &row.venue)
            .map(|l| l.instruments)
            .unwrap_or_default();
        row.splits = refresh_venue(settings_dir, &row.venue, &kept, series, &mut vendors);
        if let Some(report) = &row.splits {
            tracing::info!(venue = %row.venue, "{}", report.lines().join("; "));
        }
    }
    Response::InstrumentListsRefreshed(rows)
}

/// **One venue's pass**: each of `series` (the store's name and its first bar, epoch ms) whose kept row (in `kept`) is
/// a share or a CFD on one is mapped to its Yahoo ticker, its splits listed and judged ([`judge`]), and its vendor rows
/// made the verdict. `None` when the venue holds no share series.
pub(crate) fn refresh_venue(
    settings_dir: &Path,
    venue: &str,
    kept: &[Instrument],
    series: &BTreeMap<String, i64>,
    vendors: &mut Vendors<'_>,
) -> Option<SplitsRefresh> {
    let mut report = SplitsRefresh::default();
    let (mut tickers, mut confirming) = (BTreeSet::new(), BTreeSet::new());
    let mut shares = 0;
    // The operator's renames of this venue's tickers (`FB` is `META`): data, never compiled in.
    let renames = vike_secrets::stock_split::read_ticker_renames(settings_dir)
        .map(|rows| {
            rows.into_iter()
                .filter(|(v, ..)| v == venue)
                .map(|(_, ticker, renamed)| (ticker, renamed))
                .collect::<BTreeMap<_, _>>()
        })
        .map_err(|e| e.to_string());
    for (symbol, first_bar) in series {
        let Some(row) = vike_catalog::kept_row_of_store_name(kept, venue, symbol)
            .ok()
            .filter(|r| r.contract.is_share())
        else {
            continue;
        };
        shares += 1;
        let named = format!("{venue} {symbol} ({})", row.raw_symbol);
        let mapped = match yahoo_ticker(row) {
            Ok(ticker) => ticker,
            Err(why) => {
                report.unmapped.push(format!("{named}: {why}"));
                continue;
            }
        };
        let renamed = match &renames {
            Ok(renames) => renames.get(&mapped).unwrap_or(&mapped).clone(),
            Err(why) => {
                report.failed.push(format!(
                    "{named}: the ticker renames could not be read: {why}; its rows stand as they were"
                ));
                continue;
            }
        };
        let (ticker, answer) = vendors.yahoo_splits(&renamed);
        tickers.insert(ticker.clone());
        let because: Vec<String> = [
            (renamed != mapped).then(|| format!("the venue's rename row for {mapped}")),
            (ticker != renamed).then(|| format!("Yahoo has no {renamed}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !because.is_empty() {
            report.asked_as.push(format!("{named}: asked as {ticker} ({})", because.join("; ")));
        }
        let Vendors { fmp, fmp_answers, .. } = &mut *vendors;
        let listed = match answer {
            Ok(listed) => listed,
            Err(why) => {
                report
                    .failed
                    .push(format!("{named}, Yahoo {ticker}: {why}; its rows stand as they were"));
                continue;
            }
        };
        // A listed event that is no split (a ratio of 1) states nothing to adjust.
        let listed: Vec<(StockSplit, f64, f64)> = listed
            .into_iter()
            .filter_map(|(date, n, d)| {
                StockSplit::new(venue, symbol, &date, n / d).ok().map(|s| (s, n, d))
            })
            .collect();
        report.found += listed.len();
        let moves = |split: &StockSplit| split.effective_ms().is_ok_and(|at| at > *first_bar);
        // FMP only for a ticker whose list moves a stored bar.
        let confirmation: Option<&FmpAnswer> = if listed.iter().any(|(split, ..)| moves(split)) {
            confirming.insert(ticker.clone());
            Some(&*fmp_answers.entry(ticker.clone()).or_insert_with(|| fmp(&ticker)))
        } else {
            None
        };
        let mut verdict = Vec::new();
        for (split, numerator, denominator) in listed {
            let (source, why) = judge(&split.date, numerator, denominator, confirmation);
            if moves(&split) {
                let said = if source.is_some() { "applied" } else { "NOT applied" };
                report.decisions.push(format!(
                    "{named} {} {numerator}:{denominator}: {said}, {why}",
                    split.date
                ));
            }
            if let Some(source) = source {
                report.confirmed += usize::from(source == YAHOO_FMP);
                verdict.push((split, source));
            }
        }
        match vike_secrets::stock_split::replace_sourced_splits(
            settings_dir,
            venue,
            symbol,
            &verdict,
        ) {
            Ok((written, removed)) => {
                report.written += written;
                report.removed += removed;
            }
            Err(e) => report
                .failed
                .push(format!("{named}, Yahoo {ticker}: the rows could not be written: {e}")),
        }
    }
    report.tickers = tickers.len();
    report.fmp_tickers = confirming.len();
    (shares > 0).then_some(report)
}

/// **What one Yahoo split comes to** (the module doc's three cases): the source to write it under (`None`: not
/// applied) and the report's words. `fmp` is FMP's answer for the ticker, `None` when it was not asked. FMP confirms a
/// date it lists with a type naming a split (`stock-split`, measured; any type ending in `split`).
pub(crate) fn judge(
    date: &str,
    numerator: f64,
    denominator: f64,
    fmp: Option<&FmpAnswer>,
) -> (Option<&'static str>, String) {
    let by_ratio = |fmp_said: String| {
        if ratio_rule(numerator, denominator) {
            (Some(YAHOO), format!("by the ratio rule ({fmp_said})"))
        } else {
            (None, format!("looks like a spin-off ({fmp_said})"))
        }
    };
    match fmp {
        Some(Ok(listed)) => {
            if listed.iter().any(|(d, kind)| d == date && kind.ends_with("split")) {
                (Some(YAHOO_FMP), "FMP confirms the split".to_string())
            } else {
                (None, "FMP does not confirm this as a split".to_string())
            }
        }
        Some(Err(why)) => by_ratio(format!("FMP: {why}")),
        None => by_ratio("FMP not asked: no stored bar moves".to_string()),
    }
}

/// **The ratio rule**: a split whose two sides are whole numbers and, in lowest terms, each at most
/// [`RATIO_RULE_MAX_SIDE`] (`4:1`, `3:2`, `1:10`). `1067:1000` (CMCSA's Versant spin-off) and `10000:9356` are not.
pub(crate) fn ratio_rule(numerator: f64, denominator: f64) -> bool {
    let whole = |x: f64| x.fract() == 0.0 && (1.0..=1e12).contains(&x);
    if !(whole(numerator) && whole(denominator)) {
        return false;
    }
    // Both sides are whole and at least 1 and at most 1e12 (checked above), so the casts are exact and the
    // divisor is never 0.
    let (n, d) = (numerator as u64, denominator as u64);
    let a = vike_math::number_theory::greatest_common_divisor(n, d);
    n / a <= RATIO_RULE_MAX_SIDE && d / a <= RATIO_RULE_MAX_SIDE
}

/// FMP's `/stable/splits` answer: a JSON list of `{symbol, date, numerator, denominator, splitType}`, as
/// `(date, splitType)`. A row without the two is skipped.
///
/// # Errors
/// The body is no JSON list (FMP's premium refusal is plain text).
#[cfg(any(test, feature = "tool-feeds-serve"))]
pub(crate) fn parse_fmp_splits(body: &str) -> Result<Vec<(String, String)>, String> {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(body).map_err(|_| "FMP answered no list of splits".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some((r.get("date")?.as_str()?.to_string(), r.get("splitType")?.as_str()?.to_string()))
        })
        .collect())
}

/// **The Yahoo ticker of a share's kept row, from the row's own facts, or why there is none.** Never a guess:
///
/// * a share (`Contract::Equity`/`Etf`) names its listing exchange (alpaca's `exchange`, IBKR's routing exchange other
///   than `SMART`); [`EXCHANGE_SUFFIXES`] gives Yahoo's suffix for it, appended to the row's own name;
/// * a CFD on a share states its listing as a two-letter country after its OWN name's one dot (dukascopy's
///   `AAPL.US/USD`; its base is the ticker alone, decision 0290): `US` is the plain ticker; another country names no
///   exchange, so it maps to nothing;
/// * the name must be a plain ticker, capitals and digits, with at most a share class after one dot, which Yahoo
///   writes with `-` (`BRK.B` is `BRK-B`); any other name maps to nothing.
pub(crate) fn yahoo_ticker(row: &Instrument) -> Result<String, String> {
    match &row.contract {
        Contract::Equity(share) | Contract::Etf(share) => {
            let Some(exchange) = share.exchange.as_deref() else {
                return Err("the row states no listing exchange".to_string());
            };
            let Some((_, suffix)) =
                EXCHANGE_SUFFIXES.iter().find(|(x, _)| x.eq_ignore_ascii_case(exchange))
            else {
                return Err(format!("Yahoo has no suffix known here for the exchange {exchange}"));
            };
            plain(&row.raw_symbol).map(|ticker| format!("{ticker}{suffix}"))
        }
        _ => {
            let name = row.raw_symbol.split('/').next().unwrap_or_default();
            // `TICKER.CC`, one dot and a two-letter country; anything else (IG's `UA.D.AAPL.CASH.IP`) states none.
            match name.split_once('.') {
                Some((ticker, "US")) => plain(ticker),
                Some((_, country))
                    if country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()) =>
                {
                    Err(format!("listed in {country}: the row names a country, not an exchange"))
                }
                _ => Err("the row states no listing".to_string()),
            }
        }
    }
}

/// `ticker` as YAHOO spells it: capitals and digits, and a share class (ONE capital) after one dot written with
/// Yahoo's `-` (`BRK.B` is `BRK-B`, `BF.B` is `BF-B`); else why it maps to nothing. Yahoo's spelling only, used by no
/// other name.
fn plain(ticker: &str) -> Result<String, String> {
    let word =
        |w: &str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    match ticker.split_once('.') {
        None if word(ticker) => Ok(ticker.to_string()),
        Some((base, class))
            if word(base) && class.len() == 1 && class.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            Ok(format!("{base}-{class}"))
        }
        _ => Err(format!("`{ticker}` is no plain ticker")),
    }
}

/// The real Yahoo ask: its chart at the `yahoo` `venue_url` row (read once, at the first ask).
#[cfg(feature = "backfill-serve")]
fn yahoo(settings_dir: &Path) -> impl FnMut(&str) -> YahooAnswer + '_ {
    let mut base: Option<Result<String, String>> = None;
    move |ticker| {
        let base = base.get_or_insert_with(|| yahoo_base(settings_dir)).clone()?;
        use vike_backfill::eod::yahoo::YahooSplitsError;
        let listed =
            vike_backfill::eod::yahoo::fetch_splits(&base, ticker).map_err(|e| match e {
                YahooSplitsError::Unlisted(error) => YahooRefusal::Unlisted(error.to_string()),
                YahooSplitsError::Failed(error) => YahooRefusal::Failed(error.to_string()),
            })?;
        Ok(listed.into_iter().map(|s| (s.date, s.numerator, s.denominator)).collect())
    }
}

/// A build without `backfill-serve` links no Yahoo client: every ticker is reported, no row is touched.
#[cfg(not(feature = "backfill-serve"))]
fn yahoo(_settings_dir: &Path) -> impl FnMut(&str) -> YahooAnswer {
    |_| {
        Err("this datahub was built without `backfill-serve`, which carries the Yahoo client"
            .to_string()
            .into())
    }
}

/// The `yahoo` `venue_url` row's history address (the release's seed on a store that has none).
#[cfg(feature = "backfill-serve")]
fn yahoo_base(settings_dir: &Path) -> Result<String, String> {
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType};
    let rows = vike_secrets::venue_url::read_venue_urls(settings_dir)
        .map_err(|e| format!("the venue_url rows could not be read: {e}"))?;
    vike_secrets::venue_url::venue_urls_of(&rows, YAHOO)
        .url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "")
        .map(str::to_string)
        .map_err(|e| e.to_string())
}

/// **The real FMP ask**: `/stable/splits?symbol=TICKER&apikey=KEY` at the tool feeds' FMP origin, the key read from the
/// box's credential store each ask exactly as the dividends feed reads it (`crate::tool_feeds::ToolFeedKeyReaderStore`,
/// `FMP_API_KEY`), over the tool feeds' transport, whose errors name no URL. Every error text is redacted of the key.
#[cfg(feature = "tool-feeds-serve")]
fn fmp(settings: Option<&vike_secrets::CredentialStore>) -> impl FnMut(&str) -> FmpAnswer {
    let keys = settings.cloned().map(crate::tool_feeds::ToolFeedKeyReaderStore::new);
    let base = crate::tool_feeds::ToolFeedEndpoints::production().fmp_base;
    move |ticker| match &keys {
        Some(keys) => fmp_ask(&crate::tool_feeds::http::ToolFeedHttpUreq, keys, &base, ticker),
        None => Err(format!("no {} is stored", crate::tool_feeds::FMP_API_KEY)),
    }
}

/// **One FMP ask** through `http` with the key `keys` reads, at `base`. Every error text is the key's redaction.
#[cfg(any(test, feature = "tool-feeds-serve"))]
pub(crate) fn fmp_ask(
    http: &dyn crate::tool_feeds::ToolFeedHttp,
    keys: &dyn crate::tool_feeds::ToolFeedKeyReader,
    base: &str,
    ticker: &str,
) -> FmpAnswer {
    use crate::tool_feeds::{FMP_API_KEY, ToolFeedHttpGet, ToolFeedKeyRead};
    let key = match keys.read(FMP_API_KEY) {
        ToolFeedKeyRead::Present(key) => key,
        ToolFeedKeyRead::Unreadable => {
            return Err(format!("the credential store would not read {FMP_API_KEY}"));
        }
        ToolFeedKeyRead::Absent => return Err(format!("no {FMP_API_KEY} is stored")),
    };
    let url = format!("{base}/stable/splits?symbol={ticker}&apikey={}", key.expose());
    let request = ToolFeedHttpGet::new(url, crate::tool_feeds::equity::KEYED_TIMEOUT)
        .accepting_json()
        .carrying_a_key();
    http.get(&request).and_then(|body| parse_fmp_splits(&body)).map_err(|why| key.redact(&why))
}

/// A build without `tool-feeds-serve` links no FMP transport: the ratio rule decides every split.
#[cfg(not(feature = "tool-feeds-serve"))]
fn fmp(_settings: Option<&vike_secrets::CredentialStore>) -> impl FnMut(&str) -> FmpAnswer {
    |_| {
        Err("this datahub was built without `tool-feeds-serve`, which carries the FMP transport"
            .to_string())
    }
}

#[path = "stock_splits_tests.rs"]
#[cfg(test)]
mod stock_splits_tests;
