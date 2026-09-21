//! LIVE read-only reconcile smoke for Aster — the sibling of `binance_reconcile_smoke.rs`
//! (`vike-binance`) and `okx_reconcile_smoke.rs` (`vike-okx`), proving the real `AsterReconClient`
//! actually fetches AND parses against the live venue, not just the golden bodies
//! `tests/recon_client_parse.rs` exercises offline.
//!
//!     cargo test -p vike-aster --test aster_reconcile_smoke -- --ignored --nocapture
//!
//! Constructed through the PRODUCTION factory itself — `vike_aster::recon_client::recon_client`,
//! the function `vike_mount::make_engine`'s `("aster", _)` arm calls behind `VIKE_RECONCILE=1` — so
//! this smoke covers the env→URL resolution, the `.P` spot/perp routing and the rate gates too, not
//! just the fetch bodies. Double-gated exactly like every other `*_smoke.rs` in this crate (see
//! `aster_smoke.rs`): `#[ignore]`d, and self-skips with a printed reason when no `ASTER_*` creds are
//! in the workspace `.env`, so a credential-less box gets a clean skip, never a failure.
//!
//! ## ⚠ This authenticates against MAINNET
//!
//! Aster has NO testnet credentials configured (`aster_userdata_soak.rs`'s module doc is the
//! authority: the testnet HOSTS are real and reachable, but the workspace `.env` holds only
//! `ASTER_LIVE_*`), so `resolve` lands on `Environment::Live` in practice and these fetches
//! authenticate against a REAL money account. That is acceptable here for exactly one reason:
//! **every call in this file is a signed GET and nothing else.** No order is placed, amended or
//! cancelled; no transfer, leverage, margin or account setting is touched; there is no POST, PUT or
//! DELETE anywhere in this module — deliberately including the order-lifecycle test the binance
//! sibling carries, which has no safe analog on a mainnet-in-practice venue. It is the same
//! read-only class as `aster_smoke.rs`'s `live_balance_auth` (a signed `GET /fapi/v3/balance` that
//! is already run against these creds), which is why it resolves LIVE-then-TESTNET without the
//! extra opt-in flag `aster_userdata_soak.rs` requires for its 15-minute authenticated session.
//!
//! ## The fee question — SPOT is now a live measurement end-to-end
//!
//! `aster_spot_fee_rates_smoke` is the reason this file was written. `vike_model::fee_schedule_for`
//! USED TO carry a SELF-DECLARED ASSUMPTION for aster's spot lane (a Binance-perp-shaped 2/5 bps),
//! and the only way to replace an assumption with a measurement is to ask the venue.
//!
//! The first answer was a dead end, and worth recording because it is the reason the wiring looks
//! the way it does. The obvious path needed no new endpoint — read `commissionRates{maker,taker}`
//! off the SAME `/api/v3/account` body `fetch_balance` already fetches, exactly as binance does —
//! and **it measured `None` (2026-08-05, mainnet, HTTP 200)**: aster's spot account body carries
//! exactly seven top-level keys — `balances`, `canBurnAsset`, `canDeposit`, `canTrade`,
//! `canWithdraw`, `feeTier`, `updateTime` — and NEITHER the `commissionRates` object NOR Binance's
//! older integer `makerCommission`/`takerCommission` pair. Its one fee-shaped field is `feeTier: 0`,
//! a TIER INDEX carrying no rate. (That is itself useful: tier 0 is the base schedule, which is the
//! tier `vike_model::fee_schedule_for` documents itself as carrying.)
//!
//! So the lane was re-pointed rather than trusted. `ASTER_RECON` now sets the shared
//! `ReconPaths::spot_commission_rate` slot to `crates/bridges/aster/src/spot.rs`'s
//! `PATH_COMMISSION_RATE` — the venue's OWN per-symbol `GET /api/v3/commissionRate` — and
//! `aster_spot_fee_rates_smoke` below asserts the production `fetch_fee_rates` returns a REAL RATE
//! there, not the `Ok(None)` it used to. Measured **maker 0.5 bps / taker 4 bps for `BTCUSDT`**,
//! digit-for-digit Aster's published spot schedule, so aster's spot row is the one row in that table
//! that is a live measurement agreeing with a published claim rather than a transcribed claim alone.
//!
//! ⚠ The rate is PER-SYMBOL (the venue's own docs price `APXUSDT` at 2/7 bps) while the static row
//! is one flat pair. They agree on `BTCUSDT`; they need not agree everywhere.
//!
//! ## The PERP fee number — cross-checked by `aster_perp_commission_rate_probe`, still unwired
//!
//! `vike_model::fee_schedule_for("aster-perp")` carries **0 bps maker / 4 bps taker** from
//! `docs.asterdex.com`, and a 0-bps maker is the more perishable half of that table: if it were
//! promotional, or simply wrong, the row would OVERSTATE every maker strategy's edge — the direction
//! that flatters a backtest. Unlike the spot row it has no `fetch_fee_rates` cross-check, because
//! `ASTER_RECON`'s `perp_commission_rate` stays `None` (`aster_perp_reconcile_fetch_smoke` pins that
//! inertness with a zero-network assertion).
//!
//! `aster_perp_commission_rate_probe` closes that gap WITHOUT wiring the lane: one read-only signed
//! GET to `/fapi/v3/commissionRate`, reporting what the account is actually charged. The endpoint is
//! documented — `V3(Recommended)/EN/aster-finance-futures-api-v3.md` § "User Commission Rate
//! (USER_DATA)", <https://github.com/asterdex/api-docs>, read 2026-08-05 — so this is a documented
//! GET, not a guessed path.
//!
//! **It answered (2026-08-05, mainnet `BTCUSDT`, HTTP 200): `makerCommissionRate: "0"`,
//! `takerCommissionRate: "0.000400"`** — the published pair exactly. So the 0-bps maker is real for
//! this account today rather than a documentation artifact, which is worth knowing precisely because
//! the venue's OWN API-doc example for this endpoint shows `0.0002`/`0.0004` and contradicts it.
//!
//! ⚠ What that does NOT settle: a live `0` cannot distinguish a permanent schedule from a running
//! promotion. See `vike_model::fees`' `fee_schedule_for` `"aster-perp"` arm for the risk that leaves
//! and its direction. This probe REPORTS rather than asserts equality — a future divergence is a
//! finding to read into that row, not a test failure.
//!
//! ## What the first run found beyond the fee question — FIXED 2026-08-05
//!
//! Aster's spot `/api/v3/myTrades` answered **404**: the endpoint is `userTrades`, and its rows are
//! futures-shaped. Both halves are fixed (`crates/bridges/aster/src/spot.rs`'s
//! `PATH_SPOT_USER_TRADES` and `vike_binance::family::recon`'s `fill_side`), and
//! `aster_spot_reconcile_fetch_smoke` now asserts the verb like every other, with no known-gap
//! branch left to hide behind.

use vike_aster::recon_client::recon_client;
use vike_aster::signing::load_aster_credentials;
use vike_bridge_core::credentials::{Credentials, Environment, load_workspace_dotenv_from};
use vike_model::{FeeSchedule, fee_schedule_for};
// NOTE no `use vike_exec::recon::ReconClient`: the factory hands back a `Box<dyn ReconClient>`, and
// a trait object's own methods resolve without the trait in scope (importing it is a dead import
// clippy rejects under `-D warnings`). The siblings import it because they hold CONCRETE clients.

/// Spot lane (bare symbol). The `.P` suffix routes the perp lane — see `recon_client`'s doc.
const SPOT_SYMBOL: &str = "BTCUSDT";
const PERP_SYMBOL: &str = "BTCUSDT.P";

/// The double-gate every `*_smoke.rs` in this crate uses (see `aster_smoke.rs`'s `resolve`): load
/// the workspace `.env`, resolve aster's bespoke agent-wallet creds LIVE-then-TESTNET, and return
/// `None` (after a printed reason) when neither tier is present so each caller can self-skip via
/// `let Some((env, creds)) = resolve(test_name) else { return };`.
///
/// Aster has no standard `{VENUE}_DEMO_*` shape — `load_aster_credentials` reads
/// `ASTER_{LIVE,TESTNET}_USER` + `_PRIVATE_KEY` (+ an optional `_SIGNER`), which is why the gate
/// cannot be the usual `load_credentials_from` the binance/okx siblings call.
fn resolve(test: &str) -> Option<(Environment, Credentials)> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let resolved =
        load_aster_credentials(Environment::Live, &vars).map(|c| (Environment::Live, c)).or_else(
            || load_aster_credentials(Environment::Demo, &vars).map(|c| (Environment::Demo, c)),
        );
    match &resolved {
        None => {
            // Key NAMES only — never a value, here or anywhere else in this file.
            println!(
                "SKIP {test}: no ASTER_LIVE_USER/_PRIVATE_KEY nor ASTER_TESTNET_USER/_PRIVATE_KEY \
                 in the workspace .env"
            );
            tracing::warn!(target: "vike_aster", "SKIP {test}: ASTER_{{LIVE,TESTNET}} creds absent");
        }
        Some((Environment::Live, _)) => {
            println!(
                "⚠ {test}: no ASTER_TESTNET_* configured — authenticating against the REAL MAINNET \
                 account. READ-ONLY: signed GETs only; places nothing, cancels nothing, moves nothing."
            );
        }
        Some((env, _)) => println!("{test}: using the {env:?} tier (testnet hosts)"),
    }
    resolved
}

/// Render a fetched schedule as maker/taker bps. `FeeSchedule::from_binance_rates` always builds
/// [`FeeSchedule::PercentMakerTaker`] from the venue's fractions, so any other variant would mean
/// the parser changed shape under us — worth saying out loud rather than silently formatting.
fn as_bps(s: &FeeSchedule) -> String {
    match s {
        FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
            format!("maker {maker_bps} bps / taker {taker_bps} bps")
        }
        other => format!("UNEXPECTED variant for a fetched crypto fee: {other:?}"),
    }
}

/// One read-only signed GET, built from the production `AsterSigner` + `UreqTransport` exactly as
/// `crates/bridges/aster/src/recon_client.rs`'s `recon_client` factory builds them (no second
/// signer, no bespoke transport), returning the raw body or the transport's own error string.
///
/// `host` is a resolved base URL from `vike_aster::urls::urls_for` — `sapi_rest` for the spot API,
/// `fapi_rest` for the perp one — and `gate` the matching rate gate, so a probe cannot accidentally
/// spend the other lane's budget. **GET only**: the method is hardcoded, so no caller in this file
/// can reach a write verb even by accident. That is a structural guarantee, not a convention.
fn signed_get(
    creds: &Credentials,
    host: &str,
    path: &str,
    params: &[(&str, String)],
    gate: vike_bridge_core::ratelimit::RateGate,
) -> Result<serde_json::Value, String> {
    use vike_bridge_core::transport::RestTransport;
    let transport = vike_bridge_core::UreqTransport::new("aster").with_rate_gate(gate);
    let signer = vike_aster::signing::AsterSigner::new(creds, vike_model::now_us);
    transport.signed(host, path, "GET", params, &signer).map_err(|e| e.to_string())
}

/// Diagnostic for an ABSENT fee field: say what a body actually CARRIES, so "the parser found
/// nothing" is distinguishable from "the venue publishes the fee under a different name". Binance's
/// account body carries BOTH shapes — the integer `makerCommission`/`takerCommission` pair AND the
/// `commissionRates` object — and a fork that kept only one would be a live finding, not a dead end.
/// This is exactly how aster's `/api/v3/account` was proven to price nothing at all.
///
/// ⚠ It prints top-level KEY NAMES only. A VALUE is printed for one narrow allowlist — a scalar
/// whose key mentions commission or fee — because that is the answer being sought; balances,
/// addresses and every nested object stay unprinted.
fn report_fee_shape(label: &str, body: &serde_json::Value) {
    let Some(obj) = body.as_object() else {
        println!("  ({label} did not return a JSON object — nothing to survey)");
        return;
    };

    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    println!("  {label} top-level KEYS (names only): {keys:?}");

    let fee_ish: Vec<String> = obj
        .iter()
        .filter(|(k, _)| {
            let lower = k.to_lowercase();
            lower.contains("commission") || lower.contains("fee")
        })
        .map(|(k, v)| match v {
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => format!("{k}=<nested>"),
            scalar => format!("{k}={scalar}"),
        })
        .collect();
    if fee_ish.is_empty() {
        println!("  fee/commission-shaped fields: NONE — this endpoint prices nothing.");
    } else {
        println!("  fee/commission-shaped fields: {}", fee_ish.join(", "));
    }
}

/// Plausibility bounds every fetched crypto fee must satisfy. A rate above 100 bps (1%) would mean
/// the WRONG FIELD was parsed, not that the account sits on a punitive tier — that is the failure
/// worth catching, and it is the only one asserted: a real rate that merely disagrees with a
/// published schedule is a finding to report, not a test to fail.
fn assert_plausible(what: &str, s: &FeeSchedule) {
    let FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } = *s else {
        panic!("{what}: expected PercentMakerTaker from a fetched crypto fee, got {s:?}");
    };
    assert!(maker_bps.is_finite() && taker_bps.is_finite(), "{what}: non-finite fee rates");
    assert!(maker_bps >= 0.0 && taker_bps >= 0.0, "{what}: negative fees {maker_bps}/{taker_bps}");
    assert!(
        maker_bps <= 100.0 && taker_bps <= 100.0,
        "{what}: implausible fee (>100 bps) — likely a mis-parsed field: {maker_bps}/{taker_bps}"
    );
}

/// READ-ONLY: the four report verbs on the SPOT lane, exactly the surface the okx sibling covers
/// (`fetch_balance`/`fetch_order_status_reports`/`fetch_fill_reports`/
/// `fetch_position_status_reports`), asserting each succeeds and parses into plausible,
/// internally-consistent values.
///
/// Spot has no venue-native position report — the shared `FamilyReconClient` answers `Ok(vec![])`
/// with no network call at all (see `vike_binance::family::recon`'s `fetch_position_status_reports`).
/// Asserting it live pins the trait dispatch through `Box<dyn ReconClient>`, not just the parser.
///
/// ⚠ **The FILL verb is what this smoke's first run found broken**, and it is the reason the file
/// earned its keep: `ReconPaths::spot_my_trades` pointed at the Binance-fork `/api/v3/myTrades`,
/// which `sapi.asterdex.com` answers **404**, so aster's spot `fetch_fill_reports` returned `Err`
/// on every reconcile pass in production — and since `vike_exec::recon::run_pass` short-circuits on
/// the first fetch error, that aborted the WHOLE spot pass, orders and positions included.
///
/// Fixed 2026-08-05 on both axes (path AND row grammar — see `crate::spot::PATH_SPOT_USER_TRADES`
/// and `vike_binance::family::recon::fill_side`), so the verb is now asserted exactly like its
/// three siblings: a plain `.expect`, with **no known-gap branch**. A 404 here again means the
/// endpoint moved; any other error means a real outage. The two are no longer conflated, which is
/// the property that matters — an `Ok(vec![])` (no fills in the lookback) and a failed fetch must
/// not look alike to `diff`, or a genuine outage becomes invisible.
///
/// ⚠ Note the account is not expected to HAVE spot fills, so `fills.len()` may legitimately be 0.
/// That is the point: 0-with-`Ok` is a real answer, and it is a different answer from `Err`.
#[test]
#[ignore = "network + live creds — READ-ONLY signed GETs against MAINNET (see module doc)"]
fn aster_spot_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some((env, creds)) = resolve("aster_spot_reconcile_fetch_smoke") else { return };
    let client = recon_client(env, &creds, SPOT_SYMBOL).expect("aster recon_client is always Some");

    // Aster's spot USDT funding is not a documented contract for this account — accept `None`/
    // `Some`, but a `Some` value must be a real, non-negative number, never garbage. (Same shape
    // as the binance spot sibling, which makes the identical argument about its demo wallet.)
    let balance = client.fetch_balance().expect("fetch_balance (spot)");
    println!("aster spot USDT free balance: {balance:?}");
    match balance {
        Some(b) => assert!(b.is_finite() && b >= 0.0, "implausible spot balance: {b}"),
        None => println!("  (no USDT row in the spot account body)"),
    }

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports (spot)");
    println!("aster spot open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "aster");
        assert_eq!(o.symbol, SPOT_SYMBOL);
    }

    // The verb that used to 404 (see this test's doc). Asserted plainly now — no known-gap branch.
    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports (spot)");
    println!("aster spot recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "aster");
        assert_eq!(f.symbol, SPOT_SYMBOL);
        // The grammar half of the same bug: aster's spot rows spell side `side`/`maker`, so an
        // `isBuyer`-only reader signs EVERY row -1. A live row that parsed through the union must
        // carry a real direction and plausible economics.
        assert!(f.side == 1 || f.side == -1, "fill side must be ±1, got {}", f.side);
        assert!(f.last_qty > 0.0 && f.last_px > 0.0, "implausible fill: {f:?}");
        assert!(!f.trade_id.as_str().is_empty(), "a fill must carry a venue trade id");
    }

    let positions =
        client.fetch_position_status_reports().expect("fetch_position_status_reports (spot)");
    assert!(positions.is_empty(), "spot ReconClient must report no positions: {positions:?}");

    println!(
        "aster spot reconcile fetch smoke green: balance+orders+FILLS+positions fetched live \
         (all four verbs — the fill lane no longer 404s)"
    );
}

/// **The measurement this file exists for, and the proof the spot fee lane is WIRED.**
///
/// READ-ONLY: the PRODUCTION `fetch_fee_rates` on the spot lane, through the production factory.
/// It now routes to the venue's own per-symbol `GET /api/v3/commissionRate`
/// (`ASTER_RECON`'s `spot_commission_rate` → `crates/bridges/aster/src/spot.rs`'s
/// `PATH_COMMISSION_RATE`) instead of scraping `commissionRates` off `/api/v3/account`, because
/// aster's fork of that account body carries no such object — see the module doc for the
/// measurement that established it.
///
/// ⚠ **`Some` is ASSERTED here, and that assertion is the whole point.** Before the wiring this
/// verb answered `Ok(None)` on every pass against the real venue; a `None` today means the lane
/// regressed to a body that prices nothing, which is precisely the silent failure the wiring
/// removes. The VALUES stay report-only (`assert_plausible` catches a mis-parsed field and nothing
/// else): the endpoint is per-symbol and tier-dependent, so a live rate differing from the flat
/// static row is a finding to read into `vike_model::fees`, not a test failure.
#[test]
#[ignore = "network + live creds — READ-ONLY signed GET /api/v3/commissionRate against MAINNET (see module doc)"]
fn aster_spot_fee_rates_smoke() {
    vike_log::test_init();
    let Some((env, creds)) = resolve("aster_spot_fee_rates_smoke") else { return };
    let client = recon_client(env, &creds, SPOT_SYMBOL).expect("aster recon_client is always Some");

    let fetched = client.fetch_fee_rates().expect("fetch_fee_rates (spot)");
    let static_row = fee_schedule_for("aster");

    println!("---- aster SPOT fee rates ({SPOT_SYMBOL}, live fetch_fee_rates) ----");
    println!("  static vike_model::fee_schedule_for(\"aster\"): {}", as_bps(&static_row));
    match &fetched {
        Some(live) => {
            println!("  LIVE per-symbol rates: {}", as_bps(live));
            println!(
                "  published spot schedule (docs.asterdex.com, 2026-08-05): maker 0.5 bps / \
                 taker 4 bps"
            );
            if live == &static_row {
                println!("  => the static row MATCHES this account's live per-symbol rates.");
            } else {
                println!(
                    "  => DIVERGENT. Read before trusting the static row: a per-symbol rate, a \
                     maker-program tier or a schedule change all land here."
                );
            }
        }
        None => {
            // Diagnostic before the failure, so the report says WHY rather than just "None".
            println!("  LIVE rates: ABSENT — surveying the endpoint body to say what it carries:");
            match signed_get(
                &creds,
                vike_aster::urls::urls_for(env).sapi_rest,
                vike_aster::spot::PATH_COMMISSION_RATE,
                &[("symbol", SPOT_SYMBOL.to_string())],
                vike_aster::ratelimit::spot_rest_gate(),
            ) {
                Ok(body) => report_fee_shape("/api/v3/commissionRate", &body),
                Err(e) => println!("  (the survey read failed too: {e})"),
            }
        }
    }
    println!("-------------------------------------------------------------------");

    let live = fetched.expect(
        "aster's spot fee lane is WIRED to GET /api/v3/commissionRate and must return the \
         account's real rates — an Ok(None) here is the pre-wiring silence regressing",
    );
    assert_plausible("aster spot", &live);
}

/// **TASK-2 CROSS-CHECK for `vike_model::fee_schedule_for("aster-perp")`** — the published-only row.
///
/// That row carries 0 bps maker / 4 bps taker from
/// <https://docs.asterdex.com/trading/perpetuals/fees-and-specs/fees> (read 2026-08-05,
/// § "Fee Rates for USDT-Perpetual Contracts"; the page carries no promotional qualifier, no expiry
/// and no tier grid). A 0-bps maker is the most perishable number in that table and it errs in the
/// dangerous direction — if it is promotional, every maker-strategy backtest is flattered. So it
/// gets asked directly.
///
/// `GET /fapi/v3/commissionRate` (weight 20, `symbol` required) is DOCUMENTED —
/// `V3(Recommended)/EN/aster-finance-futures-api-v3.md` § "User Commission Rate (USER_DATA)",
/// <https://github.com/asterdex/api-docs>, read 2026-08-05 — so this is a documented read verb, not
/// a guessed path. ⚠ Note the doc's own response EXAMPLE shows `0.0002`/`0.0004` (2/4 bps), which
/// contradicts the fee page's 0-bps maker; doc examples in this family are routinely copied from
/// Binance, which is exactly why the live account is the tiebreaker and not either document.
///
/// ⚠ This deliberately does NOT wire the lane. `ASTER_RECON`'s `perp_commission_rate` stays `None`
/// and `aster_perp_reconcile_fetch_smoke` still pins that inertness: probing read-only from a test
/// and putting a live-money venue's reconcile path on a new endpoint are different decisions, and
/// only the first is in scope here. It REPORTS rather than asserts equality with the static row —
/// only implausible values (non-finite, negative, >100 bps) fail.
#[test]
#[ignore = "network + live creds — READ-ONLY signed GET /fapi/v3/commissionRate against MAINNET (see module doc)"]
fn aster_perp_commission_rate_probe() {
    vike_log::test_init();
    let Some((env, creds)) = resolve("aster_perp_commission_rate_probe") else { return };

    // The perp lane's own host and rate gate — a probe must not spend the spot budget.
    let body = match signed_get(
        &creds,
        vike_aster::urls::urls_for(env).fapi_rest,
        "/fapi/v3/commissionRate",
        &[("symbol", SPOT_SYMBOL.to_string())],
        vike_aster::ratelimit::perp_rest_gate(),
    ) {
        Ok(v) => v,
        Err(e) => {
            // A documented endpoint that does not answer is a REPORTED finding, not a failure —
            // the static row stands on the published schedule either way, and the report must say
            // so rather than leave a green tick implying verification happened.
            println!(
                "---- aster PERP commissionRate ({SPOT_SYMBOL}, GET /fapi/v3/commissionRate) ----\n\
                   DID NOT ANSWER: {e}\n  => UNVERIFIED. vike_model::fee_schedule_for(\"aster-perp\") \
                 stays published-only; read its doc for the risk that leaves open.\n\
                 -------------------------------------------------------------------"
            );
            return;
        }
    };

    println!("---- aster PERP commissionRate ({SPOT_SYMBOL}, GET /fapi/v3/commissionRate) ----");
    report_fee_shape("/fapi/v3/commissionRate", &body);
    let static_row = fee_schedule_for("aster-perp");
    println!("  static vike_model::fee_schedule_for(\"aster-perp\"): {}", as_bps(&static_row));
    println!(
        "  published perp schedule (docs.asterdex.com, 2026-08-05): maker 0 bps / taker 4 bps"
    );

    let fetched = vike_aster::recon_client::parse_perp_fee_rates(&body.to_string())
        .expect("commissionRate body must parse");
    match &fetched {
        Some(live) => {
            println!("  LIVE per-symbol rates: {}", as_bps(live));
            if live == &static_row {
                println!(
                    "  => the static row MATCHES this account's live perp rates — the published \
                     0-bps maker is real for this account TODAY, not a documentation artifact."
                );
            } else {
                println!(
                    "  => DIVERGENT. This is the finding the row's doc warns about: read it into \
                     `vike_model::fees` before trusting any maker-heavy perp backtest."
                );
            }
            assert_plausible("aster perp", live);
        }
        None => println!(
            "  no makerCommissionRate/takerCommissionRate in the body — the endpoint answered but \
             prices nothing for this symbol, so the row stays published-only."
        ),
    }
    println!("-------------------------------------------------------------------");
}

/// READ-ONLY: the four report verbs on the PERP lane (the `.P` route), the sibling of the binance
/// perp fetch smoke. `positionRisk` echoes the requested symbol even when flat — see
/// `vike_binance::family::recon`'s module doc for why that flat row is load-bearing for `diff`.
///
/// Also pins the perp FEE lane as inert. `ASTER_RECON`'s `perp_commission_rate` is `None`, so this
/// `fetch_fee_rates` short-circuits to `Ok(None)` and issues NO request — the assertion below is a
/// zero-network guard that the un-wired decision stays un-wired, deliberately NOT a probe of the
/// unverified endpoint.
#[test]
#[ignore = "network + live creds — READ-ONLY signed GETs against MAINNET (see module doc)"]
fn aster_perp_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some((env, creds)) = resolve("aster_perp_reconcile_fetch_smoke") else { return };
    let client = recon_client(env, &creds, PERP_SYMBOL).expect("aster recon_client is always Some");

    // The perp lane's `GET /fapi/v3/balance` is the call `aster_smoke.rs`'s `live_balance_auth`
    // already proves live against these creds; accept `None`/`Some` but demand plausibility.
    let balance = client.fetch_balance().expect("fetch_balance (perp)");
    println!("aster perp USDT wallet balance: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite() && b >= 0.0, "implausible perp balance: {b}");
    }

    // The factory strips `.P` before it hits the wire, so every report echoes the bare API symbol.
    let api_symbol = SPOT_SYMBOL;

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports (perp)");
    println!("aster perp open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "aster");
        assert_eq!(o.symbol, api_symbol);
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports (perp)");
    println!("aster perp recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "aster");
        assert_eq!(f.symbol, api_symbol);
    }

    let positions =
        client.fetch_position_status_reports().expect("fetch_position_status_reports (perp)");
    assert!(!positions.is_empty(), "perp positionRisk must echo {api_symbol} even when flat");
    assert_eq!(positions[0].symbol, api_symbol);
    println!("aster perp position: qty={} avg_px={}", positions[0].qty, positions[0].avg_px);

    // Zero-network: `perp_commission_rate: None` short-circuits before any request is built.
    let perp_fees = client.fetch_fee_rates().expect("fetch_fee_rates (perp)");
    assert!(
        perp_fees.is_none(),
        "aster's perp fee endpoint is deliberately unwired (unverified) — it must stay inert, \
         never probed against a live account: {perp_fees:?}"
    );

    println!("aster perp reconcile fetch smoke green: balance+orders+fills+positions fetched live");
}

// --- PER-SYMBOL FEE SWEEP (the "is one flat row per lane right?" measurement) -------------------
//
// Everything below this line is ONE read-only investigation: `vike_model::fee_schedule_for` returns
// a single flat pair per lane, and BOTH aster fee endpoints take a `symbol` parameter. Whether the
// answer actually varies with that parameter is a question only the live venue can settle — the
// published fee pages state flat rates (spot) and per-CONTRACT-FAMILY rates (perp), and neither is
// a per-symbol table. So the venue gets asked, once per representative symbol, and the result is
// printed as a table rather than asserted against a guess.

/// The perp lane's commission-rate path. Deliberately a LOCAL const rather than a `vike_aster`
/// export: `ASTER_RECON`'s `perp_commission_rate` slot is still `None` on purpose (see
/// `crates/bridges/aster/src/recon_client.rs`), so this path must not gain a production home just
/// because a probe reads it.
const PATH_PERP_COMMISSION_RATE: &str = "/fapi/v3/commissionRate";

/// SPOT probe set — picked from the venue's OWN live symbol list (keyless
/// `GET https://sapi.asterdex.com/api/v3/exchangeInfo`, read 2026-08-05: 62 pairs, all
/// `status: "TRADING"`), to span every axis a per-symbol rate could plausibly key off rather than
/// sampling one corner of it:
///
///   - the majors and the venue token (`BTCUSDT`/`ETHUSDT`/`SOLUSDT`/`BNBUSDT`/`ASTERUSDT`) —
///     `BTCUSDT` is the ONE symbol the flat `"aster"` row was ever measured against,
///   - a stablecoin pair (`USDCUSDT`) and the venue's own `USD1USDT`,
///   - each NON-USDT quote asset the venue actually lists — `USD1` (9 pairs, sampled by `BUSD1`
///     and `ANUSD1`) and `FORM` (2 pairs, sampled by `CDLFORM`) — because a quote-asset-keyed rate
///     is the most likely shape for a spot fee to vary on,
///   - two long-tail listings (`GIGGLEUSDT`, `4USDT`) to catch a rate that keys off listing age or
///     tier rather than asset class.
///
/// `APXUSDT` is LAST and is expected to fail. It is the symbol the venue's own API doc prices at
/// 2 bps / 7 bps in the `commissionRate` RESPONSE EXAMPLE — the single piece of published evidence
/// that aster spot fees are per-symbol at all — and it does not appear in the venue's live symbol
/// list. Probing it turns "that example may be stale" from a hunch into a recorded observation.
const SPOT_PROBE_SYMBOLS: &[(&str, &str)] = &[
    ("BTCUSDT", "major; the only symbol the flat row was measured against"),
    ("ETHUSDT", "major"),
    ("SOLUSDT", "major"),
    ("BNBUSDT", "major"),
    ("ASTERUSDT", "the venue's own token"),
    ("USDCUSDT", "stablecoin pair"),
    ("USD1USDT", "stablecoin pair, USD1 as BASE"),
    ("BUSD1", "quote=USD1"),
    ("ANUSD1", "quote=USD1"),
    ("CDLFORM", "quote=FORM, the venue's third quote asset"),
    ("GIGGLEUSDT", "long-tail listing"),
    ("4USDT", "long-tail listing"),
    ("APXUSDT", "NOT LISTED: the API doc's 2/7 bps response example; expected to fail"),
];

/// PERP probe set — same method, from `GET https://fapi.asterdex.com/fapi/v3/exchangeInfo`
/// (read 2026-08-05: 523 `TRADING` symbols, every one `contractType: "PERPETUAL"`).
///
/// This lane has a PUBLISHED reason to be non-flat that spot does not: the fee page prices three
/// CONTRACT FAMILIES apart — USDT-Perp 0/0.04%, USD1-Perp 0/0.005%, Stock-Perp 0/0.009%
/// (<https://docs.asterdex.com/trading/perpetuals/fees-and-specs/fees>, read 2026-08-05) — an 8x
/// and a 4.4x taker spread against the one rate `fee_schedule_for("aster-perp")` charges everyone.
/// The venue's `exchangeInfo` even carries the classification (`underlyingSubType`), so the set
/// below samples each class it reports:
///
///   `""` (plain crypto, 319 symbols), `STOCK` (90) and its `STOCK+Semiconductor` (3) /
///   `STOCK+ETF` (2) blends, `ETF` (6), `Commodities` (9), `Meme` (34), `AI` (38), `Top` (16),
///   plus the three `USD1`-quoted (`BTCUSD1`/`ETHUSD1`/`SOLUSD1`) and two `U`-quoted
///   (`BTCU`/`ETHU`, `underlyingSubType: ["AOS2"]`) contracts.
///
/// `APXUSDT` rides along for the same reason as on the spot lane (the futures doc's own example
/// says `BTCUSDT` 2/4 bps, which the 2026-08-05 live read already contradicted at 0/4).
const PERP_PROBE_SYMBOLS: &[(&str, &str)] = &[
    ("BTCUSDT", "crypto USDT-Perp; the only symbol the flat row was measured against"),
    ("ETHUSDT", "crypto USDT-Perp"),
    ("ASTERUSDT", "the venue's own token"),
    ("1000PEPEUSDT", "underlyingSubType=Meme"),
    ("TAOUSDT", "underlyingSubType=AI"),
    ("AAPLUSDT", "underlyingSubType=STOCK; docs price Stock-Perp at 0/0.009%"),
    ("TSLAUSDT", "underlyingSubType=STOCK"),
    ("NVDAUSDT", "underlyingSubType=STOCK+Semiconductor"),
    ("SPYUSDT", "underlyingSubType=ETF"),
    ("QQQUSDT", "underlyingSubType=ETF"),
    ("XAUUSDT", "underlyingSubType=Commodities"),
    ("XAGUSDT", "underlyingSubType=Commodities"),
    ("BTCUSD1", "quote=USD1; docs price USD1-Perp at 0/0.005%"),
    ("ETHUSD1", "quote=USD1"),
    ("SOLUSD1", "quote=USD1"),
    ("BTCU", "quote=U, underlyingSubType=AOS2"),
    ("BTCDOMUSDT", "index contract"),
    ("APXUSDT", "NOT LISTED: the doc's response-example symbol; expected to fail"),
];

/// One row of the sweep: ask ONE lane for ONE symbol's commission rate and render the outcome as a
/// table line. Returns the parsed schedule so the caller can summarise.
///
/// **GET only** — it can only reach [`signed_get`], whose method is hardcoded. A symbol the venue
/// does not list answers an error rather than a rate, and that error is PRINTED (venue code + msg,
/// which is where the HTTP status lands for a non-JSON error body) instead of failing the sweep:
/// "this endpoint refuses this symbol" is a finding, and one refusal must not hide the other rows.
fn probe_one(
    creds: &Credentials,
    host: &str,
    path: &str,
    gate: vike_bridge_core::ratelimit::RateGate,
    symbol: &str,
    note: &str,
) -> Option<FeeSchedule> {
    match signed_get(creds, host, path, &[("symbol", symbol.to_string())], gate) {
        Err(e) => {
            println!("  {symbol:<14} ERR    {e}   [{note}]");
            None
        }
        Ok(body) => match vike_aster::recon_client::parse_perp_fee_rates(&body.to_string())
            .expect("commissionRate body must parse as JSON")
        {
            Some(FeeSchedule::PercentMakerTaker { maker_bps, taker_bps }) => {
                println!(
                    "  {symbol:<14} 200    maker {maker_bps:>8} bps / taker {taker_bps:>8} bps   \
                     [{note}]"
                );
                Some(FeeSchedule::PercentMakerTaker { maker_bps, taker_bps })
            }
            Some(other) => {
                println!("  {symbol:<14} 200    UNEXPECTED variant {other:?}   [{note}]");
                Some(other)
            }
            None => {
                // The endpoint answered but carried no rate fields — a body-shape finding.
                println!("  {symbol:<14} 200    NO RATE FIELDS   [{note}]");
                report_fee_shape(symbol, &body);
                None
            }
        },
    }
}

/// Print the DISTINCT rates the sweep observed, so the table's conclusion is not left to the eye.
fn summarise(lane: &str, rows: &[(&str, Option<FeeSchedule>)], static_row: &FeeSchedule) {
    let mut distinct: Vec<(String, Vec<&str>)> = Vec::new();
    for (sym, sched) in rows {
        let Some(s) = sched else { continue };
        let key = as_bps(s);
        match distinct.iter_mut().find(|(k, _)| *k == key) {
            Some((_, syms)) => syms.push(sym),
            None => distinct.push((key, vec![sym])),
        }
    }
    println!("  ---- {lane}: {} DISTINCT rate(s) observed ----", distinct.len());
    for (rate, syms) in &distinct {
        println!("    {rate}  <- {syms:?}");
    }
    println!("    static vike_model row: {}", as_bps(static_row));
    let answered = rows.iter().filter(|(_, s)| s.is_some()).count();
    println!("    ({answered}/{} probed symbols answered with a rate)", rows.len());
}

/// **The per-symbol fee measurement.** READ-ONLY: one signed `GET .../commissionRate` per symbol
/// per lane, nothing else — no order, no cancel, no write verb is reachable from here (see
/// [`signed_get`]).
///
/// ## Why this exists
///
/// `vike_model::fee_schedule_for` answers ONE flat pair per lane (`"aster"` 0.5/4 bps, `"aster-perp"`
/// 0/4 bps), both established against `BTCUSDT` alone. Both aster fee endpoints take a `symbol`, and
/// the perp fee PAGE prices three contract families 8x apart, so a flat row is a claim about 61 spot
/// and 522 perp symbols that was never checked. Every paper/backtest mount on any of them spends
/// that claim.
///
/// ## What it does and does not assert
///
/// It asserts only PLAUSIBILITY of a returned rate ([`assert_plausible`]) and that both lanes
/// answered for `BTCUSDT` — never a specific number, and never equality with the static row. A
/// divergent rate is the FINDING this test is for; failing on it would make the finding
/// unreportable. A symbol the venue does not list simply prints its error row.
#[test]
#[ignore = "network + live creds — READ-ONLY signed GET .../commissionRate per symbol against MAINNET"]
fn aster_per_symbol_commission_rate_sweep() {
    vike_log::test_init();
    let Some((env, creds)) = resolve("aster_per_symbol_commission_rate_sweep") else { return };
    let urls = vike_aster::urls::urls_for(env);

    println!("==== ASTER SPOT  GET {} ====", vike_aster::spot::PATH_COMMISSION_RATE);
    let spot_rows: Vec<(&str, Option<FeeSchedule>)> = SPOT_PROBE_SYMBOLS
        .iter()
        .map(|(sym, note)| {
            let got = probe_one(
                &creds,
                urls.sapi_rest,
                vike_aster::spot::PATH_COMMISSION_RATE,
                vike_aster::ratelimit::spot_rest_gate(),
                sym,
                note,
            );
            if let Some(s) = &got {
                assert_plausible(&format!("aster spot {sym}"), s);
            }
            (*sym, got)
        })
        .collect();
    summarise("SPOT", &spot_rows, &fee_schedule_for("aster"));

    println!();
    println!("==== ASTER PERP  GET {PATH_PERP_COMMISSION_RATE} ====");
    let perp_rows: Vec<(&str, Option<FeeSchedule>)> = PERP_PROBE_SYMBOLS
        .iter()
        .map(|(sym, note)| {
            let got = probe_one(
                &creds,
                urls.fapi_rest,
                PATH_PERP_COMMISSION_RATE,
                vike_aster::ratelimit::perp_rest_gate(),
                sym,
                note,
            );
            if let Some(s) = &got {
                assert_plausible(&format!("aster perp {sym}"), s);
            }
            (*sym, got)
        })
        .collect();
    summarise("PERP", &perp_rows, &fee_schedule_for("aster-perp"));

    // The ONLY hard assertion: the sweep must have actually talked to the venue. Without this a
    // total auth/geo failure would print 31 error rows and pass green, and the report would read as
    // "no per-symbol variation found" when nothing was measured at all.
    let majors_answered = spot_rows
        .iter()
        .chain(perp_rows.iter())
        .filter(|(s, r)| *s == "BTCUSDT" && r.is_some())
        .count();
    assert_eq!(
        majors_answered, 2,
        "neither lane answered for BTCUSDT — the sweep measured nothing; treat every row above as \
         UNVERIFIED rather than as evidence of a flat schedule"
    );
}
