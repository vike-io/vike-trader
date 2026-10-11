//! The background fetch threads and the equity-calendar HTTP calls they run.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::api_keys::ToolApiKeys;
use super::calendar::{DividendRow, EarningsRow, IpoRow, fetch_calendar};
use super::news::fetch_news;
use super::options::{OPT_POLL_MS, OptionsLink, OptionsPoller, wait_for_next_poll};
use super::view::ToolData;
use crate::data::catalog_wire::CatalogDial;

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

/// Earnings rows for the whole week — Finnhub (matches vike's source). ONE call.
fn fetch_finnhub_earnings(agent: &ureq::Agent, key: &str, frm: &str, to: &str) -> Vec<EarningsRow> {
    let mut out = Vec::new();
    let url = format!("https://finnhub.io/api/v1/calendar/earnings?from={frm}&to={to}&token={key}");
    if let Some(v) = nasdaq_json(agent, &url)
        && let Some(arr) = v["earningsCalendar"].as_array()
    {
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
    out
}

/// Dividend rows for the whole week — FMP (matches vike's source). ONE call.
fn fetch_fmp_dividends(agent: &ureq::Agent, key: &str, frm: &str, to: &str) -> Vec<DividendRow> {
    let mut out = Vec::new();
    let url = format!(
        "https://financialmodelingprep.com/stable/dividends-calendar?from={frm}&to={to}&apikey={key}"
    );
    if let Some(v) = nasdaq_json(agent, &url)
        && let Some(arr) = v.as_array()
    {
        for r in arr {
            let date = r["date"].as_str().unwrap_or("").to_string();
            if date.is_empty() {
                continue;
            }
            let amount = r["dividend"].as_f64().map(|x| format!("{x:.2} $")).unwrap_or_default();
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

/// Spawn one thread per source; each fetches, updates the shared state, wakes the
/// UI, then sleeps and refreshes.
///
/// `dial` answers where the backend's datahub is and what to sign with, AT THE MOMENT the options
/// poll asks: the composition root hands the one dial the Data Manager's catalog route already
/// keeps current (it follows the active backend record, which a switch replaces), so the two
/// datahub clients can never sign with different keys. `None` is "no datahub", and the Options
/// tool then says so rather than asking anybody else.
///
/// `api_keys` are the third-party data-provider keys the equity-calendar thread fetches with,
/// resolved by the app root — see [`ToolApiKeys`], which is also where the two absent-key
/// behaviours are stated. [`ToolApiKeys::default`] (both `None`) is a complete answer: that thread
/// then runs the keyless Nasdaq IPO fetch alone.
///
/// The options poll's wake channel is installed in `state` as [`ToolData::opt_link`]: the Refresh
/// pill and the windows' picks reach the thread through it.
pub fn spawn_tool_fetchers(
    state: Arc<Mutex<ToolData>>,
    wake: impl Fn() + Send + Clone + 'static,
    dial: impl Fn() -> Option<CatalogDial> + Send + 'static,
    api_keys: ToolApiKeys,
) {
    let news = state.clone();
    let w = wake.clone();
    std::thread::Builder::new()
        .name("tools-news".into())
        .spawn(move || {
            loop {
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
            }
        })
        .expect("spawn news thread");

    let cal = state.clone();
    let w = wake.clone();
    std::thread::Builder::new()
        .name("tools-calendar".into())
        .spawn(move || {
            loop {
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
            }
        })
        .expect("spawn calendar thread");

    // Equity-calendar counts for the day-strip cards — SAME providers as vike (so the numbers
    // match): Earnings→Finnhub, Dividends→FMP (both single range-queries), IPO→Nasdaq. The 3
    // fetches run in PARALLEL behind a timed agent (10s) and each updates+wakes as it lands.
    let eq = state.clone();
    let w = wake.clone();
    std::thread::Builder::new()
        .name("tools-eq-cal".into())
        .spawn(move || {
            loop {
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
                    handles.push(
                        std::thread::Builder::new()
                            .name("tools-earnings".into())
                            .spawn(move || {
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
                            })
                            .expect("spawn earnings thread"),
                    );
                }
                // Dividends → FMP (one call)
                if let Some(k) = api_keys.fmp.clone() {
                    let (a, e2, w2, frm2, to2) =
                        (agent.clone(), eq.clone(), w.clone(), frm.clone(), to.clone());
                    handles.push(
                        std::thread::Builder::new()
                            .name("tools-dividend".into())
                            .spawn(move || {
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
                            })
                            .expect("spawn dividends thread"),
                    );
                }
                // IPO → Nasdaq (monthly, bucketed). (b) its OWN agent with a tighter per-call timeout so a
                // single stuck month-call fails fast instead of stalling behind the shared global timeout.
                let (e2, w2) = (eq.clone(), w.clone());
                handles.push(
                    std::thread::Builder::new()
                        .name("tools-ipo".into())
                        .spawn(move || {
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
                                        s.cal_equity
                                            .entry(r.date.clone())
                                            .or_insert((0, 0, 0))
                                            .2 += 1;
                                    }
                                }
                                s.cal_ipos = rows;
                            }
                            w2();
                        })
                        .expect("spawn ipo thread"),
                );
                for h in handles {
                    let _ = h.join();
                }
                // (d) if the IPO feed came up empty (almost always a transient fetch miss — Nasdaq has IPOs
                // most weeks), retry in 60s instead of stranding the gap for the full 30-min cycle.
                let retry_soon = eq.lock().unwrap().cal_ipos.is_empty();
                std::thread::sleep(Duration::from_secs(if retry_soon { 60 } else { 1800 }));
            }
        })
        .expect("spawn equity-calendar thread");

    // The Options tool's poll thread: it asks the backend's DATAHUB, never Deribit
    // (`super::options`' module doc carries what it asks, how often and why there is no fallback).
    // It dials nothing while no Options or Greeks window has recorded a pick.
    let (opt_wake_tx, opt_wake_rx) = std::sync::mpsc::channel::<()>();
    state.lock().unwrap().opt_link = OptionsLink::new(opt_wake_tx);
    std::thread::Builder::new()
        .name("tools-options".into())
        .spawn(move || {
            let mut poller = OptionsPoller::default();
            let cadence = Duration::from_millis(OPT_POLL_MS);
            loop {
                let dial_now = dial();
                if poller.run_once(&state, dial_now.as_ref(), std::time::Instant::now()) {
                    wake();
                }
                wait_for_next_poll(&opt_wake_rx, cadence);
            }
        })
        .expect("spawn options thread");
}
