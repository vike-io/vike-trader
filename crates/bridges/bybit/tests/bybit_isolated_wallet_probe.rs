//! LIVE demo probe for the Bybit per-position ISOLATED-WALLET field — the verification gate the
//! `vike_exec::margin_call` "mode-without-wallet" asymmetry doc (and
//! `recon_client::parse_positions`'s deliberate `isolated_margin: None`) says must happen before
//! any parse of `positionBalance` is trusted: an isolated Bybit position currently subtracts only
//! its uPnL from the cross pool while its UNKNOWN wallet stays folded in the balance, overstating
//! cross equity (a LATE local cross-liquidation judgment).
//!
//!     cargo test -p vike-bybit --test bybit_isolated_wallet_probe -- --ignored --nocapture
//!
//! `#[ignore]`d + double-gated exactly like every other `*_smoke.rs` here (self-skips without
//! `BYBIT_DEMO_*` creds in the workspace `.env`). DEMO ACCOUNT ONLY — fake money — and unlike the
//! read-only reconcile smoke this probe MAY MUTATE the demo account to manufacture the payload it
//! needs to inspect: if no isolated position exists it (a) switches ONE minor symbol
//! (XRPUSDT) to isolated `tradeMode` — falling back to the UTA account-level
//! `set-margin-mode ISOLATED_MARGIN` if the per-symbol switch is unsupported — (b) opens a
//! minimum-size market position, (c) dumps the raw `/v5/position/list` row (the money shot:
//! `positionBalance` / `positionIM` / `tradeMode` semantics on the live wire), then (d) CLOSES the
//! position and (e) restores the original margin configuration — the account is left as found.
//! Every step prints its raw JSON so a failed run can be triaged from the transcript.

use serde_json::{Value, json};

use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::perp::DEMO_REST;
use vike_bybit::transport::{BybitTransport, UreqBybitTransport, unwrap_envelope};
use vike_model::clock::now_ms;

const SYMBOL: &str = "XRPUSDT";

/// The double-gate every `*_smoke.rs` in this crate uses (see `bybit_perp_smoke.rs`).
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("bybit", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

struct Probe {
    signer: BybitV5Signer,
    transport: UreqBybitTransport,
}

impl Probe {
    fn call(
        &self,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        let resp = self.transport.signed(DEMO_REST, path, method, params, &self.signer)?;
        unwrap_envelope(resp)
    }

    /// Raw `/v5/position/list` result for SYMBOL, printed verbatim (the payload under test).
    fn dump_positions(&self, label: &str) -> Result<Value, VenueApiError> {
        let result = self.call(
            "/v5/position/list",
            "GET",
            &[("category", json!("linear")), ("symbol", json!(SYMBOL))],
        )?;
        println!("=== position/list [{label}] ===\n{result}");
        Ok(result)
    }

    /// Signed-size of the first nonzero SYMBOL row (Buy => +, Sell => -), 0.0 if flat.
    fn open_size(positions: &Value) -> f64 {
        for p in positions.get("list").and_then(|l| l.as_array()).unwrap_or(&vec![]) {
            let size = p.get("size").and_then(json_num).unwrap_or(0.0).abs();
            if size > 0.0 {
                let sign =
                    if p.get("side").and_then(|s| s.as_str()) == Some("Buy") { 1.0 } else { -1.0 };
                return sign * size;
            }
        }
        0.0
    }

    /// Market order (one-way positionIdx 0). `qty` is pre-formatted.
    fn market(&self, side: &str, qty: &str, reduce_only: bool) -> Result<Value, VenueApiError> {
        let coid = format!("iso-probe-{}", now_ms());
        self.call(
            "/v5/order/create",
            "POST",
            &[
                ("category", json!("linear")),
                ("symbol", json!(SYMBOL)),
                ("side", json!(side)),
                ("orderType", json!("Market")),
                ("qty", json!(qty)),
                ("orderLinkId", json!(coid)),
                ("positionIdx", json!(0)),
                ("reduceOnly", json!(reduce_only)),
            ],
        )
    }
}

#[test]
#[ignore = "network + demo creds + MUTATES the demo account (opens/closes a tiny position) — run manually (see module doc)"]
fn bybit_isolated_wallet_probe() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    let probe = Probe {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
    };

    // -- 1. account shape: UTA status + account-level margin mode ------------------------------
    let info = probe.call("/v5/account/info", "GET", &[]).expect("account/info");
    println!("=== account/info ===\n{info}");
    let orig_margin_mode =
        info.get("marginMode").and_then(|m| m.as_str()).unwrap_or("REGULAR_MARGIN").to_string();

    // -- 2. read-only pass: is an isolated position already there? -----------------------------
    let before = probe.dump_positions("before").expect("position/list before");
    if Probe::open_size(&before) != 0.0 {
        // A leftover open probe/smoke position would make open/close bookkeeping ambiguous —
        // bail out rather than trade over it. (The smokes always flatten; this is unexpected.)
        panic!("SYMBOL already has an open position — refusing to probe over it: {before}");
    }

    // -- 3. manufacture isolation (demo only). Per-symbol switch first; UTA fallback = account
    //       mode. Track what to restore. ------------------------------------------------------
    let mut restore_account_mode = false;
    let mut restore_symbol_cross = false;
    let mut isolated_manufactured = true;
    match probe.call(
        "/v5/position/switch-isolated",
        "POST",
        &[
            ("category", json!("linear")),
            ("symbol", json!(SYMBOL)),
            ("tradeMode", json!(1)),
            ("buyLeverage", json!("10")),
            ("sellLeverage", json!("10")),
        ],
    ) {
        Ok(r) => {
            println!("=== switch-isolated tradeMode=1 OK ===\n{r}");
            restore_symbol_cross = true;
        }
        Err(e) => {
            println!(
                "=== switch-isolated FAILED (code {}: {}) — trying account-level set-margin-mode ISOLATED_MARGIN ===",
                e.code, e.msg
            );
            match probe.call(
                "/v5/account/set-margin-mode",
                "POST",
                &[("setMarginMode", json!("ISOLATED_MARGIN"))],
            ) {
                Ok(r) => {
                    println!("=== set-margin-mode ISOLATED_MARGIN OK ===\n{r}");
                    restore_account_mode = orig_margin_mode != "ISOLATED_MARGIN";
                }
                Err(e2) => {
                    // VENUE-BLOCKED (seen live 2026-07-19: 10032 "Demo trading are not
                    // supported." + 110073 "Set margin mode failed" on the UTA demo account) —
                    // isolation cannot be manufactured here. FALL THROUGH to a CROSS-position
                    // control leg instead of aborting: a populated `positionBalance` on a CROSS
                    // row would itself disprove "positionBalance = the isolated wallet", which
                    // is still a live data point for the margin_call.rs asymmetry doc.
                    println!(
                        "!!! ISOLATION UNAVAILABLE on this account: switch-isolated code {} ({}), set-margin-mode code {} ({}) — proceeding with a CROSS control position",
                        e.code, e.msg, e2.code, e2.msg
                    );
                    isolated_manufactured = false;
                }
            }
        }
    }

    // -- 4. minimum viable qty from the live instrument grid + last price ----------------------
    let inst = probe
        .call(
            "/v5/market/instruments-info",
            "GET",
            &[("category", json!("linear")), ("symbol", json!(SYMBOL))],
        )
        .expect("instruments-info");
    let row = &inst["list"][0];
    let step = row["lotSizeFilter"]["qtyStep"].as_str().unwrap_or("1").parse::<f64>().unwrap();
    let min_qty =
        row["lotSizeFilter"]["minOrderQty"].as_str().unwrap_or("1").parse::<f64>().unwrap();
    let min_notional =
        row["lotSizeFilter"]["minNotionalValue"].as_str().unwrap_or("5").parse::<f64>().unwrap();
    let tickers = probe
        .call(
            "/v5/market/tickers",
            "GET",
            &[("category", json!("linear")), ("symbol", json!(SYMBOL))],
        )
        .expect("tickers");
    let last = tickers["list"][0]["lastPrice"].as_str().unwrap_or("0").parse::<f64>().unwrap();
    assert!(last > 0.0, "no last price for {SYMBOL}");
    // 1.3x cushion over min-notional so a market fill can't slip under it; snap UP to the step.
    let mut qty = (min_notional * 1.3 / last).max(min_qty);
    qty = (qty / step).ceil() * step;
    let qty_s = format_to_step_f(qty, step);
    println!("qty={qty_s} (step={step} min_qty={min_qty} min_notional={min_notional} last={last})");

    // -- 5..7. open → dump the money shot → close. NO panics in this window (cleanup below). --
    let risky = (|| -> Result<Value, String> {
        let open = probe.market("Buy", &qty_s, false).map_err(|e| format!("open: {e:?}"))?;
        println!("=== market Buy OK ===\n{open}");
        std::thread::sleep(std::time::Duration::from_secs(2));
        let label = if isolated_manufactured {
            "ISOLATED-OPEN (money shot)"
        } else {
            "CROSS-OPEN (control leg — isolation venue-blocked)"
        };
        let shot = probe.dump_positions(label).map_err(|e| format!("{e:?}"))?;
        let sz = Probe::open_size(&shot);
        if sz != 0.0 {
            let close_qty = format_to_step_f(sz.abs(), step);
            let close_side = if sz > 0.0 { "Sell" } else { "Buy" };
            let close =
                probe.market(close_side, &close_qty, true).map_err(|e| format!("close: {e:?}"))?;
            println!("=== market {close_side} reduceOnly OK ===\n{close}");
        }
        Ok(shot)
    })();

    // -- 8. restore the margin configuration regardless of how 5..7 went ----------------------
    if restore_symbol_cross {
        match probe.call(
            "/v5/position/switch-isolated",
            "POST",
            &[
                ("category", json!("linear")),
                ("symbol", json!(SYMBOL)),
                ("tradeMode", json!(0)),
                ("buyLeverage", json!("10")),
                ("sellLeverage", json!("10")),
            ],
        ) {
            Ok(_) => println!("=== restored {SYMBOL} to cross (tradeMode 0) ==="),
            Err(e) => println!("!!! FAILED to restore {SYMBOL} to cross: {} {}", e.code, e.msg),
        }
    }
    if restore_account_mode {
        match probe.call(
            "/v5/account/set-margin-mode",
            "POST",
            &[("setMarginMode", json!(orig_margin_mode.clone()))],
        ) {
            Ok(_) => println!("=== restored account margin mode to {orig_margin_mode} ==="),
            Err(e) => {
                println!(
                    "!!! FAILED to restore margin mode {orig_margin_mode}: {} {}",
                    e.code, e.msg
                )
            }
        }
    }

    // -- 9. verify flat again, then surface any risky-window failure ---------------------------
    std::thread::sleep(std::time::Duration::from_secs(1));
    let after = probe.dump_positions("after").expect("position/list after");
    let leftover = Probe::open_size(&after);
    let shot = risky.expect("open/dump/close window failed (see prints; account restored)");
    assert_eq!(leftover, 0.0, "probe left an open position behind: {after}");

    // The probe's whole point: the money shot must contain the row we opened.
    let got_row = Probe::open_size(&shot) != 0.0;
    assert!(got_row, "money-shot payload had no open position row: {shot}");
    if isolated_manufactured {
        println!(
            "=== probe complete: inspect the ISOLATED-OPEN dump above for positionBalance/positionIM/tradeMode semantics ==="
        );
    } else {
        println!(
            "=== probe complete: ISOLATION VENUE-BLOCKED on this account — the CROSS control dump above shows what positionBalance/positionIM/tradeMode carry for a cross row (a populated positionBalance there disproves the isolated-wallet reading) ==="
        );
    }
}
