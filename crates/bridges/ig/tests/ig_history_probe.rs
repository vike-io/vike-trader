//! Live demo PROBE, READ-ONLY: what does IG's historical-price endpoint (`GET /prices/{epic}`, v3) cost
//! and what does it return? It exists so the history line's numbers are MEASURED before any code is
//! built on them (IG's own pages disagree: `labs.ig.com`'s FAQ says 10,000 data points per WEEK,
//! `GET /operations/application` says `allowanceAccountHistoricalData` is per MINUTE, and
//! `crates/bridges/ig/CLAUDE.md` notes that the endpoint has never been read).
//!
//!     cargo test -p vike-ig --test ig_history_probe -- --ignored --nocapture --exact ig_history_probe
//!
//! On a lane, against the settings directory of the box that holds the demo ig account:
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box batch5/ig-history env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ig --test ig_history_probe -- --ignored --nocapture --exact ig_history_probe
//!
//! The table of what each printed number decides is the runbook's, kept with the research notes
//! outside this repository.
//!
//! **READ-ONLY, DEMO ONLY.** Every request is a `GET`; there is no order, no position, no write of any
//! kind (`the_probe_has_no_way_to_write` holds the file to it). A gateway that is not `demo-api.ig.com`
//! is refused up front. Credentials come the way the other IG smokes get them (`common::cfg`: the
//! credential store's default DEMO ig account, never argv and never the environment); the account id,
//! the identifier, the password, the API key and the session tokens are never printed, and
//! `GET /operations/application` (whose body carries the API key) is printed through an allowlist.
//!
//! **It spends at most `PLANNED_POINTS` (150) of the allowance, if a row costs a point** (`BUDGET_POINTS`
//! is 150): five calls to `/prices` and nothing else that is metered as history.
//!
//! | call | what | rows |
//! |---|---|---|
//! | A | EURUSD (the wired epic) at HOUR, `max=100&pageSize=0` (the shape `crates/bridges/ig/src/data.rs`'s `fetch_prices` sends) | 100 |
//! | B | one INDICES epic, HOUR, `max=20&pageSize=20` (paging ON, to see the page metadata of a paged answer) | 20 |
//! | C | one COMMODITIES epic, the same | 20 |
//! | E | EURUSD at DAY, `max=3&pageSize=0`: the daily stamps (hour, weekday) | 3 |
//! | D | EURUSD again, `from`/`to` over six bars A returned, sent `UNIT_WAIT` (65 s) after E | 6 |
//!
//! A 400 on A (a malformed ask: nothing was spent) earns one call A2, the same rows with paging on; a
//! 403 stops the run at once and nothing is retried. The allowance is read from the response metadata
//! of the data call itself (`metadata.allowance`), never from a call of its own. B and C are found
//! with the markets search (`GET /markets?searchTerm=`, the query `crates/bridges/ig/src/catalog.rs`'s
//! `IgCatalog` sends); when it names none, a well-known epic stands in and the output says so.
//! Everything that is not a `/prices` call (the application read, the searches, the three market
//! snapshots) is made BEFORE call A, so it cannot blur the difference between two data calls.
//!
//! **What it prints, and why raw.** The numbers a decision turns on are printed as IG sent them and then
//! once more as a table. Nothing here is a verdict: each "reading" line says what the numbers fit and
//! the runbook says what to do with it. The questions, each answered by a printed block: the HTTP status;
//! `metadata.allowance` (remaining, total, expiry) per call and what it implies for the unit (a week or a
//! minute, `remaining` before or after the call's own cost, points per row, a refill across the 65 s
//! wait); the page metadata; the price shape (bid/ask/lastTraded OHLC, volume); the decimal scale
//! against the lane's own price (`level x scalingFactor x pip`, the rule of
//! `crates/bridges/ig/src/quote_rate.rs`'s `price_units`); `snapshotTime` against `snapshotTimeUTC` and
//! whether the stamp is a bar's open or its close (the newest bar against the current hour); whether an
//! index and a commodity have history; and what `from`/`to` mean (call D).

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use common::{EPIC, cfg};
use vike_bridge_core::json::json_num;
use vike_ig::{IgConfig, IgSession, parse_ig_time_utc};

const HOUR_MS: i64 = 3_600_000;
/// Call A: the rows asked for. One row is one point if IG counts the way its FAQ reads.
const MAIN_ROWS: u32 = 100;
/// Calls B and C: the rows asked for, each.
const SMALL_ROWS: u32 = 20;
/// Call D: bars in the `from`/`to` window, and how many of the newest bars of A it stays clear of.
const WINDOW_BARS: usize = 6;
const WINDOW_CLEARANCE: usize = 6;
/// Call E: the daily bars asked for (the daily stamp convention decides how a day chunk holds them).
const DAILY_ROWS: u32 = 3;
/// The most points the five calls can spend, one point a row (D's window is inclusive: one more).
const PLANNED_POINTS: u32 = MAIN_ROWS + 2 * SMALL_ROWS + DAILY_ROWS + WINDOW_BARS as u32 + 1;
/// The owner's cap for the whole run.
const BUDGET_POINTS: u32 = 150;
/// Sent between call C and call D: longer than a minute, so a per-minute window has refilled.
const UNIT_WAIT: Duration = Duration::from_secs(65);
/// Slept before every request after the login, so the run stays far below IG's documented 30 non-trading
/// requests a minute for an account (its real figure on the demo gateway is not known).
const PACE: Duration = Duration::from_secs(1);

const INDEX_TERMS: [&str; 3] = ["Germany 40", "FTSE 100", "Wall Street"];
const COMMODITY_TERMS: [&str; 3] = ["Spot Gold", "Oil - US Crude", "Spot Silver"];
/// Well-known demo epics, used only when the search names none. NOT verified against this account.
const INDEX_FALLBACK: &str = "IX.D.FTSE.CFD.IP";
const COMMODITY_FALLBACK: &str = "CC.D.CL.UNC.IP";

const PRICE_FIELDS: [&str; 4] = ["openPrice", "highPrice", "lowPrice", "closePrice"];
/// The only keys of `GET /operations/application` that are printed: the row also carries the API key.
const APP_KEYS: [&str; 7] = [
    "status",
    "overallRequestLimit",
    "allowanceApplicationOverall",
    "allowanceAccountOverall",
    "allowanceAccountTrading",
    "allowanceAccountHistoricalData",
    "concurrentSubscriptionsLimit",
];

// ---------------------------------------------------------------------------------------------
// Pure helpers (the offline tests at the bottom hold them).
// ---------------------------------------------------------------------------------------------

/// `metadata.allowance` of a `/prices` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Allowance {
    remaining: i64,
    total: i64,
    expiry_s: i64,
}

fn allowance_of(body: &Value) -> Option<Allowance> {
    let a = &body["metadata"]["allowance"];
    let n = |key: &str| json_num(&a[key]).map(|v| v as i64);
    Some(Allowance {
        remaining: n("remainingAllowance")?,
        total: n("totalAllowance")?,
        expiry_s: n("allowanceExpiry")?,
    })
}

/// How a response's `remainingAllowance` relates to its own call.
#[derive(Debug, PartialEq, Eq)]
enum Meter {
    /// It is what is left AFTER the call's own cost.
    AfterOwnCall,
    /// It is what was left BEFORE the call's own cost.
    BeforeOwnCall,
    Unclear,
}

/// A cost per row a reader can believe: one point a row, or two (a bid and an ask).
fn plausible_cost(points_per_row: f64) -> bool {
    (0.99..=2.01).contains(&points_per_row)
}

/// `(rows, remaining)` of consecutive data calls. Of a pair, the drop in `remaining` is the NEXT call's
/// cost if the figure is read after the call and the PREVIOUS call's if it is read before; only a pair
/// whose row counts differ tells the two apart, and the reading is the one under which every such pair
/// costs a plausible number of points a row.
fn meter_reading(calls: &[(i64, i64)]) -> Meter {
    let (mut after, mut before, mut judged) = (true, true, 0);
    for pair in calls.windows(2) {
        let ((prev_rows, prev_left), (next_rows, next_left)) = (pair[0], pair[1]);
        if prev_rows == next_rows || prev_rows == 0 || next_rows == 0 {
            continue;
        }
        judged += 1;
        let drop = (prev_left - next_left) as f64;
        after &= plausible_cost(drop / next_rows as f64);
        before &= plausible_cost(drop / prev_rows as f64);
    }
    match (judged, after, before) {
        (0, _, _) => Meter::Unclear,
        (_, true, false) => Meter::AfterOwnCall,
        (_, false, true) => Meter::BeforeOwnCall,
        _ => Meter::Unclear,
    }
}

fn expiry_bucket(secs: i64) -> &'static str {
    match secs {
        ..=0 => "none",
        1..=60 => "within a minute",
        61..=3_600 => "within an hour",
        3_601..=86_400 => "within a day",
        86_401..=608_400 => "within a week",
        _ => "beyond a week",
    }
}

fn human(secs: i64) -> String {
    format!("{}d {}h {}m {}s", secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60)
}

/// The number of digits after the decimal point in the shortest text that reads back as `x`.
fn decimals(x: f64) -> usize {
    format!("{x}").split_once('.').map_or(0, |(_, fraction)| fraction.len())
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

/// Where the newest bar's stamp falls against the current UTC hour.
#[derive(Debug, PartialEq, Eq)]
enum HourPos {
    /// Stamped after the start of the current hour.
    Ahead,
    /// Stamped with the start of the current hour.
    Current,
    /// Stamped one hour before it.
    Previous,
    /// Stamped this many hours before it.
    Older(i64),
}

fn hour_pos(stamp_ms: i64, now_ms: i64) -> HourPos {
    let start = now_ms.div_euclid(HOUR_MS) * HOUR_MS;
    match (stamp_ms - start).div_euclid(HOUR_MS) {
        1.. => HourPos::Ahead,
        0 => HourPos::Current,
        -1 => HourPos::Previous,
        n => HourPos::Older(-n),
    }
}

/// What the newest bar's position says about the stamp. `tracks_live` (only for the ambiguous case): the
/// bar's close sits on the live price, as a bar still forming does and a bar that closed minutes ago does not.
fn stamp_reading(pos: &HourPos, tracks_live: Option<bool>) -> String {
    match (pos, tracks_live) {
        (HourPos::Ahead, _) => "the newest bar is stamped AHEAD of the current hour: a CLOSE stamp, \
                                and the forming bar IS returned"
            .into(),
        (HourPos::Previous, _) => "the newest bar is stamped one hour BEHIND the current hour: an OPEN \
                                   stamp, and the forming bar is NOT returned (a close stamp would \
                                   leave the last bar missing)"
            .into(),
        (HourPos::Current, Some(true)) => "stamped with the START of the current hour and its close sits on \
                                           the live price: an OPEN stamp, and the forming bar IS returned"
            .into(),
        (HourPos::Current, Some(false)) => "stamped with the START of the current hour but its close is \
                                            away from the live price: a CLOSE stamp (the bar that closed \
                                            at this hour) or a stale forming bar; rerun at another minute"
            .into(),
        (HourPos::Current, None) => "stamped with the START of the current hour: an OPEN stamp with the \
                                     forming bar returned, or a CLOSE stamp without it; no live price to \
                                     tell them apart, rerun"
            .into(),
        (HourPos::Older(n), _) => format!(
            "the newest bar is {n} hours old: the market is shut or the series lags; this run says \
             nothing about the stamp, rerun while the market is open"
        ),
    }
}

/// How the REST levels relate to the lane's own price. `Some(m)`: a REST level times `m` is the price.
fn scale_reading(rest_mid: f64, lane_mid: f64) -> (String, Option<f64>) {
    let ratio = rest_mid / lane_mid;
    let near = |x: f64| (x - 1.0).abs() <= 0.02;
    if near(ratio) {
        return (
            format!(
                "PRICE UNITS: the REST level is within 2% of the lane's price (ratio {ratio:.5})"
            ),
            Some(1.0),
        );
    }
    for k in 1..=6 {
        let scale = 10f64.powi(k);
        if near(ratio / scale) {
            return (
                format!(
                    "SCALED: the REST level is {scale} x the lane's price (ratio {ratio:.5}); \
                     the price is the level / {scale}"
                ),
                Some(1.0 / scale),
            );
        }
    }
    (format!("UNKNOWN: ratio {ratio:.5} fits neither price units nor a power of ten"), None)
}

/// Call D's answer against the stamps of A's bars that lie in the same window.
fn window_reading(want: &[String], got: &[String], from: &str) -> String {
    let Some(first) = got.first() else { return "no row came back".to_string() };
    if want == got {
        return format!(
            "the same {} bars A holds: `from`/`to` are read as UTC and both ends are inclusive",
            got.len()
        );
    }
    let shift = (parse_ig_time_utc(first) - parse_ig_time_utc(from)) / 60_000;
    format!(
        "differs: wanted {} bar(s) {}..{}, got {} bar(s) {first}..{}; the first stamp is {shift:+} min \
         from `from` (a whole-hour shift means the dates are read in another zone)",
        want.len(),
        want.first().map_or("-", String::as_str),
        want.last().map_or("-", String::as_str),
        got.len(),
        got.last().map_or("-", String::as_str),
    )
}

// ---------------------------------------------------------------------------------------------
// Reading a response.
// ---------------------------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Epoch ms as `YYYY-MM-DDTHH:MM:SSZ`.
fn iso(ms: i64) -> String {
    let (days, secs) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000) / 1000);
    let (y, m, d) = vike_util::calendar::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

fn rows_of(body: &Value) -> Vec<Value> {
    body["prices"].as_array().cloned().unwrap_or_default()
}

fn mid(row: &Value, field: &str) -> Option<f64> {
    Some((json_num(&row[field]["bid"])? + json_num(&row[field]["ask"])?) / 2.0)
}

fn stamp_ms(row: &Value, key: &str) -> Option<i64> {
    let ms = parse_ig_time_utc(row[key].as_str()?);
    (ms != 0).then_some(ms)
}

/// One market snapshot (`GET /markets/{epic}` v3): the lane's own view of the price.
struct Snapshot {
    bid: f64,
    offer: f64,
    factor: f64,
    pip: f64,
    status: String,
    pip_means: String,
}

impl Snapshot {
    /// `level x scalingFactor x pip`, the lane's rule; the bare level when either figure is unreadable.
    fn lane_mid(&self) -> f64 {
        let level = (self.bid + self.offer) / 2.0;
        if self.factor.is_finite() && self.pip.is_finite() {
            level * self.factor * self.pip
        } else {
            level
        }
    }
}

fn read_snapshot(session: &IgSession, epic: &str) -> Option<Snapshot> {
    std::thread::sleep(PACE);
    let m = session.get(&format!("/markets/{epic}"), "3", "").ok()?;
    let (s, i) = (&m["snapshot"], &m["instrument"]);
    let pip_means = i["onePipMeans"].as_str().unwrap_or_default().to_string();
    Some(Snapshot {
        bid: json_num(&s["bid"])?,
        offer: json_num(&s["offer"])?,
        factor: json_num(&s["scalingFactor"]).unwrap_or(f64::NAN),
        pip: pip_means.split_whitespace().next().and_then(|w| w.parse().ok()).unwrap_or(f64::NAN),
        status: s["marketStatus"].as_str().unwrap_or("?").to_string(),
        pip_means,
    })
}

fn print_snapshot(epic: &str, snap: Option<&Snapshot>) {
    match snap {
        Some(s) => println!(
            "  {epic}: marketStatus {}  bid {}  offer {}  scalingFactor {}  onePipMeans '{}'  \
             => the lane's price {} (level x scalingFactor x pip)",
            s.status,
            s.bid,
            s.offer,
            s.factor,
            s.pip_means,
            s.lane_mid()
        ),
        None => println!("  {epic}: no snapshot price (the read failed or the market is shut)"),
    }
}

/// What came back from one `/prices` call.
struct Reply {
    status: u16,
    text: String,
    body: Value,
    header_names: Vec<String>,
    safe_headers: Vec<String>,
    at_ms: i64,
    took: Duration,
}

/// The session's token pair. `IgSession` keeps it private and renders it as the Lightstreamer password,
/// `CST-<cst>|XST-<security token>`; it is parsed in memory for the one request below and never printed.
fn token_pair(session: &IgSession) -> Option<(String, String)> {
    let password = session.ls_password();
    let (cst, xst) = password.strip_prefix("CST-")?.split_once("|XST-")?;
    Some((cst.to_string(), xst.to_string()))
}

/// `GET /prices/{epic}?{query}` (Version 3) with the status, the headers and the body kept: the shared
/// session's `get` returns the parsed body alone, so it cannot say which 2xx IG answered.
fn get_prices(c: &IgConfig, session: &IgSession, epic: &str, query: &str) -> Result<Reply, String> {
    let (cst, xst) = token_pair(session).ok_or("the session holds no token pair")?;
    std::thread::sleep(PACE);
    let url = format!("{}/prices/{epic}?{query}", c.rest_base);
    let agent = vike_bridge_core::http::blocking_agent();
    let started = Instant::now();
    let mut resp = agent
        .get(&url)
        .header("X-IG-API-KEY", &c.api_key)
        .header("CST", &cst)
        .header("X-SECURITY-TOKEN", &xst)
        .header("Version", "3")
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("network error: {e}"))?;
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_ascii_lowercase(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("network error reading the body: {e}"))?;
    let took = started.elapsed();
    let safe = |name: &str| {
        matches!(name, "date" | "retry-after" | "content-type" | "content-length")
            || name.starts_with("x-ratelimit")
            || name.starts_with("ratelimit")
    };
    Ok(Reply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        header_names: headers.iter().map(|(k, _)| k.clone()).collect(),
        safe_headers: headers
            .iter()
            .filter(|(k, _)| safe(k))
            .map(|(k, v)| format!("{k}={v}"))
            .collect(),
        text,
        at_ms: now_ms(),
        took,
    })
}

/// One data call as the summary keeps it.
struct Call {
    label: String,
    status: u16,
    rows: i64,
    allowance: Option<Allowance>,
    at_ms: i64,
}

fn is_ok(r: &Reply) -> bool {
    (200..300).contains(&r.status)
}

/// Print one reply's status, headers, page metadata and allowance; keep what the summary needs.
fn show(label: &str, epic: &str, query: &str, r: &Reply) -> Call {
    let rows = rows_of(&r.body);
    println!("\n---- {label} ----");
    println!("  GET /prices/{epic}?{query}   (Version: 3)");
    println!(
        "  HTTP status: {}   body: {} bytes   took: {} ms   received: {}",
        r.status,
        r.text.len(),
        r.took.as_millis(),
        iso(r.at_ms)
    );
    println!("  response header names: [{}]", r.header_names.join(", "));
    if !r.safe_headers.is_empty() {
        println!("  non-secret header values: {}", r.safe_headers.join("; "));
    }
    if !is_ok(r) {
        let clip: String = r.text.chars().take(400).collect();
        println!("  ERROR BODY: {clip}");
    }
    println!("  instrumentType: {}   rows: {}", r.body["instrumentType"], rows.len());
    println!("  metadata (verbatim, page metadata included): {}", r.body["metadata"]);
    let allowance = allowance_of(&r.body);
    match allowance {
        Some(a) => println!(
            "  allowance: remaining {} of total {}; resets in {} s (= {}, {})",
            a.remaining,
            a.total,
            a.expiry_s,
            human(a.expiry_s),
            expiry_bucket(a.expiry_s)
        ),
        None => println!("  allowance: not in this response"),
    }
    Call {
        label: label.to_string(),
        status: r.status,
        rows: rows.len() as i64,
        allowance,
        at_ms: r.at_ms,
    }
}

/// The price shape of a set of rows: keys, the oldest and newest row, nulls, spread, decimals, range.
fn print_shape(rows: &[Value]) {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        println!("  (no rows)");
        return;
    };
    let keys = |v: &Value| {
        v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default()
    };
    println!("  row keys: [{}]   openPrice keys: [{}]", keys(first), keys(&first["openPrice"]));
    println!("  oldest row: {first}");
    println!("  newest row: {last}");
    let n = rows.len();
    let null_traded = rows.iter().filter(|r| r["closePrice"]["lastTraded"].is_null()).count();
    let null_volume = rows.iter().filter(|r| r["lastTradedVolume"].is_null()).count();
    println!(
        "  closePrice.lastTraded is null in {null_traded}/{n} rows; lastTradedVolume is null in {null_volume}/{n}"
    );
    let spreads: Vec<f64> = rows
        .iter()
        .filter_map(|r| {
            Some(json_num(&r["closePrice"]["ask"])? - json_num(&r["closePrice"]["bid"])?)
        })
        .collect();
    if let Some(m) = median(spreads.clone()) {
        let (lo, hi) =
            spreads.iter().fold((f64::MAX, f64::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
        println!(
            "  closePrice ask - bid (as returned, to 6 places): min {lo:.6}  median {m:.6}  max {hi:.6}"
        );
    }
    let (mut max_dec, mut lo, mut hi) = (0, f64::MAX, f64::MIN);
    for r in rows {
        for field in PRICE_FIELDS {
            for side in ["bid", "ask"] {
                if let Some(x) = json_num(&r[field][side]) {
                    max_dec = max_dec.max(decimals(x));
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
        }
    }
    println!("  price levels: {lo} .. {hi}, up to {max_dec} decimal(s)");
}

/// REST levels against the lane's price. Returns the multiplier that turns a REST level into a price.
fn print_scale(rows: &[Value], snap: Option<&Snapshot>) -> Option<f64> {
    let (rest, lane) = (mid(rows.last()?, "closePrice")?, snap?.lane_mid());
    let (text, multiplier) = scale_reading(rest, lane);
    println!(
        "  scale: newest close mid {rest} against the lane's price {lane} (the market snapshot read \
         just before this call)\n  scale reading: {text}"
    );
    multiplier
}

/// The stamps: `snapshotTime` against `snapshotTimeUTC`, the grid, and the open-or-close question.
fn print_stamps(rows: &[Value], now: i64, snap: Option<&Snapshot>, multiplier: Option<f64>) {
    let Some(last) = rows.last() else { return };
    let start = now.div_euclid(HOUR_MS) * HOUR_MS;
    println!(
        "  newest bar: snapshotTime '{}'  snapshotTimeUTC '{}'",
        last["snapshotTime"].as_str().unwrap_or("?"),
        last["snapshotTimeUTC"].as_str().unwrap_or("?")
    );
    println!(
        "  system clock (UTC): {}; the current hour began {}, {} min ago",
        iso(now),
        iso(start),
        (now - start) / 60_000
    );
    let (utc, local) = (stamp_ms(last, "snapshotTimeUTC"), stamp_ms(last, "snapshotTime"));
    if let (Some(u), Some(l)) = (utc, local) {
        println!(
            "  snapshotTime - snapshotTimeUTC = {:+} min: snapshotTime is {}",
            (l - u) / 60_000,
            if l == u {
                "the same clock as snapshotTimeUTC"
            } else {
                "a local wall clock; snapshotTimeUTC is the UTC one iff it fits the system clock, see below"
            }
        );
        println!(
            "  against the current UTC hour: snapshotTimeUTC {:?}, snapshotTime {:?}",
            hour_pos(u, now),
            hour_pos(l, now)
        );
    }
    let stamps: Vec<i64> = rows.iter().filter_map(|r| stamp_ms(r, "snapshotTimeUTC")).collect();
    let mut gaps: Vec<String> = stamps
        .windows(2)
        .filter(|w| w[1] - w[0] != HOUR_MS)
        .map(|w| format!("{}h after {}", (w[1] - w[0]) / HOUR_MS, iso(w[0])))
        .collect();
    gaps.truncate(6);
    let regular = stamps.windows(2).filter(|w| w[1] - w[0] == HOUR_MS).count();
    println!(
        "  consecutive snapshotTimeUTC gaps: {regular} of one hour; others (market shut, first 6): [{}]",
        gaps.join("; ")
    );
    let Some(u) = utc else { return };
    let pos = hour_pos(u, now);
    let mut tracks = None;
    if pos == HourPos::Current
        && let (Some(snap), Some(m), Some(close)) = (snap, multiplier, mid(last, "closePrice"))
    {
        let earlier: Vec<f64> = rows[..rows.len() - 1]
            .iter()
            .filter_map(|r| Some((mid(r, "highPrice")? - mid(r, "lowPrice")?).abs() * m))
            .collect();
        if let Some(typical) = median(earlier) {
            let away = (close * m - snap.lane_mid()).abs();
            tracks = Some(away <= 0.1 * typical);
            println!(
                "  evidence: the newest close is {away:.6} from the live price; a typical hourly range is {typical:.6}; \
                 it tracks the live price if within a tenth of that"
            );
        }
    }
    println!("  stamp reading: {}", stamp_reading(&pos, tracks));
}

/// Day of the week of an epoch-ms instant (1970-01-01 was a Thursday).
fn weekday(ms: i64) -> &'static str {
    ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]
        [ms.div_euclid(86_400_000).rem_euclid(7) as usize]
}

/// The daily bars' stamps with their weekday: the hour a DAY bar is stamped at (00:00 UTC, or the FX
/// session's evening before) decides which UTC-day chunk holds it and what a Sunday or a Saturday holds.
fn print_daily_stamps(rows: &[Value]) {
    for row in rows {
        let stamp = |key: &str| {
            stamp_ms(row, key).map_or("?".to_string(), |ms| format!("{} {}", weekday(ms), iso(ms)))
        };
        println!(
            "  daily bar: snapshotTimeUTC {}   snapshotTime {}",
            stamp("snapshotTimeUTC"),
            stamp("snapshotTime")
        );
    }
    println!(
        "  daily reading: a stamp away from 00:00 UTC puts the bar under the UTC date BEFORE the \
         one its session is named for; a UTC-day chunk holds the bar by its stamp, so read the hour \
         and the weekday above before a daily series is trusted to a day grid"
    );
}

/// `GET /operations/application`: the allowance fields only. Returns `allowanceAccountHistoricalData`.
fn print_application(session: &IgSession, api_key: &str) -> Option<i64> {
    println!(
        "\n---- GET /operations/application (v1): allowance fields only, no key, no name ----"
    );
    let body = match session.get("/operations/application", "1", "") {
        Ok(b) => b,
        Err(e) => {
            println!("  failed: HTTP {} {}", e.status, e.message);
            return None;
        }
    };
    let apps: Vec<&Value> = match &body {
        Value::Array(rows) => rows.iter().collect(),
        other => vec![other],
    };
    let ours = apps
        .iter()
        .copied()
        .find(|a| a["apiKey"].as_str().is_some_and(|k| k.eq_ignore_ascii_case(api_key)))
        .or_else(|| apps.first().copied().filter(|_| apps.len() == 1));
    let Some(app) = ours else {
        println!("  {} application row(s), none carries this key", apps.len());
        return None;
    };
    for key in APP_KEYS {
        println!("  {key}: {}", app[key]);
    }
    json_num(&app["allowanceAccountHistoricalData"]).map(|v| v as i64)
}

/// An epic for a class: the search's first TRADEABLE rolling CFD of the type, else its first TRADEABLE
/// one, else the well-known stand-in.
struct Pick {
    epic: String,
    how: String,
}

fn pick_epic(session: &IgSession, terms: &[&str], kind: &str, fallback: &str) -> Pick {
    for term in terms {
        std::thread::sleep(PACE);
        let query = format!("searchTerm={}", vike_util::percent::percent_encode(term));
        let Ok(found) = session.get("/markets", "1", &query) else { continue };
        let rows: Vec<&Value> = found["markets"]
            .as_array()
            .map(|all| all.iter().filter(|m| m["instrumentType"] == kind).collect())
            .unwrap_or_default();
        let best = rows
            .iter()
            .copied()
            .find(|m| m["marketStatus"] == "TRADEABLE" && m["expiry"] == "-")
            .or_else(|| rows.iter().copied().find(|m| m["marketStatus"] == "TRADEABLE"));
        if let Some(m) = best {
            return Pick {
                epic: m["epic"].as_str().unwrap_or_default().to_string(),
                how: format!(
                    "found by the markets search '{term}': '{}' ({}, expiry {})",
                    m["instrumentName"].as_str().unwrap_or("?"),
                    m["marketStatus"].as_str().unwrap_or("?"),
                    m["expiry"].as_str().unwrap_or("?")
                ),
            };
        }
    }
    Pick {
        epic: fallback.to_string(),
        how: "the search named no TRADEABLE epic of the type: a WELL-KNOWN epic stands in, not \
              verified against this account"
            .to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------------------------

/// A small class call (B or C): send it, show it, and say whether history exists for the epic.
fn small_call(
    c: &IgConfig,
    s: &IgSession,
    label: &str,
    pick: &Pick,
    snap: Option<&Snapshot>,
) -> Option<Call> {
    let query = format!("resolution=HOUR&max={SMALL_ROWS}&pageSize={SMALL_ROWS}&pageNumber=1");
    println!("\n  {label}: {} ({})", pick.epic, pick.how);
    let reply = match get_prices(c, s, &pick.epic, &query) {
        Ok(r) => r,
        Err(e) => {
            println!("  {label}: NO ANSWER: {e}");
            return None;
        }
    };
    let call = show(label, &pick.epic, &query, &reply);
    let rows = rows_of(&reply.body);
    if is_ok(&reply) {
        print_shape(&rows);
        print_snapshot(&pick.epic, snap);
        print_scale(&rows, snap);
    }
    println!(
        "  history exists for {}: {}",
        pick.epic,
        if is_ok(&reply) && !rows.is_empty() {
            format!(
                "YES, {} row(s), newest stamp {}",
                rows.len(),
                rows[rows.len() - 1]["snapshotTimeUTC"]
            )
        } else {
            format!("NO / NOT SERVED: HTTP {}", reply.status)
        }
    );
    Some(call)
}

#[test]
#[ignore = "live demo; needs a demo ig account's credentials; READ-ONLY (no order) but spends about 150 points of IG's historical-data allowance: run by hand (see module doc)"]
fn ig_history_probe() {
    let Some(c) = cfg() else {
        eprintln!("SKIP: demo ig account credentials absent");
        return;
    };
    if !c.rest_base.contains("demo-api.ig.com") {
        eprintln!("SKIP: refusing a non-demo IG gateway ({})", c.rest_base);
        return;
    }
    let session = match IgSession::login(&c) {
        Ok(s) => s,
        Err(e) => return eprintln!("skip: IG demo login failed: {e}"),
    };
    println!(
        "ig history probe: demo gateway, READ-ONLY, at most {PLANNED_POINTS} points (cap {BUDGET_POINTS}), \
         started {}",
        iso(now_ms())
    );

    // Everything that is not a `/prices` call comes first, so it cannot blur two data calls' difference.
    let documented = print_application(&session, &c.api_key);
    let index = pick_epic(&session, &INDEX_TERMS, "INDICES", INDEX_FALLBACK);
    let commodity = pick_epic(&session, &COMMODITY_TERMS, "COMMODITIES", COMMODITY_FALLBACK);
    println!("\n---- market snapshots (GET /markets/{{epic}}, not metered as history) ----");
    let (snap_fx, snap_index, snap_commodity) = (
        read_snapshot(&session, EPIC),
        read_snapshot(&session, &index.epic),
        read_snapshot(&session, &commodity.epic),
    );
    print_snapshot(EPIC, snap_fx.as_ref());

    let mut calls: Vec<Call> = Vec::new();

    // ---- A: the main call -------------------------------------------------------------------
    // The shape `fetch_prices` sends (paging off); only a 400 (a malformed ask, nothing spent) earns the
    // second, paged ask, so a rejected `pageSize=0` does not cost the whole run.
    let asks = [
        (
            format!("A. EURUSD, HOUR, {MAIN_ROWS} rows"),
            format!("resolution=HOUR&max={MAIN_ROWS}&pageSize=0"),
        ),
        (
            format!("A2. EURUSD, HOUR, {MAIN_ROWS} rows, paging on"),
            format!("resolution=HOUR&max={MAIN_ROWS}&pageSize={MAIN_ROWS}&pageNumber=1"),
        ),
    ];
    let mut halted = true;
    let mut rows_a: Vec<Value> = Vec::new();
    for (label, query) in &asks {
        let reply = match get_prices(&c, &session, EPIC, query) {
            Ok(reply) => reply,
            Err(e) => {
                println!("\n---- {label} ----\n  NO ANSWER: {e}");
                break;
            }
        };
        calls.push(show(label, EPIC, query, &reply));
        if reply.status == 400 {
            println!(
                "  {label} was refused as malformed (HTTP 400): the next ask changes the paging"
            );
            continue;
        }
        halted = !is_ok(&reply);
        rows_a = rows_of(&reply.body);
        if !halted {
            print_shape(&rows_a);
            let multiplier = print_scale(&rows_a, snap_fx.as_ref());
            print_stamps(&rows_a, reply.at_ms, snap_fx.as_ref(), multiplier);
        }
        break;
    }

    // ---- B and C: one small call per class ---------------------------------------------------
    for (label, pick, snap) in [
        (format!("B. index, HOUR, {SMALL_ROWS} rows"), &index, snap_index.as_ref()),
        (format!("C. commodity, HOUR, {SMALL_ROWS} rows"), &commodity, snap_commodity.as_ref()),
    ] {
        if halted {
            println!("\n  {label}: NOT SENT (an earlier call was refused or unanswered)");
            continue;
        }
        if let Some(call) = small_call(&c, &session, &label, pick, snap) {
            halted = call.status == 403;
            calls.push(call);
        }
    }

    // ---- E: three daily bars, for the daily stamp ---------------------------------------------
    if !halted {
        let query = format!("resolution=DAY&max={DAILY_ROWS}&pageSize=0");
        let label = format!("E. EURUSD, DAY, {DAILY_ROWS} rows");
        match get_prices(&c, &session, EPIC, &query) {
            Err(e) => println!("\n---- {label} ----\n  NO ANSWER: {e}"),
            Ok(reply) => {
                let call = show(&label, EPIC, &query, &reply);
                halted = call.status == 403;
                calls.push(call);
                print_daily_stamps(&rows_of(&reply.body));
            }
        }
    }

    // ---- D: from/to, after the wait ----------------------------------------------------------
    let before_wait = calls.iter().rev().find_map(|k| k.allowance).map(|a| a.remaining);
    if !halted && rows_a.len() >= WINDOW_BARS + WINDOW_CLEARANCE {
        // the bars of A that D should return again: WINDOW_BARS of them, clear of the newest ones
        let end = rows_a.len() - WINDOW_CLEARANCE;
        let stamps: Vec<String> = rows_a[end - WINDOW_BARS..end]
            .iter()
            .map(|r| r["snapshotTimeUTC"].as_str().unwrap_or_default().to_string())
            .collect();
        let (from, to) = (stamps[0].clone(), stamps[WINDOW_BARS - 1].clone());
        println!(
            "\n  waiting {} s so that a per-minute window would have refilled ...",
            UNIT_WAIT.as_secs()
        );
        std::thread::sleep(UNIT_WAIT);
        let query = format!("resolution=HOUR&from={from}&to={to}&pageSize=0");
        let label = format!("D. EURUSD, HOUR, from {from} to {to}");
        match get_prices(&c, &session, EPIC, &query) {
            Err(e) => println!("\n---- {label} ----\n  NO ANSWER: {e}"),
            Ok(reply) => {
                calls.push(show(&label, EPIC, &query, &reply));
                let got: Vec<String> = rows_of(&reply.body)
                    .iter()
                    .map(|r| r["snapshotTimeUTC"].as_str().unwrap_or_default().to_string())
                    .collect();
                println!("  window reading: {}", window_reading(&stamps, &got, &from));
            }
        }
    } else {
        println!("\n  D: NOT SENT (call A or B/C was refused, or A returned too few bars)");
    }

    print_summary(&calls, documented, before_wait);
}

/// The allowance table and what it fits. Hints, not verdicts: the numbers are printed first.
fn print_summary(calls: &[Call], documented: Option<i64>, before_wait: Option<i64>) {
    println!("\n================ ALLOWANCE: read the numbers, decide the unit ================");
    println!(
        "  {:<46} {:>4} {:>5} {:>9} {:>7} {:>9}  {:<16}  received (UTC)",
        "call", "HTTP", "rows", "remaining", "total", "expiry_s", "expires in"
    );
    for k in calls {
        let (rem, tot, exp, text) = match k.allowance {
            Some(a) => (
                a.remaining.to_string(),
                a.total.to_string(),
                a.expiry_s.to_string(),
                human(a.expiry_s),
            ),
            None => ("-".into(), "-".into(), "-".into(), "-".into()),
        };
        println!(
            "  {:<46} {:>4} {:>5} {rem:>9} {tot:>7} {exp:>9}  {text:<16}  {}",
            k.label,
            k.status,
            k.rows,
            iso(k.at_ms)
        );
    }
    let metered: Vec<(&Call, Allowance)> =
        calls.iter().filter_map(|k| k.allowance.map(|a| (k, a))).collect();
    let Some((first, a1)) = metered.first().copied() else {
        println!(
            "  NO call returned metadata.allowance: this run MEASURED NOTHING about the allowance."
        );
        return;
    };
    for pair in metered.windows(2) {
        let ((p, pa), (n, na)) = (pair[0], pair[1]);
        let fall = pa.remaining - na.remaining;
        println!(
            "  step '{}' -> '{}': remaining {} -> {} (fell {fall}); rows {} then {}\n      \
             read AFTER the call's own cost: {:.2} point(s) per row of the later call\n      \
             read BEFORE the call's own cost: {:.2} point(s) per row of the earlier call",
            p.label.split('.').next().unwrap_or("?"),
            n.label.split('.').next().unwrap_or("?"),
            pa.remaining,
            na.remaining,
            p.rows,
            n.rows,
            fall as f64 / n.rows.max(1) as f64,
            fall as f64 / p.rows.max(1) as f64,
        );
    }
    let rows_left: Vec<(i64, i64)> = metered.iter().map(|(k, a)| (k.rows, a.remaining)).collect();
    let meter = meter_reading(&rows_left);
    println!(
        "  meter reading: {meter:?} (AfterOwnCall: `remaining` is what is left once the call's cost is taken)"
    );
    let used = a1.total - a1.remaining;
    println!(
        "  first call: total - remaining = {used} against {} row(s): {}",
        first.rows,
        match meter {
            Meter::AfterOwnCall if used == first.rows =>
                "the window was untouched before this run and a row costs a point",
            Meter::AfterOwnCall =>
                "the window was used before this run (or a row costs other than a point)",
            Meter::BeforeOwnCall => "that much was already used in this window before this run",
            Meter::Unclear => "unreadable until the meter is known",
        }
    );
    println!(
        "  window: the first call's expiry is {} s ({}, {})",
        a1.expiry_s,
        human(a1.expiry_s),
        expiry_bucket(a1.expiry_s)
    );
    if let Some((last, al)) = metered.last().copied().filter(|(k, _)| !std::ptr::eq(*k, first)) {
        let wall = (last.at_ms - first.at_ms) / 1000;
        println!(
            "  expiry countdown: {} s -> {} s over {wall} s of wall clock (a fixed window falls by about the wall clock; a window that restarts per call does not)",
            a1.expiry_s, al.expiry_s
        );
    }
    println!(
        "  total allowance: {} (the FAQ says 10,000 a week for a live key, and 'different' for demo)",
        a1.total
    );
    match documented {
        Some(n) => println!(
            "  GET /operations/application allowanceAccountHistoricalData: {n} ({})",
            if n == a1.total { "equals the total above" } else { "DIFFERS from the total above" }
        ),
        None => println!("  GET /operations/application allowanceAccountHistoricalData: not read"),
    }
    if let Some(prev) = before_wait
        && let Some(after) =
            calls.iter().find(|k| k.label.starts_with("D.")).and_then(|k| k.allowance)
    {
        println!(
            "  wait test ({} s): remaining {prev} before the wait, {} in call D => {}",
            UNIT_WAIT.as_secs(),
            after.remaining,
            if after.remaining > prev {
                "the remainder went UP: a window of about a minute, not a week"
            } else {
                "no refill: not a per-minute window"
            }
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Offline tests: the pure helpers above. They run in the default suite and touch no network.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_plan_stays_inside_the_budget() {
    assert_eq!(PLANNED_POINTS, 150);
    const { assert!(PLANNED_POINTS <= BUDGET_POINTS) };
}

/// The probe holds no order path: the file never names a mutating verb or an execution builder. The
/// needles are spelled in pieces so this test does not find itself.
#[test]
fn the_probe_has_no_way_to_write() {
    let source = include_str!("ig_history_probe.rs");
    for needle in [
        concat!(".po", "st("),
        concat!(".pu", "t("),
        concat!(".dele", "te("),
        concat!("post_method", "_delete"),
        concat!("build_", "request"),
        concat!("build_close", "_request"),
        concat!("IgExecution", "Client"),
    ] {
        assert!(!source.contains(needle), "the probe names `{needle}`: it is READ-ONLY");
    }
}

#[test]
fn an_allowance_is_read_from_the_response_metadata() {
    let body = serde_json::json!({"metadata": {"allowance":
        {"remainingAllowance": 9900, "totalAllowance": 10000, "allowanceExpiry": 604000}}});
    assert_eq!(
        allowance_of(&body),
        Some(Allowance { remaining: 9900, total: 10000, expiry_s: 604000 })
    );
    assert_eq!(allowance_of(&serde_json::json!({"errorCode": "x"})), None);
}

#[test]
fn the_meter_is_read_from_the_pair_whose_row_counts_differ() {
    // one point a row, `remaining` read after the call: 10000 -> 9900 (100 rows) -> 9880 (20 rows)
    assert_eq!(meter_reading(&[(100, 9900), (20, 9880)]), Meter::AfterOwnCall);
    // read before the call: the 100-row call shows 10000, the 20-row call shows what the first left
    assert_eq!(meter_reading(&[(100, 10000), (20, 9900)]), Meter::BeforeOwnCall);
    // equal row counts cannot tell the two apart
    assert_eq!(meter_reading(&[(20, 9900), (20, 9880)]), Meter::Unclear);
    // a cost no reading explains
    assert_eq!(meter_reading(&[(100, 9900), (20, 9000)]), Meter::Unclear);
}

#[test]
fn the_newest_stamp_is_placed_against_the_current_hour() {
    let start = 1_800_000_000_000_i64.div_euclid(HOUR_MS) * HOUR_MS;
    let now = start + 20 * 60_000;
    assert_eq!(hour_pos(start, now), HourPos::Current);
    assert_eq!(hour_pos(start - HOUR_MS, now), HourPos::Previous);
    assert_eq!(hour_pos(start + HOUR_MS, now), HourPos::Ahead);
    assert_eq!(hour_pos(start - 5 * HOUR_MS, now), HourPos::Older(5));
    assert!(stamp_reading(&HourPos::Previous, None).contains("OPEN"));
    assert!(stamp_reading(&HourPos::Ahead, None).contains("CLOSE"));
}

#[test]
fn the_scale_is_read_against_the_lanes_price() {
    assert_eq!(scale_reading(1.16543, 1.1654).1, Some(1.0));
    let (text, multiplier) = scale_reading(11654.3, 1.16543);
    assert!(text.starts_with("SCALED"), "{text}");
    assert!((multiplier.unwrap() - 1e-4).abs() < 1e-12);
    assert_eq!(scale_reading(3.0, 1.0).1, None);
}

#[test]
fn small_readers_count_what_they_say() {
    assert_eq!(decimals(1.16543), 5);
    assert_eq!(decimals(7.0), 0);
    assert_eq!(decimals(11654.3), 1);
    assert_eq!(expiry_bucket(55), "within a minute");
    assert_eq!(expiry_bucket(604_000), "within a week");
    assert_eq!(expiry_bucket(2_000_000), "beyond a week");
    assert_eq!(human(604_800 - 1), "6d 23h 59m 59s");
    assert_eq!(iso(1_642_255_200_000), "2022-01-15T14:00:00Z");
    assert_eq!((weekday(1_642_255_200_000), weekday(0), weekday(-1)), ("Sat", "Thu", "Wed"));
}

#[test]
fn the_window_reading_names_a_shift() {
    let want: Vec<String> =
        ["2026-10-20T08:00:00", "2026-10-20T09:00:00"].map(String::from).to_vec();
    assert!(window_reading(&want, &want, &want[0]).starts_with("the same 2 bars"));
    let shifted: Vec<String> =
        ["2026-10-20T07:00:00", "2026-10-20T08:00:00"].map(String::from).to_vec();
    assert!(window_reading(&want, &shifted, &want[0]).contains("-60 min"));
    assert_eq!(window_reading(&want, &[], &want[0]), "no row came back");
}

/// `n` hourly rows ending at `newest_ms`, shaped like IG's v3 answer, EURUSD-like levels.
fn synthetic_rows(n: usize, newest_ms: i64) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let at = newest_ms - (n - 1 - i) as i64 * HOUR_MS;
            let level = 1.1650 + 0.0001 * i as f64;
            let side = |x: f64| serde_json::json!({"bid": x, "ask": x + 0.0001, "lastTraded": null});
            serde_json::json!({
                "snapshotTime": iso(at + HOUR_MS).replace('-', "/").replace('T', " ").trim_end_matches('Z'),
                "snapshotTimeUTC": iso(at).trim_end_matches('Z'),
                "openPrice": side(level), "highPrice": side(level + 0.0004),
                "lowPrice": side(level - 0.0004), "closePrice": side(level + 0.0001),
                "lastTradedVolume": null
            })
        })
        .collect()
}

fn synthetic_reply(status: u16, body: Value, at_ms: i64) -> Reply {
    Reply {
        status,
        text: body.to_string(),
        body,
        header_names: vec!["date".into(), "cst".into()],
        safe_headers: vec!["date=now".into()],
        at_ms,
        took: Duration::from_millis(5),
    }
}

/// The live run is the owner's one chance to spend the points, so every printer is driven offline over
/// a synthetic answer, an error and an empty one: a panic in a printer must be found here, not there.
#[test]
fn the_printers_survive_every_shape_of_answer() {
    let start = 1_800_000_000_000_i64.div_euclid(HOUR_MS) * HOUR_MS;
    let now = start + 20 * 60_000;
    let rows = synthetic_rows(100, start);
    let allowance = |left: i64| serde_json::json!({"remainingAllowance": left, "totalAllowance": 10000, "allowanceExpiry": 604000});
    let body = serde_json::json!({"prices": rows, "instrumentType": "CURRENCIES",
        "metadata": {"allowance": allowance(9900), "size": 100,
                     "pageData": {"pageSize": 0, "pageNumber": 1, "totalPages": 1}}});
    let a = show("A. synthetic", EPIC, "q", &synthetic_reply(200, body, now));
    assert_eq!((a.rows, a.allowance.map(|x| x.remaining)), (100, Some(9900)));

    let snap = Snapshot {
        bid: 1.175,
        offer: 1.1751,
        factor: 10000.0,
        pip: 0.0001,
        status: "TRADEABLE".into(),
        pip_means: "0.0001 USD/EUR".into(),
    };
    print_shape(&rows);
    let multiplier = print_scale(&rows, Some(&snap));
    assert_eq!(multiplier, Some(1.0));
    print_stamps(&rows, now, Some(&snap), multiplier);
    print_stamps(&rows, now + 5 * HOUR_MS, None, None);
    print_stamps(&rows[..1], now, Some(&snap), multiplier);
    print_snapshot(EPIC, Some(&snap));
    print_snapshot(EPIC, None);

    let refused = serde_json::json!({"errorCode": "error.public-api.exceeded-account-historical-data-allowance"});
    let b = show("B. refused", EPIC, "q", &synthetic_reply(403, refused, now));
    let empty = serde_json::json!({"prices": [], "metadata": {"allowance": allowance(9880)}});
    let c = show("C. empty", EPIC, "q", &synthetic_reply(200, empty, now));
    let d = show("D. not json", EPIC, "q", &synthetic_reply(200, Value::Null, now));
    print_shape(&[]);
    print_stamps(&[], now, None, None);
    print_daily_stamps(&rows[..2]);
    print_daily_stamps(&[]);
    assert_eq!((b.allowance, c.rows, d.allowance), (None, 0, None));

    print_summary(&[], None, None);
    print_summary(&[b, d], Some(10000), None);
    let wait = show(
        "D. after the wait",
        EPIC,
        "q",
        &synthetic_reply(200, serde_json::json!({"metadata": {"allowance": allowance(9890)}}), now),
    );
    print_summary(&[a, c, wait], Some(10000), Some(9880));
}
