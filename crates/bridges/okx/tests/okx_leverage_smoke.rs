//! LIVE DEMO smokes for okx's per-account leverage (decision 0182): the READ half
//! (`crates/bridges/okx/src/leverage_read.rs`'s `leverage_reader`) and the WRITE-AND-READ-BACK round trip
//! through the bridge's one existing writer (`crates/bridges/okx/src/perp.rs`'s `OkxPerpRest`, whose
//! `set_leverage` posts `mgnMode: cross` and which the exec thread fires at every start). Neither places
//! an order.
//!
//!     cargo test -p vike-okx --test okx_leverage_smoke -- --ignored --nocapture
//!
//! HAND-RUN ONLY: no workflow lists this file. Only `account/config`'s `uid`, `mainUid`, `perm` and
//! `posMode` had ever been measured on the demo host; `acctLv`, `leverage-info` and a flat symbol's
//! `positions` answer were the venue's documented shapes. These are the drill that was owed.
//!
//! * `okx_leverage_read_smoke` — READ-ONLY. Asks the four GETs the reader makes (`account/config`,
//!   `leverage-info`, `positions`, the public `instruments`) and PRINTS each answer through
//!   `vike_bridge_core::capture::capture_frame` (identity keys redacted, structure verbatim: the form a
//!   committed fixture takes; the key's `label` and `ip` are blanked here too), then the verdicts of the
//!   pure parsers, then asserts the production reader answers exactly what the parsers do and that the
//!   account is one the feature accepts (`acctLv` 2 or 3, net positions, cross, a published maximum).
//!   READ THE PRINTED BODY BEFORE PASTING IT ANYWHERE: the sanitizer is an enumeration of keys.
//! * `okx_leverage_write_read_back_smoke` — WRITES the demo account's cross leverage for `SYMBOL`, once,
//!   and puts it back. It reads the prior figure, writes a small different one through
//!   `OkxPerpRest::set_leverage`, polls the reader until the venue holds it (`confirm`, the lane's own
//!   poll), then writes the PRIOR figure and reads that back. The restore is armed BEFORE the write and
//!   also runs from a drop guard, so a panic in between still puts the figure back. It refuses
//!   (self-skips) while the account holds an open position on `SYMBOL`. ⚠ If a daemon runs this account,
//!   `SYMBOL`'s cross leverage is the smoke's for a few seconds; the daemon's exec thread writes only at
//!   its own start.
//!
//! Double-gated like every `*_smoke.rs` here: the store's default DEMO account (`TIER`, resolved through
//! `vike_secrets::smoke_directory`, self-skipping with a `SKIP:` warn when its key trio is absent) and the
//! `x-simulated-trading: 1` header, which is the ONLY thing separating demo from mainnet on this venue
//! (one host); `demo_transport` asserts it on every transport this file builds.

use serde_json::{Value, json};
use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::leverage_read::{HeldLeverage, LeverageReader, confirm, same_leverage};
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, SymbolProperties, now_ms};
use vike_okx::account_config::PATH_ACCOUNT_CONFIG;
use vike_okx::leverage_read::{
    PATH_LEVERAGE_INFO, check_account_config, leverage_reader, parse_leverage_info,
    parse_max_leverage, parse_open_position,
};
use vike_okx::perp::{OkxPerpRest, PATH_INSTRUMENTS, PATH_POSITIONS, REST};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTC-USDT-SWAP";
/// The only tier this file resolves an account at.
const TIER: AccountTier = AccountTier::Demo;

/// The guard's first key, checked before any request; the second is [`demo_transport`].
fn assert_demo_only() {
    assert_eq!(TIER, AccountTier::Demo, "a leverage smoke resolves the DEMO account only");
}

/// A transport that sends `x-simulated-trading: 1` (demo), rate-gated like the neighbouring smokes. The
/// header is the demo switch on this venue, so its absence is refused here.
fn demo_transport() -> UreqOkxTransport {
    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
    assert!(transport.simulated, "the demo header is not set: this would be MAINNET");
    transport
}

/// Print one answer the way a fixture would hold it: sanitized, and with what was redacted listed.
/// `extra` names further keys to blank (a key's label and whitelisted ip identify the box, not the
/// account).
fn show(what: &str, raw: &Value, extra: &[&str]) {
    let mut s = capture_frame("okx", what, raw);
    blank(&mut s.frame, extra);
    println!(
        "\n=== {what}: sanitized answer (read it before pasting: an unrecognised key prints in the clear) ===\nredacted: {:?} + blanked {extra:?}\n{}",
        s.redacted_paths,
        serde_json::to_string_pretty(&s.frame).unwrap_or_default()
    );
}

/// Replace the value under every key in `keys` (any depth) with a fixed placeholder.
fn blank(v: &mut Value, keys: &[&str]) {
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                if keys.contains(&k.as_str()) {
                    *child = json!("[blanked]");
                } else {
                    blank(child, keys);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| blank(i, keys)),
        _ => {}
    }
}

fn rest_at(creds: &Credentials, leverage: f64) -> OkxPerpRest<UreqOkxTransport> {
    OkxPerpRest {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: demo_transport(),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: SymbolProperties::default(),
        ct_val: 1.0, // unused by set_leverage
        leverage,
        broker_code: None,
    }
}

/// Puts the prior leverage back: explicitly (`run`, whose failure the test reports) or, if the test
/// panics first, from `Drop`. Never panics itself (a panic in `drop` during unwinding aborts).
struct Restore {
    rest: OkxPerpRest<UreqOkxTransport>,
    done: bool,
}

impl Restore {
    fn run(&mut self) -> Result<(), String> {
        self.rest.set_leverage().map_err(|e| e.to_string())?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        match self.run() {
            Ok(()) => eprintln!("restored {SYMBOL} to {}x from the drop guard", self.rest.leverage),
            Err(why) => eprintln!(
                "RESTORE FAILED: set the DEMO account's {SYMBOL} cross leverage back to {}x by hand: {why}",
                self.rest.leverage
            ),
        }
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_leverage_read_smoke() {
    vike_log::test_init();
    assert_demo_only();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, TIER);
    let Some(creds) = api_credentials(&account, "okx") else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };

    let transport = demo_transport();
    let signer = OkxV5Signer::new(&creds, now_ms);
    let get = |what: &str, path: &str, params: &[(&str, Value)], extra: &[&str]| {
        let raw = transport
            .signed(REST, path, "GET", params, &signer)
            .unwrap_or_else(|e| panic!("GET {path} failed: {e}"));
        show(what, &raw, extra);
        unwrap_okx(raw).unwrap_or_else(|e| panic!("GET {path} answered an error: {e}"))
    };
    let config = get("account-config", PATH_ACCOUNT_CONFIG, &[], &["label", "ip"]);
    let leverage_info = get(
        "leverage-info",
        PATH_LEVERAGE_INFO,
        &[("mgnMode", json!("cross")), ("instId", json!(SYMBOL))],
        &[],
    );
    let positions = get(
        "positions",
        PATH_POSITIONS,
        &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))],
        &[],
    );
    let instruments = {
        let raw = transport
            .public(
                REST,
                PATH_INSTRUMENTS,
                &[("instType", "SWAP".into()), ("instId", SYMBOL.into())],
            )
            .expect("public instruments");
        show("instruments", &raw, &[]);
        unwrap_okx(raw).expect("instruments data")
    };

    let mode = config
        .get(0)
        .ok_or_else(|| "account/config answered no row".to_string())
        .and_then(check_account_config);
    let leverage = parse_leverage_info(&leverage_info, SYMBOL);
    let open = parse_open_position(&positions, SYMBOL);
    let max = parse_max_leverage(&instruments, SYMBOL);
    println!(
        "\n=== VERDICTS ({SYMBOL}, DEMO) ===\naccount mode : {mode:?}\nlever (cross): {leverage:?}\nposition open: {open:?}\nvenue max    : {max:?}\n"
    );

    // The production reader, over its own transport, must answer what the parsers did over this one.
    let reader = leverage_reader(&account, TIER, SYMBOL).expect("an okx reader for the account");
    assert_eq!(reader.check_account(), mode, "reader and parser disagree on the account mode");
    assert_eq!(reader.venue_max(), max, "reader and parser disagree on the maximum");
    let parsed = leverage
        .clone()
        .and_then(|held| open.clone().map(|position_open| HeldLeverage { held, position_open }));
    assert_eq!(reader.held(), parsed, "reader and parsers disagree on what is held");

    assert_eq!(
        mode,
        Ok(()),
        "the demo account is not one this feature accepts (see the body above)"
    );
    let lever =
        leverage.expect("the leverage row is not one this reader accepts (see the body above)");
    assert!(lever >= 1.0);
    open.expect("the positions answer is not one this reader accepts (see the body above)");
    assert!(matches!(max, Ok(Some(m)) if m >= 1.0), "no usable instrument maximum: {max:?}");
    tracing::info!(target: "vike_okx", "okx leverage read smoke green: account mode, lever, positions and maximum all read live");
}

#[test]
#[ignore = "network + demo creds + WRITES the demo leverage, then restores it — run manually (see module doc)"]
fn okx_leverage_write_read_back_smoke() {
    vike_log::test_init();
    assert_demo_only();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, TIER);
    let Some(creds) = api_credentials(&account, "okx") else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };

    let reader = leverage_reader(&account, TIER, SYMBOL).expect("an okx reader for the account");
    reader.check_account().unwrap_or_else(|why| panic!("account refused: {why}"));
    let before = reader.held().unwrap_or_else(|why| panic!("held could not be read: {why}"));
    if before.position_open {
        tracing::warn!(target: "vike_okx", "SKIP: the demo account holds an open {SYMBOL} position");
        return;
    }
    let prior = before.held;
    let target = if same_leverage(prior, 3.0) { 4.0 } else { 3.0 };
    if let Some(max) = reader.venue_max().expect("the venue maximum") {
        assert!(target <= max, "{target}x is above the venue maximum {max}x");
    }
    println!(
        "\n=== {SYMBOL} (DEMO, cross): prior {prior}x, writing {target}x, restoring {prior}x ==="
    );

    let mut restore = Restore { rest: rest_at(&creds, prior), done: false };
    rest_at(&creds, target).set_leverage().expect("set_leverage through the bridge's writer");
    let seen = confirm(&reader, target, std::thread::sleep)
        .unwrap_or_else(|why| panic!("the venue never held the written figure: {why}"));
    println!("read back after the write: {seen}x");
    assert!(same_leverage(seen, target));

    restore.run().unwrap_or_else(|why| {
        panic!("RESTORE FAILED: set {SYMBOL} back to {prior}x on the demo account by hand: {why}")
    });
    let restored = confirm(&reader, prior, std::thread::sleep).unwrap_or_else(|why| {
        panic!("the prior figure was not read back after the restore: {why}")
    });
    println!("read back after the restore: {restored}x");
    assert!(same_leverage(restored, prior));
    tracing::info!(target: "vike_okx", "okx leverage write/read-back smoke green: {prior}x -> {target}x -> {prior}x");
}
