//! **What does the binance DEMO account's income history answer for perpetual funding?** A READ-ONLY capture, run
//! by hand, printed for `crates/bridges/binance/tests/fixtures/funding_income_demo.json`:
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture binance_funding_capture::
//!
//! Every request is a signed GET of `/fapi/v1/income` (`incomeType=FUNDING_FEE`) on the demo USDⓈ-M host, the
//! seed's row (`crate::demo_credentials::demo_hosts`'s `fapi_rest`; nothing is
//! placed, cancelled or moved), printed sanitized with its row count, the `time` range and order it lists, and the
//! symbols it names: with no window (the venue's last 7 days), with `startTime` alone 3 days back, and with a
//! `startTime`/`endTime` pair over the same 3 days at `limit=2` (the order a full page comes back in). Then the
//! position risk read, to say which positions were held. ⚠ Read every printed row before committing it.

use serde_json::Value;

use crate::demo_credentials::{demo_hosts, load_demo_credentials};
use vike_binance::execution::rest::perp::PATH_POSITIONS;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::now_ms;

const PATH_INCOME: &str = "/fapi/v1/income";
const DAY: i64 = 86_400_000;

fn read(
    t: &UreqTransport,
    s: &BinanceHmacSigner,
    tag: &str,
    path: &str,
    params: &[(&str, String)],
) {
    match t.signed(demo_hosts().fapi_rest, path, "GET", params, s) {
        Ok(body) => {
            let rows = body.as_array().cloned().unwrap_or_default();
            let times: Vec<i64> = rows.iter().filter_map(|r| r["time"].as_i64()).collect();
            let mut symbols: Vec<&str> = rows.iter().filter_map(|r| r["symbol"].as_str()).collect();
            symbols.sort_unstable();
            symbols.dedup();
            println!(
                "WINDOW {tag} {params:?}: {} rows, time {:?}..{:?}, first {:?} last {:?}, symbols {symbols:?}",
                rows.len(),
                times.iter().min(),
                times.iter().max(),
                times.first(),
                times.last()
            );
            let sanitized = capture_frame("binance", tag, &body);
            println!("CAPTURED {tag} redacted={:?} {}", sanitized.redacted_paths, sanitized.frame);
        }
        Err(e) => println!("REFUSED {tag} {params:?}: [{}] {}", e.code, e.msg),
    }
}

#[test]
#[ignore = "network + demo creds — a read-only CAPTURE, run by hand (see module doc)"]
fn binance_funding_capture() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let t = UreqTransport::new("binance");
    let s = BinanceHmacSigner::new(&creds, now_ms);
    let now = now_ms();
    let funding = ("incomeType", "FUNDING_FEE".to_string());
    read(&t, &s, "income-no-window", PATH_INCOME, &[funding.clone(), ("limit", "1000".into())]);
    read(
        &t,
        &s,
        "income-start-only-3d",
        PATH_INCOME,
        &[funding.clone(), ("startTime", (now - 3 * DAY).to_string()), ("limit", "1000".into())],
    );
    read(
        &t,
        &s,
        "income-window-3d-limit-2",
        PATH_INCOME,
        &[
            funding,
            ("startTime", (now - 3 * DAY).to_string()),
            ("endTime", now.to_string()),
            ("limit", "2".into()),
        ],
    );
    // Every income kind over the venue's default week (the row shape the account does list), and the
    // funding rows week by week over the three months the venue keeps.
    read(&t, &s, "income-any-kind", PATH_INCOME, &[("limit", "50".into())]);
    for week in 1..=13 {
        read(
            &t,
            &s,
            &format!("income-funding-week-{week}"),
            PATH_INCOME,
            &[
                ("incomeType", "FUNDING_FEE".into()),
                ("startTime", (now - week * 7 * DAY).to_string()),
                ("endTime", (now - (week - 1) * 7 * DAY).to_string()),
                ("limit", "1000".into()),
            ],
        );
    }
    match t.signed(demo_hosts().fapi_rest, PATH_POSITIONS, "GET", &[], &s) {
        Ok(body) => {
            let held: Vec<(Value, Value)> = body
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| {
                    p["positionAmt"].as_str().and_then(|a| a.parse::<f64>().ok()) != Some(0.0)
                })
                .map(|p| (p["symbol"].clone(), p["positionAmt"].clone()))
                .collect();
            println!("POSITIONS held: {held:?}");
        }
        Err(e) => println!("REFUSED positionRisk: [{}] {}", e.code, e.msg),
    }
}
