//! Real data for the tool windows, fetched on background threads (ureq, rustls):
//!   News     → Cointelegraph RSS
//!   Calendar → ForexFactory weekly JSON
//!   Options  → vike-deribit's chain provider (BTC/ETH/SOL book summaries; pricing/model math in
//!              vike-options — this file only orchestrates the fetch loop + shared state). Its
//!              provider carries the app root's opt-in `kind=chain` snapshot recorder
//!              (`VIKE_RECORD_CHAINS=1`, off by default) — see [`options_provider`].
//! (Data Manager reads the app's own live-feed state, not here.)
//! Also home to [`ToolView`] — the per-window VIEW state for these tools.
//!
//! Two of these fetches need a third-party data-provider key ([`ToolApiKeys`]). This module reads
//! NEITHER the environment nor the credential store to get them: the app root resolves both and
//! hands them to [`spawn_tool_fetchers`].

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct NewsItem {
    pub source: String, // provider name
    pub title: String,
    pub summary: String,   // plain-text description (HTML stripped)
    pub url: String,       // <link> — drives "Open original"
    pub market: String,    // crypto / forex / stocks / global — drives the Market filter
    pub tags: Vec<String>, // <category> values — drive classify + reader chips
    pub ts_ms: i64,        // pubDate epoch ms (for "Xm ago" + sorting)
}

/// Coarse topic bucket from a headline (+ feed tags) — a faithful port of `news/classify.py`
/// (first word-boundary keyword rule wins; default "Markets"). Drives the Category filter + chips.
pub fn classify_news(title: &str, tags: &[String]) -> &'static str {
    let mut hay = String::from(" ");
    hay.push_str(&title.to_lowercase());
    hay.push(' ');
    for t in tags {
        hay.push_str(&t.to_lowercase());
        hay.push(' ');
    }
    // (category, keywords) — ordered; trailing-space keys match standalone tokens only.
    const RULES: &[(&str, &[&str])] = &[
        (
            "Earnings",
            &[
                "earnings",
                "quarterly results",
                "earnings results",
                "eps ",
                "guidance",
                "profit",
                "beats estimates",
                "misses estimates",
                "tops estimates",
                "operating margin",
                "net income",
            ],
        ),
        (
            "M&A",
            &[
                "acquire",
                "acquisition",
                "merger",
                "merge",
                "takeover",
                "buyout",
                "to buy",
                "stake in",
                "deal to",
                "in talks to",
            ],
        ),
        (
            "Regulation",
            &[
                "sec ",
                "regulator",
                "lawsuit",
                "antitrust",
                "probe",
                "subpoena",
                "sanction",
                "settlement",
                "indict",
                "banned",
                "to ban",
                "ban on",
            ],
        ),
        (
            "Macro",
            &[
                "inflation",
                "cpi",
                "ppi",
                "gdp",
                "jobless",
                "payroll",
                "unemployment",
                "rate decision",
                "rate cut",
                "rate hike",
                "interest rate",
                "fed ",
                "fomc",
                "ecb ",
                "boe ",
                "central bank",
                "yields",
                "treasury",
                "recession",
                "emergency",
            ],
        ),
        (
            "Commodities",
            &[
                "oil",
                "crude",
                "brent",
                "wti",
                "gold",
                "silver",
                "copper",
                "natural gas",
                "opec",
                "commodit",
            ],
        ),
        (
            "Crypto",
            &[
                "bitcoin",
                "btc",
                "ethereum",
                "eth ",
                "crypto",
                "token",
                "blockchain",
                "defi ",
                "stablecoin",
                "altcoin",
                "solana",
                "xrp",
                "binance",
                "etf inflow",
            ],
        ),
        (
            "Tech",
            &[
                "artificial intelligence",
                "ai ",
                "chip",
                "semiconductor",
                "nvidia",
                "software",
                "cloud",
                "data center",
                "iphone",
                "app store",
                "openai",
            ],
        ),
    ];
    for (cat, kws) in RULES {
        if kws.iter().any(|k| word_boundary_contains(&hay, k)) {
            return cat;
        }
    }
    "Markets"
}

/// `\b`-anchored substring test: `kw` must start at a word boundary (start or after a non-alnum).
fn word_boundary_contains(hay: &str, kw: &str) -> bool {
    let bytes = hay.as_bytes();
    let mut start = 0;
    while let Some(pos) = hay[start..].find(kw) {
        let idx = start + pos;
        if idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric() {
            return true;
        }
        start = idx + 1;
    }
    false
}

pub const NEWS_CATEGORIES: [&str; 8] =
    ["Earnings", "M&A", "Macro", "Crypto", "Regulation", "Commodities", "Tech", "Markets"];
pub const NEWS_MARKETS: [&str; 4] = ["Crypto", "Forex", "Stocks", "Global"];
#[derive(Clone, Default)]
pub struct CalEvent {
    pub time: String,
    pub country: String,      // currency code (USD/EUR/…)
    pub country_name: String, // full name (United States/European Union/…)
    pub iso2: String,         // flag code (us/eu/gb/jp/…)
    pub impact: String,
    pub importance: u8, // 0 low · 1 medium · 2 high (drives the 3-bar glyph)
    pub title: String,
    pub actual: String,
    pub forecast: String,
    pub previous: String,
    pub day: String,       // "Monday, June 29" — day-group header
    pub day_short: String, // "Mon 29" — day-strip card title
    pub date_iso: String,  // "2026-06-29" — day matching for the card strip
    pub ts_ms: i64,        // event time (epoch ms) — countdown + now-marker
}

/// currency code → (full country name, iso2 flag code). Ported from
/// data/calendar/taxonomy.py. Unknown → ("", "") so the UI falls back to the code chip.
pub fn currency_country(ccy: &str) -> (&'static str, &'static str) {
    match ccy {
        "USD" => ("United States", "us"),
        "EUR" => ("European Union", "eu"),
        "GBP" => ("United Kingdom", "gb"),
        "JPY" => ("Japan", "jp"),
        "AUD" => ("Australia", "au"),
        "NZD" => ("New Zealand", "nz"),
        "CAD" => ("Canada", "ca"),
        "CHF" => ("Switzerland", "ch"),
        "CNY" => ("Mainland China", "cn"),
        "INR" => ("India", "in"),
        "BRL" => ("Brazil", "br"),
        "ZAR" => ("South Africa", "za"),
        "KRW" => ("South Korea", "kr"),
        "MXN" => ("Mexico", "mx"),
        "RUB" => ("Russia", "ru"),
        "TRY" => ("Turkey", "tr"),
        "IDR" => ("Indonesia", "id"),
        "SAR" => ("Saudi Arabia", "sa"),
        "SGD" => ("Singapore", "sg"),
        "HKD" => ("Hong Kong", "hk"),
        "SEK" => ("Sweden", "se"),
        "NOK" => ("Norway", "no"),
        _ => ("", ""),
    }
}

fn impact_importance(s: &str) -> u8 {
    match s {
        "High" => 2,
        "Medium" => 1,
        _ => 0,
    }
}
#[derive(Clone, Default)]
pub struct EarningsRow {
    pub date: String, // ISO day (group)
    pub symbol: String,
    pub hour: String, // Pre-mkt / After-hrs / Mid-day
    pub eps_est: String,
    pub eps_act: String,
    pub surprise: String, // signed %
}
#[derive(Clone, Default)]
pub struct DividendRow {
    pub date: String, // ex-date (group)
    pub symbol: String,
    pub pay_date: String,
    pub amount: String,
    pub yield_pct: String,
    pub freq: String,
}
#[derive(Clone, Default)]
pub struct IpoRow {
    pub date: String, // priced/expected day (group)
    pub symbol: String,
    pub company: String,
    pub exchange: String,
    pub price: String,
    pub shares: String,
    pub status: String,
}

#[derive(Clone, Default)]
pub struct ToolData {
    pub news: Vec<NewsItem>,
    pub news_status: String,
    pub calendar: Vec<CalEvent>,
    pub cal_status: String,
    pub cal_range: String, // "Jun 29 — Jul 5, 2026"
    // earnings/dividends/IPO counts per ISO date — drives the day-card rows
    pub cal_equity: std::collections::HashMap<String, (usize, usize, usize)>,
    pub cal_earnings: Vec<EarningsRow>, // Finnhub earnings calendar rows (Earnings page)
    pub cal_dividends: Vec<DividendRow>, // FMP dividends calendar rows (Dividends page)
    pub cal_ipos: Vec<IpoRow>,          // Nasdaq IPO calendar rows (IPO page)
    // Per-underlying options books (BTC/ETH/SOL) — the source of truth the Options tool reads.
    // `opt_by_underlying[u]` is that underlying's nearest-expiries bundle; `opt_underlyings` is the
    // order to show in the selector (BTC, ETH, SOL — only those that fetched, canonical order).
    pub opt_by_underlying: std::collections::BTreeMap<String, UnderlyingChains>,
    pub opt_underlyings: Vec<String>,
    pub opt_status: String,
}

/// One underlying's options snapshot: its nearest expiries + each expiry's ±12-strike chain
/// (greeks-enriched, USD premiums; built via vike-deribit) + the front-expiry default selection.
#[derive(Clone, Default)]
pub struct UnderlyingChains {
    pub default_expiry: String, // nearest expiry ISO date (default selection)
    pub expiries: Vec<vike_options::Expiry>, // nearest expiries for the date strip (ordered)
    pub chains: std::collections::BTreeMap<String, vike_options::OptionChain>, // expiry ISO → chain
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

// --- News (RSS, multiple sources merged) ---
fn clean_html(s: &str) -> String {
    // strip CDATA, tags, and decode the few common entities — RSS descriptions are HTML.
    let s = s.replace("<![CDATA[", "").replace("]]>", "");
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    // numeric entities first (decimal &#8217; AND hex &#x2019;), then named, then collapse space
    let out = decode_numeric_entities(&out);
    out.replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&") // last, so it can't re-create an entity head
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decode every `&#NNN;` (decimal) and `&#xHHHH;` (hex) numeric character reference.
fn decode_numeric_entities(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("&#") {
        result.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        if let Some(semi) = after.find(';') {
            let code = &after[..semi];
            let cp = if let Some(hex) = code.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                code.parse::<u32>().ok()
            };
            if let Some(ch) = cp.and_then(char::from_u32) {
                result.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        result.push_str("&#"); // not a valid entity — keep the literal and move on
        rest = after;
    }
    result.push_str(rest);
    result
}

fn fetch_one_feed(
    agent: &ureq::Agent,
    source: &str,
    market: &str,
    url: &str,
    out: &mut Vec<NewsItem>,
) {
    let Ok(mut resp) = agent.get(url).call() else { return };
    let Ok(body) = resp.body_mut().read_to_string() else { return };
    for item in body.split("<item>").skip(1).take(25) {
        let title = clean_html(between(item, "<title>", "</title>").unwrap_or(""));
        if title.is_empty() {
            continue;
        }
        let summary = clean_html(between(item, "<description>", "</description>").unwrap_or(""));
        let url_s =
            between(item, "<link>", "</link>").map(|s| s.trim().to_string()).unwrap_or_default();
        let ts_ms = between(item, "<pubDate>", "</pubDate>")
            .and_then(|d| chrono::DateTime::parse_from_rfc2822(d.trim()).ok())
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);
        // all <category> values (RSS topic tags) → chips + classify
        let mut tags: Vec<String> = Vec::new();
        let mut rest = item;
        while let Some(t) = between(rest, "<category>", "</category>") {
            let clean = clean_html(t);
            if !clean.is_empty() && tags.len() < 6 {
                tags.push(clean);
            }
            match rest.find("</category>") {
                Some(p) => rest = &rest[p + "</category>".len()..],
                None => break,
            }
        }
        out.push(NewsItem {
            source: source.to_string(),
            title,
            summary: summary.chars().take(400).collect(),
            url: url_s,
            market: market.to_string(),
            tags,
            ts_ms,
        });
    }
}

fn fetch_news() -> Result<Vec<NewsItem>, Box<dyn std::error::Error>> {
    // 16 broad RSS providers across crypto/forex/stocks (ported from news/providers.py)
    const FEEDS: &[(&str, &str, &str)] = &[
        ("CoinDesk", "crypto", "https://www.coindesk.com/arc/outboundfeeds/rss/"),
        ("Cointelegraph", "crypto", "https://cointelegraph.com/rss"),
        ("Decrypt", "crypto", "https://decrypt.co/feed"),
        ("CryptoSlate", "crypto", "https://cryptoslate.com/feed/"),
        ("BeInCrypto", "crypto", "https://beincrypto.com/feed/"),
        ("Bitcoin Magazine", "crypto", "https://bitcoinmagazine.com/feed"),
        ("NewsBTC", "crypto", "https://www.newsbtc.com/feed/"),
        ("CoinJournal", "crypto", "https://coinjournal.net/feed/"),
        ("FXStreet", "forex", "https://www.fxstreet.com/rss/news"),
        ("ForexLive", "forex", "https://www.forexlive.com/feed/news/"),
        ("FXEmpire", "forex", "https://www.fxempire.com/api/v1/en/articles/rss/news"),
        ("Investing.com", "forex", "https://www.investing.com/rss/news.rss"),
        ("Investing.com FX", "forex", "https://www.investing.com/rss/forex.rss"),
        ("MarketWatch", "stocks", "http://feeds.marketwatch.com/marketwatch/topstories/"),
        ("CNBC", "stocks", "https://www.cnbc.com/id/100003114/device/rss/rss.html"),
        ("Seeking Alpha", "stocks", "https://seekingalpha.com/market_currents.xml"),
    ];
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(12))).build().into();
    // fetch all feeds in parallel (16 sequential RSS hits would be far too slow)
    let handles: Vec<_> = FEEDS
        .iter()
        .map(|&(src, mkt, url)| {
            let agent = agent.clone();
            std::thread::spawn(move || {
                let mut v = Vec::new();
                fetch_one_feed(&agent, src, mkt, url, &mut v);
                v
            })
        })
        .collect();
    let mut out = Vec::new();
    for h in handles {
        if let Ok(v) = h.join() {
            out.extend(v);
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.ts_ms));
    out.truncate(500);
    if out.is_empty() {
        return Err("no news".into());
    }
    Ok(out)
}

// --- Calendar (ForexFactory weekly JSON) ---
#[derive(Deserialize)]
struct FfEvent {
    title: String,
    country: String,
    date: String,
    impact: String,
    #[serde(default)]
    forecast: String,
    #[serde(default)]
    previous: String,
    #[serde(default)]
    actual: String,
}
fn fetch_calendar() -> Result<(String, Vec<CalEvent>), Box<dyn std::error::Error>> {
    use chrono::Datelike;
    // browser UA + 10s timeout — the bare request stalls on ForexFactory's CDN (Python uses a UA)
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let mut resp = agent
        .get("https://nfs.faireconomy.media/ff_calendar_thisweek.json")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0 Safari/537.36")
        .header("Accept", "application/json, text/plain, */*")
        .call()?;
    let body = resp.body_mut().read_to_string()?;
    let evs: Vec<FfEvent> = serde_json::from_str(&body)?;
    const WD: [&str; 7] =
        ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    const WD3: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MO: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const MO3: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut bounds: Option<(chrono::NaiveDate, chrono::NaiveDate)> = None;
    let out: Vec<CalEvent> = evs
        .into_iter()
        .take(400) // the full week can run ~150+ events; 90 truncated Thu/Fri (count mismatch)
        .map(|e| {
            // date "2026-06-29T08:15:00-04:00" → convert to LOCAL tz (matches vike's _local
            // display-tz bucketing) so times + per-day grouping line up with the Python app.
            let dt = chrono::DateTime::parse_from_rfc3339(&e.date)
                .ok()
                .map(|d| d.with_timezone(&chrono::Local));
            if let Some(d) = dt {
                let nd = d.date_naive();
                bounds = Some(match bounds {
                    Some((lo, hi)) => (lo.min(nd), hi.max(nd)),
                    None => (nd, nd),
                });
            }
            let time = dt.map(|d| d.format("%H:%M").to_string()).unwrap_or_else(|| {
                e.date
                    .split('T')
                    .nth(1)
                    .map(|t| t.chars().take(5).collect())
                    .unwrap_or_else(|| "—".into())
            });
            let (day, day_short) = dt
                .map(|d| {
                    let wd = d.weekday().num_days_from_monday() as usize;
                    (
                        format!("{}, {} {}", WD[wd], MO[d.month0() as usize], d.day()),
                        format!("{} {}", WD3[wd], d.day()),
                    )
                })
                .unwrap_or_default();
            let (country_name, iso2) = currency_country(&e.country);
            CalEvent {
                time,
                importance: impact_importance(&e.impact),
                country: e.country,
                country_name: country_name.to_string(),
                iso2: iso2.to_string(),
                impact: e.impact,
                title: e.title,
                actual: e.actual,
                forecast: e.forecast,
                previous: e.previous,
                day,
                day_short,
                date_iso: dt.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
                ts_ms: dt.map(|d| d.timestamp_millis()).unwrap_or(0),
            }
        })
        .collect();
    // range label = the Mon→Sun week containing today (matches the day-strip cards AND Python's
    // CalendarSpace), NOT the min/max event-date span the feed happens to carry.
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let sunday = monday + chrono::Duration::days(6);
    let range = format!(
        "{} {} — {} {}, {}",
        MO3[monday.month0() as usize],
        monday.day(),
        MO3[sunday.month0() as usize],
        sunday.day(),
        sunday.year()
    );
    let _ = bounds; // (previously the data-span range; superseded by the Mon→Sun week)
    Ok((range, out))
}

// --- Equity calendars (Nasdaq): earnings / dividends / IPO counts per day ---
// Powers the day-strip card category rows. Nasdaq blocks bare clients → browser UA required.
fn nasdaq_json(agent: &ureq::Agent, url: &str) -> Option<serde_json::Value> {
    let mut resp = agent
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0 Safari/537.36")
        .header("Accept", "application/json, text/plain, */*")
        .call()
        .ok()?;
    let body = resp.body_mut().read_to_string().ok()?;
    serde_json::from_str(&body).ok()
}

/// The env-var / store key holding the Finnhub key the weekly EARNINGS fetch needs.
pub const FINNHUB_API_KEY: &str = "FINNHUB_API_KEY";
/// The env-var / store key holding the FMP key the weekly DIVIDENDS fetch needs.
pub const FMP_API_KEY: &str = "FMP_API_KEY";

/// The third-party data-provider keys the equity-calendar fetches need — resolved by the BINARY
/// and handed to [`spawn_tool_fetchers`], never read here.
///
/// These are NOT venue credentials: they buy read-only market metadata (Finnhub earnings, FMP
/// dividends) for the calendar day-strip. Absent, those two counts are simply blank and the third
/// (Nasdaq IPOs, keyless) still lands — nothing about trading changes.
///
/// # Where they come from
///
/// The process environment first, `<project>/settings/secrets.env` second — the SAME store every
/// venue credential lives in, resolved by the same `vike_secrets` walk. Both tiers arrive as
/// PARAMETERS ([`ToolApiKeys::resolve`]) because this is a library: only the binary reads global
/// configuration state.
///
/// ⚠ **A CWD-relative `./.env` used to be the second tier and is deliberately gone.** It was the
/// last location outside `<project>/settings/`, and CWD-relative is the worst kind — the same
/// binary reads a different file depending on the directory it was launched from, so a key that
/// works from the repo root vanishes when the app is started from anywhere else. There is no
/// legacy fallback: move the `FINNHUB_API_KEY=` / `FMP_API_KEY=` line into
/// `<project>/settings/secrets.env` (or export the variable), and the panel fills in again.
///
/// Resolved ONCE, at startup: a key added to the store afterwards needs a restart. The fetch loop
/// re-read it every 30 minutes before, which is not a property worth a per-cycle store read on a
/// background thread — the Connections tool is where live credential state is shown.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ToolApiKeys {
    /// `FINNHUB_API_KEY` — the weekly earnings calendar.
    pub finnhub: Option<String>,
    /// `FMP_API_KEY` — the weekly dividends calendar.
    pub fmp: Option<String>,
}

impl ToolApiKeys {
    /// Resolve both keys from an already-loaded process environment and credential store.
    ///
    /// PURE — no environment read, no file read, so the precedence below is unit-testable without
    /// mutating process-global state (`set_var` is unsound under threads; the repo's rule).
    pub fn resolve(env: &HashMap<String, String>, store: &HashMap<String, String>) -> Self {
        Self {
            finnhub: layered_key(env, store, FINNHUB_API_KEY),
            fmp: layered_key(env, store, FMP_API_KEY),
        }
    }
}

/// Redacts. A key is a secret, and this type is the only place one is now NAMED rather than living
/// as an anonymous local — so it gets the same manual `Debug` every credential type here has.
impl std::fmt::Debug for ToolApiKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = |v: &Option<String>| if v.is_some() { "<set>" } else { "<unset>" };
        f.debug_struct("ToolApiKeys")
            .field("finnhub", &shown(&self.finnhub))
            .field("fmp", &shown(&self.fmp))
            .finish()
    }
}

/// One key, process env FIRST and credential store second.
///
/// A present-but-BLANK value falls THROUGH to the next tier rather than winning, at both tiers —
/// preserving the pre-injection behaviour exactly, and the right rule for this knob: an empty
/// `FINNHUB_API_KEY=` cannot mean "no key on purpose", because absent already means that and both
/// spellings produce the identical blank column. (`vike_polymarket`'s `proxy_var_layered` makes
/// the OPPOSITE call for its proxy knobs, where an empty value genuinely means "direct".)
fn layered_key(
    env: &HashMap<String, String>,
    store: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    [env, store]
        .into_iter()
        .filter_map(|m| m.get(key))
        .map(|v| v.trim())
        .find(|v| !v.is_empty())
        .map(str::to_string)
}

/// Earnings rows for the whole week — Finnhub (matches vike's source). ONE call.
fn fetch_finnhub_earnings(agent: &ureq::Agent, key: &str, frm: &str, to: &str) -> Vec<EarningsRow> {
    let mut out = Vec::new();
    let url = format!("https://finnhub.io/api/v1/calendar/earnings?from={frm}&to={to}&token={key}");
    if let Some(v) = nasdaq_json(agent, &url) {
        if let Some(arr) = v["earningsCalendar"].as_array() {
            for r in arr {
                let date = r["date"].as_str().unwrap_or("").to_string();
                if date.is_empty() {
                    continue;
                }
                let hour = match r["hour"].as_str().unwrap_or("") {
                    "bmo" => "Pre-mkt",
                    "amc" => "After-hrs",
                    "dmh" => "Mid-day",
                    _ => "",
                };
                let est = r["epsEstimate"].as_f64();
                let act = r["epsActual"].as_f64();
                let surprise = match (act, est) {
                    (Some(a), Some(e)) if e.abs() > 1e-9 => {
                        format!("{:+.1}%", (a - e) / e.abs() * 100.0)
                    }
                    _ => String::new(),
                };
                out.push(EarningsRow {
                    date,
                    symbol: r["symbol"].as_str().unwrap_or("").to_string(),
                    hour: hour.to_string(),
                    eps_est: est.map(|x| format!("{x:.2}")).unwrap_or_default(),
                    eps_act: act.map(|x| format!("{x:.2}")).unwrap_or_default(),
                    surprise,
                });
            }
        }
    }
    out
}

/// Dividend rows for the whole week — FMP (matches vike's source). ONE call.
fn fetch_fmp_dividends(agent: &ureq::Agent, key: &str, frm: &str, to: &str) -> Vec<DividendRow> {
    let mut out = Vec::new();
    let url = format!("https://financialmodelingprep.com/stable/dividends-calendar?from={frm}&to={to}&apikey={key}");
    if let Some(v) = nasdaq_json(agent, &url) {
        if let Some(arr) = v.as_array() {
            for r in arr {
                let date = r["date"].as_str().unwrap_or("").to_string();
                if date.is_empty() {
                    continue;
                }
                let amount =
                    r["dividend"].as_f64().map(|x| format!("{x:.2} $")).unwrap_or_default();
                let yld = r["yield"].as_f64().map(|x| format!("{x:.2}%")).unwrap_or_default();
                out.push(DividendRow {
                    date,
                    symbol: r["symbol"].as_str().unwrap_or("").to_string(),
                    pay_date: r["paymentDate"].as_str().unwrap_or("").to_string(),
                    amount,
                    yield_pct: yld,
                    freq: r["frequency"].as_str().unwrap_or("").to_string(),
                });
            }
        }
    }
    out
}

/// Monthly IPO calendar rows (one or two calls for the window), priced + upcoming.
fn fetch_ipo_by_day(agent: &ureq::Agent, monday: chrono::NaiveDate) -> Vec<IpoRow> {
    use chrono::Duration as CDur;
    let mut out: Vec<IpoRow> = Vec::new();
    let months: std::collections::BTreeSet<String> =
        [monday.format("%Y-%m").to_string(), (monday + CDur::days(6)).format("%Y-%m").to_string()]
            .into_iter()
            .collect();
    for ym in months {
        if let Some(v) =
            nasdaq_json(agent, &format!("https://api.nasdaq.com/api/ipo/calendar?date={ym}"))
        {
            for (rows, status) in [
                (&v["data"]["priced"]["rows"], "Priced"),
                (&v["data"]["upcoming"]["upcomingTable"]["rows"], "Upcoming"),
            ] {
                if let Some(rows) = rows.as_array() {
                    for r in rows {
                        let raw = r["pricedDate"]
                            .as_str()
                            .or_else(|| r["expectedPriceDate"].as_str())
                            .unwrap_or("");
                        let parts: Vec<&str> = raw.split('/').collect(); // "MM/DD/YYYY"
                        let date = if parts.len() == 3 {
                            format!("{}-{:0>2}-{:0>2}", parts[2], parts[0], parts[1])
                        } else {
                            String::new()
                        };
                        out.push(IpoRow {
                            date,
                            symbol: r["proposedTickerSymbol"].as_str().unwrap_or("").to_string(),
                            company: r["companyName"].as_str().unwrap_or("").to_string(),
                            exchange: r["proposedExchange"].as_str().unwrap_or("").to_string(),
                            price: r["proposedSharePrice"].as_str().unwrap_or("").to_string(),
                            shares: r["sharesOffered"].as_str().unwrap_or("").to_string(),
                            status: status.to_string(),
                        });
                    }
                }
            }
        }
    }
    out
}

// --- Options (Deribit, via the vike-deribit chain provider) ---

/// Build one underlying's bundle: the nearest 8 expiries, each chain greeks-enriched and windowed
/// to ±30 strikes around spot (the WIDEST display preset), plus the front-expiry default. The
/// renderer trims to the user's ±N so switching N never needs a re-fetch. Returns `None` (skip this
/// underlying) when the summary fetch errors or every chain came back empty — so one dead book
/// never fails the whole multi-underlying pass. `r = 0.0` matches the Python default (env
/// `options_risk_free` is deliberately unread here — see vike-options `greeks`).
fn fetch_underlying(
    provider: &mut vike_deribit::chain::DeribitOptionsProvider,
    underlying: &str,
) -> Option<UnderlyingChains> {
    let mut expiries = provider.list_expiries(underlying).ok()?;
    expiries.truncate(8);
    let mut chains = std::collections::BTreeMap::new();
    for e in &expiries {
        // ±30 = the max `STRIKE_WINDOW_PRESETS` value; the renderer windows down to the active ±N.
        if let Ok(chain) = provider.fetch_chain(underlying, &e.date, Some(30), 0.0) {
            chains.insert(e.date.clone(), chain);
        }
    }
    if chains.is_empty() {
        return None;
    }
    // default = the front expiry (lowest DTE) — mirrors vike's `_ExpiryStrip.set_expiries`
    let default_expiry =
        expiries.iter().min_by_key(|e| e.dte).map(|e| e.date.clone()).unwrap_or_default();
    Some(UnderlyingChains { default_expiry, expiries, chains })
}

/// One options refresh: fetch EVERY underlying the provider offers (BTC/ETH/SOL) — a thin adapter
/// over vike-deribit's `DeribitOptionsProvider` (its 5s cache keeps a full multi-underlying pass
/// cheap). Skips any underlying that errors/returns empty (keeps the others); returns the map of
/// fetched underlyings only. Errors only when NOTHING fetched (so the caller keeps last-good).
fn fetch_options(
    provider: &mut vike_deribit::chain::DeribitOptionsProvider,
) -> Result<std::collections::BTreeMap<String, UnderlyingChains>, String> {
    let mut by_underlying = std::collections::BTreeMap::new();
    for u in provider.list_underlyings() {
        if let Some(bundle) = fetch_underlying(provider, u) {
            by_underlying.insert(u.to_string(), bundle);
        }
    }
    if by_underlying.is_empty() {
        return Err("no option chains".into());
    }
    Ok(by_underlying)
}

/// Canonical rank for the underlying selector order: BTC, ETH, SOL, then anything else.
fn underlying_rank(u: &str) -> u8 {
    match u {
        "BTC" => 0,
        "ETH" => 1,
        "SOL" => 2,
        _ => 3,
    }
}

/// Resolve which underlying's bundle the Options tool should render, and reset a now-invalid expiry
/// selection (the BTC→ETH switch guard). Pure so the app's Options arm is unit-testable without
/// egui. Returns the selected `UnderlyingChains` (or `None` when nothing has fetched yet).
///
/// - selected underlying = `underlying_sel` if it's still a fetched underlying, else the first of
///   `order` (canonical: BTC).
/// - if `expiry_sel` names an expiry that isn't in the selected underlying's chains, it's cleared
///   so the underlying's own `default_expiry` takes over (switching BTC→ETH never shows an empty
///   grid).
pub fn resolve_options_selection<'a>(
    by_underlying: &'a std::collections::BTreeMap<String, UnderlyingChains>,
    order: &[String],
    underlying_sel: &Option<String>,
    expiry_sel: &mut Option<String>,
) -> Option<&'a UnderlyingChains> {
    let active_u = underlying_sel
        .as_ref()
        .filter(|u| by_underlying.contains_key(*u))
        .cloned()
        .or_else(|| order.first().cloned())?;
    let bundle = by_underlying.get(&active_u)?;
    if let Some(sel) = expiry_sel.as_ref() {
        if !bundle.chains.contains_key(sel) {
            *expiry_sel = None;
        }
    }
    Some(bundle)
}

/// Build the options-refresh thread's Deribit provider, threading in the opt-in `kind=chain`
/// snapshot recorder when the app root supplied one. Factored out of [`spawn_tool_fetchers`] so the
/// production wiring is a testable unit (the thread body itself needs the network).
///
/// `chain_rec` is `None` unless `vike-app` resolved one from `VIKE_RECORD_CHAINS=1` — see
/// [`vike_data::ChainRecorder::open_from_env`]. `None` is byte-identical to pre-recording behavior:
/// `fetch_chain` skips the record hook entirely. The ONE startup log line (never per-fetch — this is
/// a 30s poll loop, and the whole point is that an operator who set the flag can confirm it took)
/// fires only when recording is actually live.
///
/// The recorder type is UNGATED (`ChainRecorder` holds only `Arc<dyn HistStore>`), which is why this
/// crate can thread it while keeping its `vike-data` dependency feature-free — no DataFusion in the
/// vike-app-core build. The store open lives in vike-data behind `hist-datafusion`, where the app
/// binary (which already enables it) calls it.
fn options_provider(
    chain_rec: Option<Arc<vike_data::ChainRecorder>>,
) -> vike_deribit::chain::DeribitOptionsProvider {
    let provider = vike_deribit::chain::DeribitOptionsProvider::new();
    match chain_rec {
        Some(rec) => {
            tracing::info!("options: kind=chain snapshot recording is LIVE for this session");
            provider.with_chain_recorder(rec)
        }
        None => provider,
    }
}

/// REST options re-poll cadence. With the `markprice.options` WS feed carrying live mark/IV/greeks
/// chain-wide AND the `ticker.{inst}.100ms` feed carrying live bid/ask for the visible focus-set (both
/// wired in [`spawn_tool_fetchers`]), this poll now backstops chain STRUCTURE + OI + volume + the
/// bid/ask of strikes OUTSIDE the ticker focus-set. 12s × 3 underlyings (5s-cached) ≈ 15 REST
/// calls/min — well inside Deribit's public limits.
const OPT_POLL_SECS: u64 = 12;

/// Fold a batch of streamed `markprice.options` rows onto the live option grid: for each row,
/// resolve its `(underlying, expiry, strike, call/put)` coordinates from the venue instrument id and
/// update the matching quote's `mark` (coin→USD scaled by the chain spot, matching the REST path)
/// and `iv` (already a decimal — stored verbatim), then re-derive greeks from the fresh IV. Rows
/// whose instrument isn't in the current grid (a strike outside the fetched ±window, or an
/// expiry/underlying not yet fetched) are skipped. Returns the number of quotes actually updated
/// (0 ⇒ nothing changed, so the caller can skip the repaint).
///
/// Pure over the grid state (no I/O), so the fold is unit-testable without a socket. `now` sets the
/// greeks' time-to-expiry; `r` is the risk-free (0.0, matching [`fetch_underlying`]). The join key,
/// the coin→USD scaling, and the `enrich_quote` re-derivation are the SAME primitives the REST
/// [`vike_deribit::chain::build_chain_from_summary`] uses, so a streamed update and a re-poll agree.
pub fn apply_markprice_to_chains(
    by_underlying: &mut std::collections::BTreeMap<String, UnderlyingChains>,
    rows: &[vike_deribit::options_feed::MarkPriceRow],
    now: i64,
    r: f64,
) -> usize {
    let mut updated = 0usize;
    for row in rows {
        let Some((base, expiry_iso, strike, kind)) =
            vike_deribit::chain::parse_instrument_name(&row.instrument_name)
        else {
            continue;
        };
        let Some(bundle) = by_underlying.get_mut(&base) else { continue };
        let Some(chain) = bundle.chains.get_mut(&expiry_iso) else { continue };
        let spot = chain.underlying_price;
        // Mirror chain.rs's `usd()` EXACTLY so a streamed mark and a re-polled mark agree: USDC-book
        // premiums (SOL) are already USD (scale 1.0, passed through); coin-settled (BTC/ETH) scale by
        // the spot. A 0 scale (a coin book with no spot yet) yields an absent mark, never a 0.0.
        let scale =
            if vike_deribit::chain::is_usd_quoted(&base) { 1.0 } else { spot.unwrap_or(0.0) };
        let mark_usd = if scale == 0.0 { None } else { Some(row.mark_price * scale) };
        let t = vike_options::years_to_expiry(&expiry_iso, now);
        let Some(slot) = chain.rows.iter_mut().find(|sr| sr.strike == strike) else { continue };
        let q = match kind {
            vike_options::OptionKind::Call => &mut slot.call,
            vike_options::OptionKind::Put => &mut slot.put,
        };
        if let Some(quote) = q.take() {
            let mut nq = quote;
            nq.mark = mark_usd;
            nq.iv = Some(row.iv);
            *q = Some(vike_options::enrich_quote(nq, spot, t, r));
            updated += 1;
        }
    }
    updated
}

/// Fold a batch of streamed `ticker.{instrument}.100ms` rows onto the live option grid — the bid/ask
/// twin of [`apply_markprice_to_chains`]. For each row, resolve its `(underlying, expiry, strike,
/// call/put)` coordinates from the venue instrument id and update the matching quote's `bid`/`ask`
/// (coin→USD scaled by the chain spot), plus `mark` and `iv` when the ticker carries them (`mark_iv`
/// is a PERCENT → ÷100), then re-derive greeks from the fresh IV. Rows whose instrument isn't in the
/// current grid (a strike outside the fetched window, or an unfetched expiry/underlying) are skipped.
/// Returns the number of quotes actually updated (0 ⇒ nothing changed, so the caller skips repaint).
///
/// UPDATE POLICY (the one deliberate asymmetry vs the markprice fold):
/// - `bid`/`ask` are set UNCONDITIONALLY — this feed IS the authoritative live top-of-book, so an
///   absent field clears a stale quote rather than lingering.
/// - `mark`/`iv` are refined only when the ticker CARRIES them — the `markprice.options` delta feed
///   is their primary continuous source, so a sparse ticker frame must not wipe a good mark/IV.
///
/// Scaling uses the chain's REST-seeded spot (NOT the ticker's own `underlying_price`) so every strike
/// scales by ONE consistent spot — exactly like [`vike_deribit::chain::build_chain_from_summary`] and
/// the markprice fold, so a streamed update and a re-poll agree. A 0 scale (a coin book with no spot
/// yet) yields absent, never a fabricated 0.0. Pure over the grid state (no I/O), so it's
/// unit-testable without a socket. `now` sets the greeks' time-to-expiry; `r` is the risk-free (0.0).
pub fn apply_ticker_to_chains(
    by_underlying: &mut std::collections::BTreeMap<String, UnderlyingChains>,
    rows: &[vike_deribit::options_feed::TickerRow],
    now: i64,
    r: f64,
) -> usize {
    let mut updated = 0usize;
    for row in rows {
        let Some((base, expiry_iso, strike, kind)) =
            vike_deribit::chain::parse_instrument_name(&row.instrument_name)
        else {
            continue;
        };
        let Some(bundle) = by_underlying.get_mut(&base) else { continue };
        let Some(chain) = bundle.chains.get_mut(&expiry_iso) else { continue };
        let spot = chain.underlying_price;
        // Mirror chain.rs's `usd()` EXACTLY: USDC-book premiums (SOL) are already USD (scale 1.0);
        // coin-settled (BTC/ETH) scale by the chain spot. 0 scale ⇒ absent (never a fabricated 0.0).
        let scale =
            if vike_deribit::chain::is_usd_quoted(&base) { 1.0 } else { spot.unwrap_or(0.0) };
        let usd = |v: Option<f64>| -> Option<f64> {
            if scale == 0.0 {
                None
            } else {
                v.map(|x| x * scale)
            }
        };
        let t = vike_options::years_to_expiry(&expiry_iso, now);
        let Some(slot) = chain.rows.iter_mut().find(|sr| sr.strike == strike) else { continue };
        let q = match kind {
            vike_options::OptionKind::Call => &mut slot.call,
            vike_options::OptionKind::Put => &mut slot.put,
        };
        if let Some(quote) = q.take() {
            let mut nq = quote;
            // bid/ask: the ticker is the authoritative live top-of-book → set unconditionally.
            nq.bid = usd(row.best_bid);
            nq.ask = usd(row.best_ask);
            // mark/iv: refine only when carried (markprice is their primary feed — never clobber).
            if let Some(m) = row.mark_price {
                nq.mark = usd(Some(m));
            }
            if let Some(iv_pct) = row.mark_iv {
                nq.iv = Some(iv_pct / 100.0); // mark_iv is a PERCENT → decimal (REST parity)
            }
            *q = Some(vike_options::enrich_quote(nq, spot, t, r));
            updated += 1;
        }
    }
    updated
}

/// Collect the venue instrument ids for the FRONT (nearest) expiry's ATM ±`n` strikes of each named
/// underlying, read from the seeded grid — the fixed focus-set the `ticker.{inst}.100ms` bid/ask feed
/// subscribes. "ATM" is the strike band straddling the chain spot; ±n takes n strikes each side (the
/// same window shape as [`vike_options::limit_strikes`]). BOTH the call and put id at each in-window
/// strike are collected (the grid renders both sides). An underlying that hasn't fetched, has no
/// front-expiry chain, or has no spot is skipped. Pure (grid in, ids out) so the selection is
/// unit-testable without a socket.
//
// TODO(dynamic-focus): MVP = a FIXED front-expiry ±n window. The next increment follows the user's
// live expiry/strike-window selection (`ToolView::opt_expiry_sel` / `opt_strike_window`) so the
// ticker set tracks whatever the user is actually viewing, plumbed UI→feed (re-subscribe on change).
fn front_expiry_focus_instruments(
    by_underlying: &std::collections::BTreeMap<String, UnderlyingChains>,
    underlyings: &[&str],
    n: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for &u in underlyings {
        let Some(bundle) = by_underlying.get(u) else { continue };
        // Front expiry = the bundle's default (nearest DTE).
        let Some(chain) = bundle.chains.get(&bundle.default_expiry) else { continue };
        let Some(spot) = chain.underlying_price else { continue };
        // ATM split: first strike at/above spot; take n below + n at/above (limit_strikes' window).
        let split = chain.rows.iter().position(|r| r.strike >= spot).unwrap_or(chain.rows.len());
        let lo = split.saturating_sub(n);
        let hi = (split + n).min(chain.rows.len());
        for sr in &chain.rows[lo..hi] {
            for q in [sr.call.as_ref(), sr.put.as_ref()].into_iter().flatten() {
                if let Some(name) = &q.instrument_name {
                    out.push(name.clone());
                }
            }
        }
    }
    out
}

/// Spawn one thread per source; each fetches, updates the shared state, wakes the
/// UI, then sleeps and refreshes.
///
/// `chain_rec` is the app root's opt-in option-chain snapshot recorder (`VIKE_RECORD_CHAINS=1`,
/// `None` by default) — threaded into the options-refresh thread's provider by
/// [`options_provider`], so every polled chain lands in the `kind=chain` PIT series. `None` leaves
/// the options path byte-identical to pre-recording behavior.
///
/// `api_keys` are the third-party data-provider keys the equity-calendar thread fetches with,
/// resolved by the app root — see [`ToolApiKeys`], which is also where the two absent-key
/// behaviours are stated. [`ToolApiKeys::default`] (both `None`) is a complete answer: that thread
/// then runs the keyless Nasdaq IPO fetch alone.
///
/// Returns the options-refresh [`Sender`](std::sync::mpsc::Sender): send `()` on it (e.g. from the
/// Options tool's Refresh pill) to wake the options poll thread for an IMMEDIATE re-poll instead of
/// waiting out its 30s cadence. Dropping the sender is harmless — the thread keeps its 30s cadence.
pub fn spawn_tool_fetchers(
    state: Arc<Mutex<ToolData>>,
    wake: impl Fn() + Send + Clone + 'static,
    chain_rec: Option<Arc<vike_data::ChainRecorder>>,
    api_keys: ToolApiKeys,
) -> std::sync::mpsc::Sender<()> {
    let news = state.clone();
    let w = wake.clone();
    std::thread::spawn(move || loop {
        match fetch_news() {
            Ok(n) => {
                let mut s = news.lock().unwrap();
                s.news_status = format!("{} headlines · LIVE", n.len());
                s.news = n;
            }
            Err(e) => news.lock().unwrap().news_status = format!("news error: {e}"),
        }
        w();
        std::thread::sleep(Duration::from_secs(300));
    });

    let cal = state.clone();
    let w = wake.clone();
    std::thread::spawn(move || loop {
        let ok = match fetch_calendar() {
            Ok((range, c)) => {
                let mut s = cal.lock().unwrap();
                s.calendar = c;
                s.cal_range = range;
                s.cal_status = "LIVE · ForexFactory".into();
                true
            }
            Err(e) => {
                cal.lock().unwrap().cal_status = format!("calendar error: {e}");
                false
            }
        };
        w();
        // on success refresh in 30 min; on failure retry in 20s (don't strand an empty calendar)
        std::thread::sleep(Duration::from_secs(if ok { 1800 } else { 20 }));
    });

    // Equity-calendar counts for the day-strip cards — SAME providers as vike (so the numbers
    // match): Earnings→Finnhub, Dividends→FMP (both single range-queries), IPO→Nasdaq. The 3
    // fetches run in PARALLEL behind a timed agent (10s) and each updates+wakes as it lands.
    let eq = state.clone();
    let w = wake.clone();
    std::thread::spawn(move || loop {
        use chrono::{Datelike, Duration as CDur, Local};
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(12)))
            .build()
            .into();
        let today = Local::now().date_naive(); // Local week (matches the displayed strip)
        let monday = today - CDur::days(today.weekday().num_days_from_monday() as i64);
        let frm = monday.format("%Y-%m-%d").to_string();
        let to = (monday + CDur::days(6)).format("%Y-%m-%d").to_string();
        let mut handles = Vec::new();
        // Earnings → Finnhub (one call)
        if let Some(k) = api_keys.finnhub.clone() {
            let (a, e2, w2, frm2, to2) =
                (agent.clone(), eq.clone(), w.clone(), frm.clone(), to.clone());
            handles.push(std::thread::spawn(move || {
                let rows = fetch_finnhub_earnings(&a, &k, &frm2, &to2);
                let mut s = e2.lock().unwrap();
                for v in s.cal_equity.values_mut() {
                    v.0 = 0;
                }
                for r in &rows {
                    s.cal_equity.entry(r.date.clone()).or_insert((0, 0, 0)).0 += 1;
                }
                s.cal_earnings = rows;
                drop(s);
                w2();
            }));
        }
        // Dividends → FMP (one call)
        if let Some(k) = api_keys.fmp.clone() {
            let (a, e2, w2, frm2, to2) =
                (agent.clone(), eq.clone(), w.clone(), frm.clone(), to.clone());
            handles.push(std::thread::spawn(move || {
                let rows = fetch_fmp_dividends(&a, &k, &frm2, &to2);
                let mut s = e2.lock().unwrap();
                for v in s.cal_equity.values_mut() {
                    v.1 = 0;
                }
                for r in &rows {
                    s.cal_equity.entry(r.date.clone()).or_insert((0, 0, 0)).1 += 1;
                }
                s.cal_dividends = rows;
                drop(s);
                w2();
            }));
        }
        // IPO → Nasdaq (monthly, bucketed). (b) its OWN agent with a tighter per-call timeout so a
        // single stuck month-call fails fast instead of stalling behind the shared global timeout.
        let (e2, w2) = (eq.clone(), w.clone());
        handles.push(std::thread::spawn(move || {
            let ipo_agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(8)))
                .build()
                .into();
            let rows = fetch_ipo_by_day(&ipo_agent, monday);
            // (c) only overwrite when we actually got data — a transient failure keeps last-good rows
            if !rows.is_empty() {
                let mut s = e2.lock().unwrap();
                for v in s.cal_equity.values_mut() {
                    v.2 = 0;
                }
                for r in &rows {
                    if !r.date.is_empty() {
                        s.cal_equity.entry(r.date.clone()).or_insert((0, 0, 0)).2 += 1;
                    }
                }
                s.cal_ipos = rows;
            }
            w2();
        }));
        for h in handles {
            let _ = h.join();
        }
        // (d) if the IPO feed came up empty (almost always a transient fetch miss — Nasdaq has IPOs
        // most weeks), retry in 60s instead of stranding the gap for the full 30-min cycle.
        let retry_soon = eq.lock().unwrap().cal_ipos.is_empty();
        std::thread::sleep(Duration::from_secs(if retry_soon { 60 } else { 1800 }));
    });

    let opt = state.clone();
    let w = wake.clone();
    // Clones for the visible-strike `ticker` (bid/ask) feed, which — unlike the markprice feed —
    // needs the SEEDED grid to know which instruments to subscribe, so it is spawned from INSIDE this
    // poll thread on the first successful REST pass (the focus-set is read from the seeded chains).
    let ticker_state = state.clone();
    let ticker_wake = wake.clone();
    // Refresh channel: the Options tool's Refresh pill sends `()` here to force an immediate
    // re-poll; the thread otherwise wakes on its own OPT_POLL_SECS cadence (see `recv_timeout`).
    let (opt_refresh_tx, opt_refresh_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut provider = options_provider(chain_rec);
        // The live bid/ask feed for the front-expiry ATM focus-set. Spawned ONCE, on the first seed
        // that yields a non-empty focus-set (held for the thread's — i.e. the process's — lifetime;
        // dropping it would merely detach the reconnecting feed).
        let mut ticker_feed: Option<vike_deribit::options_feed::TickerFeed> = None;
        loop {
            match fetch_options(&mut provider) {
                Ok(by_underlying) => {
                    // Merge + rank under the lock, then (first seed only) read out the focus-set;
                    // release the lock BEFORE spawning the feed — its fold closure re-locks the mutex.
                    let focus = {
                        let mut s = opt.lock().unwrap();
                        // Merge per-underlying so an underlying that transiently failed this pass keeps
                        // its last-good bundle (same last-good discipline the IPO/dividend feeds use).
                        for (u, bundle) in by_underlying {
                            s.opt_by_underlying.insert(u, bundle);
                        }
                        // Selector order: canonical BTC, ETH, SOL over whatever is currently present.
                        let mut order: Vec<String> = s.opt_by_underlying.keys().cloned().collect();
                        order.sort_by_key(|u| underlying_rank(u));
                        s.opt_underlyings = order;
                        s.opt_status = "LIVE · Deribit".into();
                        if ticker_feed.is_none() {
                            front_expiry_focus_instruments(
                                &s.opt_by_underlying,
                                &["BTC", "ETH"],
                                10,
                            )
                        } else {
                            Vec::new()
                        }
                    };
                    if ticker_feed.is_none() && !focus.is_empty() {
                        let ts = ticker_state.clone();
                        let tw = ticker_wake.clone();
                        ticker_feed = Some(vike_deribit::options_feed::spawn_deribit_ticker_feed(
                            vike_deribit::options_feed::MAINNET_WS.to_string(),
                            focus,
                            move |row| {
                                let n = {
                                    let mut s = ts.lock().unwrap();
                                    apply_ticker_to_chains(
                                        &mut s.opt_by_underlying,
                                        std::slice::from_ref(&row),
                                        vike_model::now_ms(),
                                        0.0,
                                    )
                                };
                                if n > 0 {
                                    tw();
                                }
                            },
                        ));
                    }
                }
                Err(e) => opt.lock().unwrap().opt_status = format!("options error: {e}"),
            }
            w();
            // Wake on the OPT_POLL_SECS cadence OR an explicit Refresh signal, whichever comes first.
            // Coalesce a burst of clicks: refetch exactly once per wake, draining queued signals.
            match opt_refresh_rx.recv_timeout(Duration::from_secs(OPT_POLL_SECS)) {
                Ok(()) => while opt_refresh_rx.try_recv().is_ok() {},
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                // Sender dropped (app shutting down): keep the cadence, never busy-loop.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_secs(OPT_POLL_SECS));
                }
            }
        }
    });

    // Live mark/IV streaming ON TOP of the REST poll: `markprice.options.{btc_usd,eth_usd,sol_usdc}`
    // pushes the chain-wide mark price + implied vol continuously (a full snapshot on subscribe, then
    // changed-only deltas), so the grid's IV / mark / greeks stay live between the OPT_POLL_SECS REST
    // snapshots instead of only refreshing on the poll. Each batch folds onto the SAME shared grid
    // the poll writes, under the same lock + `wake`. Detached like the other fetch threads (the feed
    // owns its own stop; process exit reaps it). BTC/ETH are coin-settled, SOL rides the USDC book
    // (usd-quoted) — the fold's `is_usd_quoted` scale handles both, matching the REST path.
    let _markprice_feed = vike_deribit::options_feed::spawn_deribit_markprice_options_feed(
        vike_deribit::options_feed::MAINNET_WS.to_string(),
        vec!["btc_usd".to_string(), "eth_usd".to_string(), "sol_usdc".to_string()],
        move |rows| {
            let n = {
                let mut s = state.lock().unwrap();
                apply_markprice_to_chains(
                    &mut s.opt_by_underlying,
                    &rows,
                    vike_model::now_ms(),
                    0.0,
                )
            };
            if n > 0 {
                wake();
            }
        },
    );

    opt_refresh_tx
}

/// A prefilled options order ticket awaiting the user's Confirm. Built from a chain bid/ask cell
/// click (see `vike_chart::OptionOrderClick`); the order is submitted to the deribit exec engine
/// ONLY when the user clicks Confirm in the modal — NEVER on the chain click that opened it.
/// `side` is vike's `i32` order side (+1 Buy / −1 Sell); `price`/`qty` are editable in the modal.
#[derive(Debug, Clone, PartialEq)]
pub struct OptOrderTicket {
    pub instrument: String,
    pub side: i32,
    pub price: f64,
    pub qty: f64,
    pub is_call: bool,
    pub strike: f64,
}

/// Per-window UI state for the tool windows (selection/filters/page/expiry). This is VIEW state,
/// not fetched data, so it lives with the tools — NOT in `workspace::WinState` (which stays free
/// of app content) and not in the shared (fetch-thread-owned) ToolData. The App owns one per tool
/// window, keyed by window id; the window loop takes it out of the map before show_window and
/// writes it back after (the same mutate-around-the-closure pattern the chart windows use).
/// A Trade-ticket order the App drains into a `vike_model::OrderRequest` (via
/// [`crate::order_entry::build_order_request`]). Carries the order type + optional limit/stop
/// prices the ticket now supports, beyond the original market-only (side, qty, reduce_only).
#[derive(Debug, Clone, PartialEq)]
pub struct TradeSubmit {
    pub side: i32,
    pub qty: f64,
    pub reduce_only: bool,
    pub kind: crate::order_entry::OrderKind,
    /// limit price (Limit orders)
    pub price: Option<f64>,
    /// stop trigger price (Stop orders)
    pub trigger_price: Option<f64>,
    /// take-profit price — when both `tp` and `sl` are set on an ENTRY the App dispatches an
    /// `OrderIntent::Bracket` (OTO entry + OCO exits) instead of a plain order.
    pub tp: Option<f64>,
    /// stop-loss price (bracket exit)
    pub sl: Option<f64>,
}

#[derive(Clone)]
pub struct ToolView {
    // Options
    pub opt_underlying_sel: Option<String>, // None = follow the first fetched underlying (BTC)
    pub opt_expiry_sel: Option<String>,     // None = follow the nearest expiry
    pub opt_strike_window: usize,           // display half-window (±N strike rows around ATM)
    // Options order ticket (Deribit chain-click → confirm → deribit exec). `opt_order_ticket` is
    // the modal's live, editable state (set on a bid/ask cell click, cleared on Confirm/Cancel);
    // `opt_submit` is the OUT slot the App drains AFTER the user clicks Confirm — the only path to
    // an actual order.
    pub opt_order_ticket: Option<OptOrderTicket>,
    pub opt_submit: Option<OptOrderTicket>, // OUT: confirmed order — App builds the OrderRequest
    pub opt_cancel: Option<String>, // OUT: cancel this deribit client_order_id (chain marker click)
    // Tearsheet (live performance summary read from the command journal). Rendered rows are cached
    // (a journal read + trade reconstruction is not a per-frame cost); the App recomputes them only
    // on first show and on an explicit Refresh. Held as primitive (label, value) pairs so the render
    // path stays formatting-free. `tool_views::tearsheet` computes the `vike_report::LiveTearsheet`
    // and fills these (it moved down here in tool-view extraction batch 1).
    pub ts_seed: f64, // starting-capital base for the realized equity curve
    pub ts_rows: Vec<(String, String)>, // cached rendered rows; empty until loaded
    pub ts_err: Option<String>, // load error, if any (shown in place of the table)
    pub ts_loaded: bool, // whether a load has been attempted (gates auto-load-on-open)
    // News
    pub news_sel: usize,
    pub news_query: String,
    pub news_markets: std::collections::HashSet<String>,
    pub news_categories: std::collections::HashSet<String>,
    pub news_providers: std::collections::HashSet<String>,
    pub news_follow: bool,
    pub news_reader_open: bool,
    pub news_open_url: Option<String>, // OUT: app opens this URL after the frame
    // Calendar
    pub cal_page: u8,         // 0=Economic 1=Earnings 2=Dividends 3=IPO
    pub cal_selected_day: i8, // -1 = none
    pub cal_high_only: bool,
    // Data
    pub data_subtab: usize, // 0=Symbols 1=Cached Series 2..providers 5=Stored 6=Venues
    pub data_sel: Option<String>, // selected cached-series key
    pub data_delete: Option<String>, // OUT: app drops this feed after the frame
    pub data_log: Vec<String>, // activity log lines (newest last)
    /// The Venues sub-tab's per-venue arming flow — at most ONE venue in flight, per window.
    /// `Idle` is the whole of it until the operator clicks a mode; see
    /// [`crate::tool_views::ArmEdit`].
    pub arm_edit: crate::tool_views::ArmEdit,
    // Connections
    /// A PENDING one-shot account preselection for the Connections panel's chip strip, seeded by
    /// `crate::startup::StartupLayout::connections_account` (the `connections-account` QA capture
    /// arm) and consumed — `take`n — by the first frame of `tool_views::connections_tool_content`.
    ///
    /// ⚠ `take`, not read: the panel's own selection lives in the widget's egui temp memory, so a
    /// value that stayed here would be re-applied every frame and would silently undo an
    /// operator's chip click. `None` is the ordinary state and the whole of it on any launch that
    /// did not ask for a preselection.
    pub connections_account: Option<vike_model::account_keys::AccountLabel>,
    // Stored (Data Manager "Stored" tab, Task 3) — HistStore inventory tree actions. The tree
    // data itself is shared App state (`App::stored_tree`, like `symbols_catalog`); only this
    // window's transient actions/confirm-modal state lives here.
    pub stored_auto_requested: bool, // one-shot: the first-shown load has already been requested
    pub stored_refresh: bool,        // OUT: (re)run the background inventory load
    /// The last series row clicked in the shared tree: (venue, symbol, kind, interval). Set on
    /// every `StoredSelection` (the tree's single click affordance doubles as "Open" AND as the
    /// target for the Delete control kept app-side, next to Refresh).
    pub stored_last_sel: Option<(String, String, String, Option<String>)>,
    /// Pending delete awaiting the confirm modal's click: (venue, symbol, kind, interval).
    pub stored_confirm_delete: Option<(String, String, String, Option<String>)>,
    /// OUT: confirmed delete — App reconstructs the `SeriesId` and calls `delete_series`.
    pub stored_delete: Option<(String, String, String, Option<String>)>,
    /// OUT: Open-in-chart — (venue, symbol, interval); App spawns/points a chart window at it.
    pub stored_open: Option<(String, String, Option<String>)>,
    /// Data Manager grid v2 (dm-gridv2): persisted state for
    /// `vike_data_manager::stored_catalog_grid` + its `views_sidebar` companion — query, sort,
    /// multi-select, and the active left-nav view, all in one place so the sidebar and grid render
    /// off the identical `active_view`.
    pub stored_grid: vike_data_manager::GridState,

    /// The **Polymarket proxy** box's edit buffer. Seeded once per window-open from the value
    /// actually in force (see `stored_proxy_loaded`), then owned by the operator's typing.
    pub stored_proxy: vike_data_manager::ProxyEdit,

    /// One-shot: has `stored_proxy` been seeded from the resolver yet? Without this the box would
    /// be re-filled from disk every frame and the operator could never type into it.
    pub stored_proxy_loaded: bool,

    /// OUT: the operator clicked Save on the proxy box. Carries the value to write to
    /// `POLY_SOCKS_PROXY`, already in stored form (`none` for a cleared box). `App` performs the
    /// credential-store write and clears this — the view does no I/O, like every `stored_*` action
    /// above it.
    pub stored_proxy_save: Option<String>,
    /// Pending bulk delete awaiting the confirm modal: the grid's `selected` set at the moment
    /// `BulkAction::Delete` fired (snapshotted so a selection change while the modal is open can't
    /// silently widen/narrow what gets deleted).
    pub stored_confirm_bulk_delete: Option<Vec<vike_data_manager::SeriesKey>>,
    /// OUT: confirmed bulk delete — App reconstructs each `SeriesId` and calls `delete_series`,
    /// same per-key path as the single-row `stored_delete`.
    pub stored_bulk_delete: Vec<vike_data_manager::SeriesKey>,
    /// OUT (dm-bulk-backfill): the grid's `selected` set at the moment `BulkAction::Backfill` OR
    /// `BulkAction::Update` fired — both alias to the same MVP path (see
    /// `App::maybe_spawn_stored_backfill`'s doc for why there's no separate "to-now" mode yet).
    /// Unlike bulk delete this needs no confirm modal (non-destructive, network-only), so it's
    /// drained straight into the App's backfill spawn, same deferred-mutation shape as
    /// `stored_bulk_delete`.
    pub stored_backfill: Vec<vike_data_manager::SeriesKey>,
    // DataSets (Symbols tab) — the editor working copy + OUT actions the App applies
    pub ds_sel: Option<String>, // selected DataSet name (None until first load)
    pub ds_name: String,        // form: name
    pub ds_provider: String,    // form: provider
    pub ds_interval: String,    // form: interval
    pub ds_benchmark: String,   // form: benchmark
    pub ds_symbols_text: String, // form: free-text symbols
    pub ds_sym_sel: Option<String>, // "select one to Test" list pick
    pub ds_ai_prompt: String,   // "Ask the AI" prompt
    pub ds_save: bool,          // OUT: persist the working copy
    pub ds_delete: Option<String>, // OUT: delete this DataSet
    pub ds_test: Option<String>, // OUT: open a chart for this symbol
    // Trade (R7.5) — paper order ticket
    pub trade_qty: String,                               // form: order quantity
    pub trade_size_mode: crate::trade_sizing::SizeMode,  // form: which unit the size field is in
    pub trade_leverage: f64,                             // form: perp leverage (1.0 = spot)
    pub trade_margin_isolated: bool,                     // form: isolated (true) vs cross (false)
    pub trade_lev_open: bool,                            // UI: leverage popover expanded
    pub trade_order_kind: crate::order_entry::OrderKind, // form: Market / Limit / Stop
    pub trade_price: String,                             // form: limit price (Limit)
    pub trade_trigger: String,                           // form: stop trigger price (Stop)
    pub trade_tpsl_on: bool,                             // form: attach TP/SL bracket
    pub trade_tp: String,                                // form: take-profit price
    pub trade_sl: String,                                // form: stop-loss price
    pub trade_submit: Option<TradeSubmit>, // OUT: the ticket order — App builds the command
    pub trade_cancel: Option<String>,      // OUT: cancel this client_order_id
    pub trade_set_margin: Option<(String, String, f64)>, // OUT: (venue, symbol, im=1/lev) — SetMargin
    // DOM (Pro/Elite ladder) — per-window widget state + this-frame actions the App drains
    pub dom: vike_panels::DomState,
    pub dom_actions: Vec<vike_panels::DomAction>, // OUT: click-trade intents — App maps to Commands
    // Polymarket cockpit (WinKind::Polymarket) — per-window widget view state for the four
    // `vike-cockpit` widgets, plus this-frame order intents the App drains onto the command lane
    // (same seam as `dom`/`dom_actions`). The chain rail latches its window selection into
    // `cockpit_chain`; the header is a stateless readout (no state field).
    pub cockpit_chain: vike_cockpit::ChainRailState, // window-chain rail selection
    pub cockpit_ladder: vike_cockpit::ProbLadderState, // probability-ladder hover/selection
    pub cockpit_ticket: vike_cockpit::TicketState,   // arm switch + stake
    /// OUT: probability-ladder click-trade intents (rest-a-limit / cancel) — App maps to Commands.
    pub cockpit_ladder_actions: Vec<vike_cockpit::ProbLadderAction>,
    /// OUT: one-click-ticket intents (BuyUp/BuyDown/size/arm) — App maps the market buys to Commands.
    pub cockpit_ticket_actions: Vec<vike_cockpit::TicketAction>,
}

impl Default for ToolView {
    fn default() -> Self {
        Self {
            opt_underlying_sel: None,
            opt_expiry_sel: None,
            opt_strike_window: 12,
            opt_order_ticket: None,
            opt_submit: None,
            opt_cancel: None,
            ts_seed: 10_000.0,
            ts_rows: Vec::new(),
            ts_err: None,
            ts_loaded: false,
            news_sel: 0,
            news_query: String::new(),
            news_markets: std::collections::HashSet::new(),
            news_categories: std::collections::HashSet::new(),
            news_providers: std::collections::HashSet::new(),
            news_follow: true,
            news_reader_open: true,
            news_open_url: None,
            cal_page: 0,
            cal_selected_day: -1,
            cal_high_only: false,
            data_subtab: 0, // Symbols (matches vike's default tab — shows the DataSet tree + editor)
            data_sel: None,
            data_delete: None,
            data_log: Vec::new(),
            arm_edit: crate::tool_views::ArmEdit::Idle,
            connections_account: None,
            stored_auto_requested: false,
            stored_refresh: false,
            stored_last_sel: None,
            stored_confirm_delete: None,
            stored_delete: None,
            stored_open: None,
            stored_grid: vike_data_manager::GridState::default(),
            stored_proxy: vike_data_manager::ProxyEdit::default(),
            stored_proxy_loaded: false,
            stored_proxy_save: None,
            stored_confirm_bulk_delete: None,
            stored_bulk_delete: Vec::new(),
            stored_backfill: Vec::new(),
            ds_sel: None,
            ds_name: String::new(),
            ds_provider: String::new(),
            ds_interval: String::new(),
            ds_benchmark: String::new(),
            ds_symbols_text: String::new(),
            ds_sym_sel: None,
            ds_ai_prompt: String::new(),
            ds_save: false,
            ds_delete: None,
            ds_test: None,
            trade_qty: "0.001".to_string(),
            trade_size_mode: crate::trade_sizing::SizeMode::Qty,
            trade_leverage: 1.0,
            trade_margin_isolated: false,
            trade_lev_open: false,
            trade_order_kind: crate::order_entry::OrderKind::Market,
            trade_price: String::new(),
            trade_trigger: String::new(),
            trade_tpsl_on: false,
            trade_tp: String::new(),
            trade_sl: String::new(),
            trade_submit: None,
            trade_cancel: None,
            trade_set_margin: None,
            dom: vike_panels::DomState::default(),
            dom_actions: Vec::new(),
            cockpit_chain: vike_cockpit::ChainRailState::default(),
            cockpit_ladder: vike_cockpit::ProbLadderState::default(),
            cockpit_ticket: vike_cockpit::TicketState::default(),
            cockpit_ladder_actions: Vec::new(),
            cockpit_ticket_actions: Vec::new(),
        }
    }
}

#[cfg(test)]
mod option_selection_tests {
    use super::*;
    use std::collections::BTreeMap;
    use vike_options::{AssetClass, Expiry, OptionChain};

    /// Minimal chain for an (underlying, expiry) — only the keys/fields the selection logic reads.
    fn chain(underlying: &str, date: &str) -> OptionChain {
        OptionChain {
            underlying: underlying.to_string(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(100.0),
            expiry: Expiry { date: date.to_string(), dte: 1, label: date.to_string() },
            asof_ms: 0,
            source: "deribit".into(),
            rows: Vec::new(),
        }
    }

    fn bundle(underlying: &str, expiries: &[&str]) -> UnderlyingChains {
        let mut chains = BTreeMap::new();
        let mut exps = Vec::new();
        for (i, d) in expiries.iter().enumerate() {
            chains.insert(d.to_string(), chain(underlying, d));
            exps.push(Expiry { date: d.to_string(), dte: i as i64, label: d.to_string() });
        }
        UnderlyingChains {
            default_expiry: expiries.first().map(|s| s.to_string()).unwrap_or_default(),
            expiries: exps,
            chains,
        }
    }

    fn books() -> BTreeMap<String, UnderlyingChains> {
        let mut m = BTreeMap::new();
        m.insert("BTC".into(), bundle("BTC", &["2026-07-17", "2026-07-24"]));
        m.insert("ETH".into(), bundle("ETH", &["2026-07-18", "2026-07-25"]));
        m.insert("SOL".into(), bundle("SOL", &["2026-07-19"]));
        m
    }

    #[test]
    fn defaults_to_first_underlying_when_unset() {
        let by = books();
        let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
        let mut expiry = None;
        let sel = resolve_options_selection(&by, &order, &None, &mut expiry).unwrap();
        assert_eq!(sel.default_expiry, "2026-07-17", "BTC bundle");
        assert_eq!(expiry, None, "no expiry pick, nothing to reset");
    }

    #[test]
    fn honors_a_valid_underlying_selection() {
        // Pre-selecting ETH (the VIKE_SHOT path) renders ETH's book.
        let by = books();
        let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
        let mut expiry = None;
        let sel = resolve_options_selection(&by, &order, &Some("ETH".into()), &mut expiry).unwrap();
        assert_eq!(sel.default_expiry, "2026-07-18", "ETH bundle");
        assert_eq!(sel.default_expiry, "2026-07-18");
    }

    #[test]
    fn unknown_underlying_falls_back_to_first() {
        let by = books();
        let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
        let mut expiry = None;
        // "DOGE" never fetched → fall back to the first fetched underlying.
        let sel =
            resolve_options_selection(&by, &order, &Some("DOGE".into()), &mut expiry).unwrap();
        assert_eq!(sel.default_expiry, "2026-07-17", "BTC bundle");
    }

    #[test]
    fn resets_expiry_when_invalid_for_new_underlying() {
        // Switching BTC→ETH while a BTC-only expiry is selected clears it so ETH's default shows.
        let by = books();
        let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
        let mut expiry = Some("2026-07-17".to_string()); // a BTC expiry, absent from ETH
        let sel = resolve_options_selection(&by, &order, &Some("ETH".into()), &mut expiry).unwrap();
        assert_eq!(sel.default_expiry, "2026-07-18", "ETH bundle");
        assert_eq!(expiry, None, "the BTC expiry must reset so ETH's default takes over");
    }

    #[test]
    fn keeps_expiry_that_is_valid_for_selected_underlying() {
        let by = books();
        let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
        let mut expiry = Some("2026-07-24".to_string()); // a real BTC expiry
        resolve_options_selection(&by, &order, &Some("BTC".into()), &mut expiry).unwrap();
        assert_eq!(expiry, Some("2026-07-24".to_string()), "a valid pick is preserved");
    }

    #[test]
    fn none_when_nothing_fetched() {
        let by: BTreeMap<String, UnderlyingChains> = BTreeMap::new();
        let order: Vec<String> = Vec::new();
        let mut expiry = None;
        assert!(resolve_options_selection(&by, &order, &Some("ETH".into()), &mut expiry).is_none());
    }

    #[test]
    fn canonical_order_ranks_btc_eth_sol() {
        let mut order = vec!["SOL".to_string(), "BTC".into(), "ETH".into()];
        order.sort_by_key(|u| underlying_rank(u));
        assert_eq!(order, vec!["BTC", "ETH", "SOL"]);
    }
}

/// The pure `markprice.options` fold onto the live grid ([`apply_markprice_to_chains`]) — the WS
/// streaming half exercised without a socket. Pins the wire-unit contract (coin→USD mark, verbatim
/// decimal IV, greeks re-enriched) and the skip-what-isn't-in-the-grid discipline.
#[cfg(test)]
mod markprice_apply_tests {
    use super::*;
    use std::collections::BTreeMap;
    use vike_deribit::options_feed::MarkPriceRow;
    use vike_options::{AssetClass, Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow};

    const NOW: i64 = 1_780_387_200_000; // 2026-06-02 08:00 UTC (chain.rs's NOW)

    /// A one-strike BTC chain for 2026-06-27 (call+put), mimicking a REST-seeded grid the WS stream
    /// then updates. Spot 100k; each quote carries a stale mark/iv the stream overwrites.
    fn seeded_btc() -> BTreeMap<String, UnderlyingChains> {
        let mk = |name: &str, kind: OptionKind| OptionQuote {
            mark: Some(1.0),
            iv: Some(0.10),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(100_000.0, kind)
        };
        let chain = OptionChain {
            underlying: "BTC".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(100_000.0),
            expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
            asof_ms: NOW,
            source: "deribit".into(),
            rows: vec![StrikeRow {
                strike: 100_000.0,
                call: Some(mk("BTC-27JUN26-100000-C", OptionKind::Call)),
                put: Some(mk("BTC-27JUN26-100000-P", OptionKind::Put)),
            }],
        };
        let mut chains = BTreeMap::new();
        chains.insert("2026-06-27".to_string(), chain);
        let bundle = UnderlyingChains {
            default_expiry: "2026-06-27".into(),
            expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
            chains,
        };
        let mut by = BTreeMap::new();
        by.insert("BTC".to_string(), bundle);
        by
    }

    #[test]
    fn folds_mark_usd_scaled_iv_verbatim_and_reenriches_greeks() {
        let mut by = seeded_btc();
        let rows = vec![MarkPriceRow {
            instrument_name: "BTC-27JUN26-100000-C".into(),
            mark_price: 0.05, // COIN units → USD = 0.05 * 100_000
            iv: 0.62,         // DECIMAL, stored verbatim
        }];
        let n = apply_markprice_to_chains(&mut by, &rows, NOW, 0.0);
        assert_eq!(n, 1);
        let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
        assert_eq!(call.mark, Some(0.05 * 100_000.0), "coin→USD scaled by spot");
        assert_eq!(call.iv, Some(0.62), "decimal IV stored verbatim (no ÷100)");
        assert!(call.delta.is_some(), "greeks re-enriched from the fresh IV");
        // the put had no row this batch → untouched
        let put = by["BTC"].chains["2026-06-27"].rows[0].put.as_ref().unwrap();
        assert_eq!(put.iv, Some(0.10), "unrelated quote unchanged");
    }

    #[test]
    fn skips_unknown_instrument_underlying_and_strike() {
        let mut by = seeded_btc();
        let rows = vec![
            // wrong underlying (never fetched)
            MarkPriceRow { instrument_name: "ETH-27JUN26-3000-C".into(), mark_price: 0.1, iv: 0.5 },
            // right underlying+expiry, strike outside the grid
            MarkPriceRow {
                instrument_name: "BTC-27JUN26-999000-C".into(),
                mark_price: 0.1,
                iv: 0.5,
            },
            // wrong expiry (not in chains)
            MarkPriceRow {
                instrument_name: "BTC-25SEP26-100000-C".into(),
                mark_price: 0.1,
                iv: 0.5,
            },
            // a non-option instrument — parse_instrument_name rejects it
            MarkPriceRow { instrument_name: "BTC-PERPETUAL".into(), mark_price: 0.1, iv: 0.5 },
        ];
        assert_eq!(apply_markprice_to_chains(&mut by, &rows, NOW, 0.0), 0, "nothing matched");
        let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
        assert_eq!(call.mark, Some(1.0), "seeded quote untouched");
        assert_eq!(call.iv, Some(0.10));
    }

    #[test]
    fn absent_spot_yields_absent_mark_not_zero() {
        let mut by = seeded_btc();
        by.get_mut("BTC").unwrap().chains.get_mut("2026-06-27").unwrap().underlying_price = None;
        let rows = vec![MarkPriceRow {
            instrument_name: "BTC-27JUN26-100000-C".into(),
            mark_price: 0.05,
            iv: 0.62,
        }];
        assert_eq!(apply_markprice_to_chains(&mut by, &rows, NOW, 0.0), 1);
        let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
        assert_eq!(call.mark, None, "no spot → absent mark, never a fabricated 0.0");
        assert_eq!(call.iv, Some(0.62), "IV still updates (spot-independent)");
    }

    #[test]
    fn sol_usdc_mark_is_usd_quoted_not_spot_scaled() {
        // SOL rides the shared USDC book: its markprice `mark_price` is ALREADY USD, so the fold must
        // NOT scale it by spot (the bug adding SOL naively would introduce — a $0.25 premium would
        // become ~$18). parse_instrument_name strips the _USDC suffix → base "SOL" → is_usd_quoted →
        // scale 1.0. Verified live: streamed 0.2477 ≈ REST 0.2463 at SOL spot 75.64.
        let seeded = OptionQuote {
            mark: Some(1.0),
            iv: Some(0.10),
            instrument_name: Some("SOL_USDC-25SEP26-45-P".into()),
            ..OptionQuote::new(45.0, OptionKind::Put)
        };
        let chain = OptionChain {
            underlying: "SOL".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(75.64),
            expiry: Expiry { date: "2026-09-25".into(), dte: 90, label: "25 Sep".into() },
            asof_ms: NOW,
            source: "deribit".into(),
            rows: vec![StrikeRow { strike: 45.0, call: None, put: Some(seeded) }],
        };
        let mut chains = BTreeMap::new();
        chains.insert("2026-09-25".to_string(), chain);
        let mut by = BTreeMap::new();
        by.insert(
            "SOL".to_string(),
            UnderlyingChains {
                default_expiry: "2026-09-25".into(),
                expiries: vec![Expiry {
                    date: "2026-09-25".into(),
                    dte: 90,
                    label: "25 Sep".into(),
                }],
                chains,
            },
        );
        let rows = vec![MarkPriceRow {
            instrument_name: "SOL_USDC-25SEP26-45-P".into(),
            mark_price: 0.2477, // ALREADY USD — must pass through, NOT × 75.64
            iv: 0.7115,
        }];
        assert_eq!(apply_markprice_to_chains(&mut by, &rows, NOW, 0.0), 1);
        let put = by["SOL"].chains["2026-09-25"].rows[0].put.as_ref().unwrap();
        assert_eq!(put.mark, Some(0.2477), "USDC premium passed through unscaled (no × spot)");
        assert_eq!(put.iv, Some(0.7115));
        assert!(put.delta.is_some(), "greeks re-enriched from spot + IV");
    }
}

/// The pure `ticker.{inst}.100ms` fold onto the live grid ([`apply_ticker_to_chains`]) — the bid/ask
/// streaming half exercised without a socket. Pins the wire-unit contract (coin→USD bid/ask/mark,
/// PERCENT→decimal IV, greeks re-enriched), the mark/iv-refine-only-when-carried policy, and the
/// skip-what-isn't-in-the-grid discipline.
#[cfg(test)]
mod ticker_apply_tests {
    use super::*;
    use std::collections::BTreeMap;
    use vike_deribit::options_feed::TickerRow;
    use vike_options::{AssetClass, Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow};

    const NOW: i64 = 1_780_387_200_000; // 2026-06-02 08:00 UTC (chain.rs's NOW)

    /// A one-strike BTC chain for 2026-06-27 (call+put), mimicking a REST-seeded grid the ticker then
    /// updates. Spot 100k; each quote carries a stale bid/ask/mark/iv the stream overwrites.
    fn seeded_btc() -> BTreeMap<String, UnderlyingChains> {
        let mk = |name: &str, kind: OptionKind| OptionQuote {
            bid: Some(9.9),
            ask: Some(9.9),
            mark: Some(1.0),
            iv: Some(0.10),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(100_000.0, kind)
        };
        let chain = OptionChain {
            underlying: "BTC".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(100_000.0),
            expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
            asof_ms: NOW,
            source: "deribit".into(),
            rows: vec![StrikeRow {
                strike: 100_000.0,
                call: Some(mk("BTC-27JUN26-100000-C", OptionKind::Call)),
                put: Some(mk("BTC-27JUN26-100000-P", OptionKind::Put)),
            }],
        };
        let mut chains = BTreeMap::new();
        chains.insert("2026-06-27".to_string(), chain);
        let bundle = UnderlyingChains {
            default_expiry: "2026-06-27".into(),
            expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
            chains,
        };
        let mut by = BTreeMap::new();
        by.insert("BTC".to_string(), bundle);
        by
    }

    /// A full ticker row for `name`. `underlying_price` is deliberately DIFFERENT from the seeded
    /// chain spot (100k) to prove the fold scales by the CHAIN spot, not this per-row value.
    fn row(name: &str) -> TickerRow {
        TickerRow {
            instrument_name: name.into(),
            best_bid: Some(0.1015),
            best_ask: Some(0.106),
            mark_price: Some(0.1036),
            mark_iv: Some(66.33),
            open_interest: Some(1.0),
            volume: Some(2.0),
            underlying_price: Some(99_000.0),
        }
    }

    #[test]
    fn folds_bidask_usd_scaled_iv_percent_and_reenriches_greeks() {
        let mut by = seeded_btc();
        let n = apply_ticker_to_chains(&mut by, &[row("BTC-27JUN26-100000-C")], NOW, 0.0);
        assert_eq!(n, 1);
        let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
        // coin → USD by the CHAIN spot (100k), NOT the ticker's own underlying_price (99k)
        assert_eq!(call.bid, Some(0.1015 * 100_000.0), "bid coin→USD by chain spot");
        assert_eq!(call.ask, Some(0.106 * 100_000.0), "ask coin→USD by chain spot");
        assert_eq!(call.mark, Some(0.1036 * 100_000.0), "mark coin→USD");
        assert_eq!(call.iv, Some(0.6633), "mark_iv PERCENT → decimal (÷100)");
        assert!(call.delta.is_some(), "greeks re-enriched from the fresh IV");
        // the put had no row this batch → untouched
        let put = by["BTC"].chains["2026-06-27"].rows[0].put.as_ref().unwrap();
        assert_eq!(put.iv, Some(0.10), "unrelated quote unchanged");
        assert_eq!(put.bid, Some(9.9));
    }

    #[test]
    fn skips_unknown_instrument_underlying_and_strike() {
        let mut by = seeded_btc();
        let rows = vec![
            row("ETH-27JUN26-3000-C"),   // underlying never fetched
            row("BTC-27JUN26-999000-C"), // right underlying+expiry, strike outside the grid
            row("BTC-25SEP26-100000-C"), // wrong expiry (not in chains)
            row("BTC-PERPETUAL"),        // not an option — parse_instrument_name rejects it
        ];
        assert_eq!(apply_ticker_to_chains(&mut by, &rows, NOW, 0.0), 0, "nothing matched");
        let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
        assert_eq!(call.bid, Some(9.9), "seeded quote untouched");
    }

    #[test]
    fn absent_spot_yields_absent_bidask_not_zero() {
        let mut by = seeded_btc();
        by.get_mut("BTC").unwrap().chains.get_mut("2026-06-27").unwrap().underlying_price = None;
        assert_eq!(apply_ticker_to_chains(&mut by, &[row("BTC-27JUN26-100000-C")], NOW, 0.0), 1);
        let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
        assert_eq!(call.bid, None, "no spot → absent bid, never a fabricated 0.0");
        assert_eq!(call.ask, None);
        assert_eq!(call.mark, None);
        assert_eq!(call.iv, Some(0.6633), "IV still updates (spot-independent)");
    }

    #[test]
    fn mark_and_iv_refined_only_when_carried() {
        // a sparse ticker (bid/ask only, no mark/iv) updates bid/ask but must NOT wipe a good mark/iv
        // (the markprice.options feed is their primary source).
        let mut by = seeded_btc();
        let sparse = TickerRow {
            instrument_name: "BTC-27JUN26-100000-C".into(),
            best_bid: Some(0.2),
            best_ask: Some(0.21),
            mark_price: None,
            mark_iv: None,
            open_interest: None,
            volume: None,
            underlying_price: None,
        };
        assert_eq!(apply_ticker_to_chains(&mut by, &[sparse], NOW, 0.0), 1);
        let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
        assert_eq!(call.bid, Some(0.2 * 100_000.0), "bid updated");
        assert_eq!(call.mark, Some(1.0), "mark preserved (ticker carried none)");
        assert_eq!(call.iv, Some(0.10), "iv preserved (ticker carried none)");
    }

    #[test]
    fn sol_usdc_bidask_is_usd_quoted_not_spot_scaled() {
        // SOL rides the shared USDC book: its ticker bid/ask are ALREADY USD, so the fold must NOT
        // scale by spot (parse_instrument_name strips _USDC → base "SOL" → is_usd_quoted → scale 1.0).
        let seeded = OptionQuote {
            bid: Some(9.9),
            ask: Some(9.9),
            mark: Some(1.0),
            iv: Some(0.10),
            instrument_name: Some("SOL_USDC-25SEP26-45-P".into()),
            ..OptionQuote::new(45.0, OptionKind::Put)
        };
        let chain = OptionChain {
            underlying: "SOL".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(75.64),
            expiry: Expiry { date: "2026-09-25".into(), dte: 90, label: "25 Sep".into() },
            asof_ms: NOW,
            source: "deribit".into(),
            rows: vec![StrikeRow { strike: 45.0, call: None, put: Some(seeded) }],
        };
        let mut chains = BTreeMap::new();
        chains.insert("2026-09-25".to_string(), chain);
        let mut by = BTreeMap::new();
        by.insert(
            "SOL".to_string(),
            UnderlyingChains {
                default_expiry: "2026-09-25".into(),
                expiries: vec![Expiry {
                    date: "2026-09-25".into(),
                    dte: 90,
                    label: "25 Sep".into(),
                }],
                chains,
            },
        );
        let rows = vec![TickerRow {
            instrument_name: "SOL_USDC-25SEP26-45-P".into(),
            best_bid: Some(0.24), // ALREADY USD — must pass through, NOT × 75.64
            best_ask: Some(0.26),
            mark_price: Some(0.25),
            mark_iv: Some(71.15),
            open_interest: Some(1.0),
            volume: Some(1.0),
            underlying_price: Some(75.64),
        }];
        assert_eq!(apply_ticker_to_chains(&mut by, &rows, NOW, 0.0), 1);
        let put = by["SOL"].chains["2026-09-25"].rows[0].put.as_ref().unwrap();
        assert_eq!(put.bid, Some(0.24), "USDC bid passed through unscaled (no × spot)");
        assert_eq!(put.ask, Some(0.26));
        assert_eq!(put.mark, Some(0.25));
        assert_eq!(put.iv, Some(0.7115));
        assert!(put.delta.is_some(), "greeks re-enriched from spot + IV");
    }
}

/// The front-expiry ATM focus-set selection ([`front_expiry_focus_instruments`]) — the pure grid→ids
/// half of the ticker wiring. Pins the ±n ATM window (both call+put ids), the front-only scope, and
/// the skip-what-has-no-spot discipline.
#[cfg(test)]
mod focus_set_tests {
    use super::*;
    use std::collections::BTreeMap;
    use vike_options::{AssetClass, Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow};

    const NOW: i64 = 1_780_387_200_000;

    fn opt(name: &str, k: f64, call: bool) -> OptionQuote {
        OptionQuote {
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(k, if call { OptionKind::Call } else { OptionKind::Put })
        }
    }

    fn srow(prefix: &str, k: f64) -> StrikeRow {
        StrikeRow {
            strike: k,
            call: Some(opt(&format!("{prefix}-{}-C", k as i64), k, true)),
            put: Some(opt(&format!("{prefix}-{}-P", k as i64), k, false)),
        }
    }

    fn chain(date: &str, spot: f64, strikes: &[f64], prefix: &str) -> OptionChain {
        OptionChain {
            underlying: "BTC".into(),
            asset_class: AssetClass::Crypto,
            underlying_price: Some(spot),
            expiry: Expiry { date: date.into(), dte: 25, label: date.into() },
            asof_ms: NOW,
            source: "deribit".into(),
            rows: strikes.iter().map(|&k| srow(prefix, k)).collect(),
        }
    }

    /// A BTC bundle: a 6-strike FRONT expiry (spot 100_500, between 100k and 101k) + a LATER expiry
    /// the front-only focus must ignore.
    fn by_btc() -> BTreeMap<String, UnderlyingChains> {
        let mut chains = BTreeMap::new();
        chains.insert(
            "2026-06-27".to_string(),
            chain(
                "2026-06-27",
                100_500.0,
                &[98_000.0, 99_000.0, 100_000.0, 101_000.0, 102_000.0, 103_000.0],
                "BTC-27JUN26",
            ),
        );
        chains.insert(
            "2026-07-25".to_string(),
            chain("2026-07-25", 100_500.0, &[100_000.0], "BTC-25JUL26"),
        );
        let bundle = UnderlyingChains {
            default_expiry: "2026-06-27".into(), // nearest DTE
            expiries: vec![
                Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
                Expiry { date: "2026-07-25".into(), dte: 53, label: "25 Jul".into() },
            ],
            chains,
        };
        let mut by = BTreeMap::new();
        by.insert("BTC".to_string(), bundle);
        by
    }

    #[test]
    fn atm_window_collects_both_sides_at_each_strike() {
        // ±1 around spot 100_500 (split at 101_000): strikes [100_000, 101_000], call+put each.
        let ids = front_expiry_focus_instruments(&by_btc(), &["BTC"], 1);
        assert_eq!(
            ids,
            vec![
                "BTC-27JUN26-100000-C",
                "BTC-27JUN26-100000-P",
                "BTC-27JUN26-101000-C",
                "BTC-27JUN26-101000-P",
            ]
        );
    }

    #[test]
    fn only_front_expiry_never_the_later_one() {
        // a window wider than the chain returns ALL front strikes and NONE of the later expiry.
        let ids = front_expiry_focus_instruments(&by_btc(), &["BTC"], 30);
        assert_eq!(ids.len(), 12, "6 front strikes × call+put");
        assert!(
            ids.iter().all(|i| i.starts_with("BTC-27JUN26-")),
            "only the front expiry: {ids:?}"
        );
        assert!(!ids.iter().any(|i| i.contains("25JUL26")), "the later expiry is excluded");
    }

    #[test]
    fn skips_underlying_without_spot_or_without_fetch() {
        let mut by = by_btc();
        by.get_mut("BTC").unwrap().chains.get_mut("2026-06-27").unwrap().underlying_price = None;
        assert!(front_expiry_focus_instruments(&by, &["BTC"], 10).is_empty(), "no spot → skipped");
        // an underlying that never fetched is simply skipped (empty, no panic).
        assert!(front_expiry_focus_instruments(&by_btc(), &["DOGE"], 10).is_empty());
    }
}

/// The options poll thread's wake mechanism: a Refresh signal wakes it early (before the
/// OPT_POLL_SECS cadence), and a burst of clicks coalesces to a single refetch. Exercises the exact
/// `recv_timeout` + drain pattern the poll loop uses (without spawning the network-bound loop).
#[cfg(test)]
mod refresh_wake_tests {
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::time::{Duration, Instant};

    #[test]
    fn signal_wakes_before_the_30s_cadence() {
        let (tx, rx) = channel::<()>();
        tx.send(()).unwrap(); // Refresh pill clicked
        let t0 = Instant::now();
        // Uses the same 30s timeout the poll loop does; the queued signal must return immediately.
        assert!(matches!(rx.recv_timeout(Duration::from_secs(30)), Ok(())));
        assert!(t0.elapsed() < Duration::from_secs(1), "woke on the signal, not the timeout");
    }

    #[test]
    fn burst_of_clicks_coalesces_to_one_refetch() {
        let (tx, rx) = channel::<()>();
        for _ in 0..5 {
            tx.send(()).unwrap(); // 5 rapid Refresh clicks
        }
        // Wake once...
        assert!(matches!(rx.recv_timeout(Duration::from_secs(30)), Ok(())));
        // ...then drain the rest so they don't trigger 4 more back-to-back refetches.
        while rx.try_recv().is_ok() {}
        assert!(rx.try_recv().is_err(), "all queued signals drained → exactly one refetch");
    }

    #[test]
    fn dropped_sender_reports_disconnected_not_ok() {
        // At shutdown the App's Sender drops; the loop must see Disconnected (and fall back to the
        // 30s sleep) rather than spuriously refetching or panicking.
        let (tx, rx) = channel::<()>();
        drop(tx);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(30)),
            Err(RecvTimeoutError::Disconnected)
        ));
    }
}

#[cfg(test)]
mod chain_recorder_wiring_tests {
    //! The production hop the adversarial review flagged as missing: proving the options-refresh
    //! thread's provider actually CARRIES the app root's chain recorder (previously nothing outside
    //! tests ever called `with_chain_recorder`, so `VIKE_RECORD_CHAINS=1` was silently inert).
    //!
    //! This is the CI-gated end of the chain. The remaining hop — `vike-app`'s `App::new` calling
    //! `ChainRecorder::open_from_env(tick_store_root())` and passing the result to
    //! `spawn_tool_fetchers` — is in the wgpu binary, which CI compile-checks but never tests; the store
    //! open it performs is itself covered by `vike-data`'s `open_from_env` tests.

    use super::options_provider;
    use std::sync::Arc;
    use vike_data::{ChainRecorder, MemHistStore};

    #[test]
    fn provider_carries_the_threaded_recorder() {
        let store = Arc::new(MemHistStore::new());
        let rec = Arc::new(ChainRecorder::new(store, true));
        let provider = options_provider(Some(rec));
        let wired = provider.chain_recorder().expect("recorder reached the options provider");
        assert!(wired.enabled(), "and arrives enabled — fetch_chain will record");
    }

    #[test]
    fn no_recorder_is_the_default_and_leaves_the_provider_bare() {
        // The OFF path: byte-identical to pre-recording behavior — `fetch_chain` skips the hook.
        assert!(options_provider(None).chain_recorder().is_none());
    }

    #[test]
    fn a_disabled_recorder_still_threads_but_records_nothing() {
        // `open_from_env` returns `None` when the gate is off, so this shape only arises if a caller
        // builds a recorder directly; it must stay inert rather than half-record.
        let store = Arc::new(MemHistStore::new());
        let provider = options_provider(Some(Arc::new(ChainRecorder::new(store, false))));
        assert!(!provider.chain_recorder().expect("threaded").enabled());
    }
}

#[cfg(test)]
mod api_key_tests {
    //! [`ToolApiKeys`]' precedence, and the two properties the injection exists to buy: this
    //! module reads NO global state, and there is no CWD-relative store left to read.
    //!
    //! The tier that WON used to be decided by `std::env::var` + a `./.env` read inside the fetch
    //! thread, so neither could be exercised without mutating the process (unsound under threads)
    //! or writing a file into whatever directory the test runner happened to start in.

    use super::{ToolApiKeys, FINNHUB_API_KEY, FMP_API_KEY};
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn the_process_environment_outranks_the_store() {
        let keys = ToolApiKeys::resolve(
            &map(&[(FINNHUB_API_KEY, "from-env")]),
            &map(&[(FINNHUB_API_KEY, "from-store"), (FMP_API_KEY, "fmp-store")]),
        );
        assert_eq!(keys.finnhub.as_deref(), Some("from-env"));
        // ...and a key the environment does not mention still comes from the store.
        assert_eq!(keys.fmp.as_deref(), Some("fmp-store"));
    }

    #[test]
    fn a_blank_value_falls_through_instead_of_winning() {
        // Both tiers, both directions: whitespace is not an answer at either level. An empty
        // env value must not shadow a real stored key (the pre-injection behaviour), and an empty
        // stored value must not resolve to `Some("")` and send an unauthenticated fetch.
        let keys = ToolApiKeys::resolve(
            &map(&[(FINNHUB_API_KEY, "   "), (FMP_API_KEY, "")]),
            &map(&[(FINNHUB_API_KEY, "real")]),
        );
        assert_eq!(keys.finnhub.as_deref(), Some("real"));
        assert_eq!(keys.fmp, None);
    }

    /// ⚠ The CWD-relative regression, stated as an assertion: **empty maps in ⇒ no keys out**,
    /// for every working directory — including one holding a populated `.env`.
    ///
    /// The old reader answered this case from `./.env`, so the result depended on where the binary
    /// was launched from and no test could pin it without `set_current_dir` (process-global, and a
    /// race in a threaded harness). Purity is what makes the property assertable at all.
    #[test]
    fn values_are_trimmed_and_absent_keys_are_none() {
        let keys = ToolApiKeys::resolve(&HashMap::new(), &map(&[(FMP_API_KEY, "  padded  ")]));
        assert_eq!(keys.fmp.as_deref(), Some("padded"));
        assert_eq!(keys.finnhub, None);
        // Two empty maps is the ordinary no-keys case, not an error: the calendar day-strip simply
        // shows no earnings/dividend counts.
        assert_eq!(ToolApiKeys::resolve(&HashMap::new(), &HashMap::new()), ToolApiKeys::default());
    }

    #[test]
    fn debug_redacts_the_keys() {
        let keys = ToolApiKeys::resolve(
            &map(&[(FINNHUB_API_KEY, "secret-finnhub"), (FMP_API_KEY, "secret-fmp")]),
            &HashMap::new(),
        );
        let shown = format!("{keys:?}");
        assert!(!shown.contains("secret-finnhub"), "Debug leaked a key: {shown}");
        assert!(!shown.contains("secret-fmp"), "Debug leaked a key: {shown}");
        assert!(shown.contains("<set>"), "and still says which are configured: {shown}");
        assert!(format!("{:?}", ToolApiKeys::default()).contains("<unset>"));
    }
}
