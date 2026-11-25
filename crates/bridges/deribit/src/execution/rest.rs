//! VenueRestDeribit — JSON-RPC private/buy|sell|cancel over the order transport. Exact port
//! of `exec/deribit/client.py` (NOT a REST/HMAC client — Deribit is JSON-RPC/WS; it
//! implements the same `VenueRest` seam so `ExecutionClientRest` reuses the live order path).
//!
//! amount/price are COIN units (options); **post_only is forced FALSE** (Deribit
//! defaults it true — a crossing order would otherwise be rejected/repriced: the trap
//! the Python program hit live) — except for an order that ASKED to be post-only
//! (`TimeInForce::PostOnly`), which sends `post_only` + `reject_post_only` both true so a
//! crossing order is refused, never re-priced. Instruments: tick_size + min_trade_amount from
//! public/get_instruments.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use serde_json::{Value, json};

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::rest::{VenueRest, amend_answer_time, modify_rejected};
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModified, OrderModifyRejected, OrderRejected,
    OrderSubmitted,
};
use vike_model::{
    AssetClass, MoneyModel, MoneyModelRefusal, OrderRefusalId, OrderRequest, OrderType,
    SymbolProperties, TimeInForce, TriggerBy,
};
use vike_utility::mutex::lock;
use vike_venues::venue_delivery_guard::{DeliveryGuardVerdict, delivery_verdict};

use crate::execution::combo::{
    ComboMapError, build_combo_order, build_combo_order_params, build_create_combo_params,
    map_combo, parse_combo, parse_combo_grid,
};
use crate::transport::client::{CallError, DeribitTransport, redial_bound, reply_bound};
use crate::transport::rpc::parse_response;
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::{json_number, json_text};
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::trigger::venue_trigger_by;

/// cancel of an already-gone order — swallowed (unknown ≠ rejection):
/// 10004 order_not_found, 11044 not_open_order, 10010 already_closed, 11008 already_filled
const NOT_FOUND: [i64; 4] = [10004, 11044, 10010, 11008];

/// How far a label row may sit before the order's own stamp and still be read as that order, in
/// milliseconds. The stamp is this host's clock and the row's `creation_timestamp` the venue's, and a
/// drifted clock stops no deribit account (`crates/bridges/deribit/src/transport/client.rs`'s `PATH_TIME`), so
/// a strict comparison would turn a drift into "absent", a terminal reject of an order that stands. An
/// explicit client id reused across a restart lies further back than this.
const LABEL_ROW_CLOCK_SLACK_MS: i64 = 60_000;

/// Canonical venue id — the deribit `venue_time_in_force` row key (tif step-2).
const VENUE: &str = "deribit";

/// How long an empty label lookup waits before the one more ask that may license a terminal reject. When
/// the socket closes just after the frame and the re-dial is quick, the frame may still be on its way to
/// the venue. Nothing measured this lag: [`VenueRestDeribit::query_order_by_label`]'s evidence covers terminal
/// orders, not how soon a new one shows.
const LABEL_LAG_WAIT: Duration = Duration::from_secs(2);

/// **The most a submit may hold the exec thread**, from the top of `submit_order`, before it stops
/// re-dialing and asking. The order may first have waited `MAX_SUBMIT_WAIT` on the queue, and the two together
/// must end by the core's last-resort reject of an order with no answer (`vike_model::submit_ack_last_resort` at
/// the shipped `DEFAULT_SUBMIT_ACK_TIMEOUT` and `DEFAULT_SUBMIT_ACK_CONFIRM_GRACE`; the sweep is
/// `crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders`), past which an answer finds the order
/// already rejected and a live one rests untracked: 20 + 40 = 60. With every wait spent in full the path was 4 + 10 + 10 (a stale
/// socket's probe, dial and auth) + 10 (the reply) + 10 + 10 (the re-dial) + 10 (the lookup) = 64 s, and a
/// second ask alone would have made it 76 s. So every step after the frame runs only if it can end inside
/// this limit, and one that cannot is the order's unknown (nothing is said, the stuck-order ladder settles
/// it), never a reject: the same 64 s path now ends by 34 s, at the cost of its last steps. Not counted:
/// a re-auth inside the lookup's own `call`. A row read or a combo's registration ahead of the frame asks the order's
/// age again (`too_old_to_post`), and a close, which has no age, can answer later and folds by its fill.
const SUBMIT_HOLD_LIMIT: Duration = Duration::from_secs(40);
const _: () = assert!(
    vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT.as_secs() + SUBMIT_HOLD_LIMIT.as_secs()
        <= vike_model::submit_ack_last_resort(
            vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT,
            vike_model::DEFAULT_SUBMIT_ACK_CONFIRM_GRACE,
        )
        .as_secs()
);

/// Option instrument row — twin of `parse_deribit_option_instruments` (step = min qty =
/// min_trade_amount; no per-instrument max/notional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeribitInstrument {
    pub properties: SymbolProperties,
    pub contract_size: f64,
    pub base_asset: String,
}

/// Parse a `public/get_instruments` payload into the OPTION rows' order grids, keyed by instrument
/// name.
///
/// ⚠ **The `kind == "option"` gate below is LOAD-BEARING — do not widen it to "classify the other
/// kinds too".** The frozen `fixtures/r6/deribit.json` `instruments_payload` carries a
/// `BTC-PERPETUAL` `kind: "future"` row, and the frozen `instruments` map it is compared against
/// holds the two OPTION rows alone — `crates/bridges/deribit/tests/offline/r6_deribit_parity.rs`'s
/// `instruments_parity` asserts that membership (the message is `"future + non-option rows
/// skipped"`). Admitting a future changes what this function RETURNS, against oracle bytes that are
/// the reference by `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`.
///
/// **`DeribitInstrument::base_asset` is the row's own `base_currency`** whenever the row carries
/// one (every live `get_instruments` row does) — identity from the payload, never the name
/// (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`).
/// Only a row WITHOUT `base_currency` falls back to [`option_base_asset`]'s legacy
/// `BASE-DDMMMYY-STRIKE-C|P` name grammar, and that fallback is what still drops a row the grammar
/// rejects: the frozen r6 rows carry no `base_currency`, so their parity (the two options kept,
/// `"not-an-option"` skipped) is byte-identical. A USDC-settled option row (`BTC_USDC-…`), which
/// the grammar refused, is now admitted with its payload base (`BTC`); nothing under `src/` calls
/// this function, so no order path changes scale with it.
///
/// This function classifies options and nothing else BY DESIGN, and that is not where Deribit's
/// futures/perps are missed. They are classified in `crates/bridges/deribit/src/execution/mod.rs`'s
/// `parse_get_instrument` — the singular `public/get_instrument` grid, which is symbol-agnostic and
/// is what `crate::adapter`'s `VenueAdapterDeribit` and the PIT properties recorder actually call.
/// This function has no caller under `src/` at all: its live callers are this crate's smoke tests
/// (each of which asks the venue for `kind=option` in the REQUEST) plus the r6 parity test.
pub fn parse_deribit_option_instruments(payload: &Value) -> IndexMap<String, DeribitInstrument> {
    let mut out = IndexMap::new();
    for entry in payload.get("result").and_then(|r| r.as_array()).unwrap_or(&vec![]) {
        if entry.get("kind").and_then(|k| k.as_str()) != Some("option") {
            continue;
        }
        let name = entry.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let payload_base = entry
            .get("base_currency")
            .and_then(|b| b.as_str())
            .filter(|b| !b.is_empty())
            .map(str::to_uppercase);
        // The legacy name grammar answers ONLY for a row that publishes no base (the r6 rows).
        let Some(base) = payload_base.or_else(|| option_base_asset(&name)) else { continue };
        let f = |key: &str| -> f64 { entry.get(key).and_then(json_number).unwrap_or(0.0) };
        let step = f("min_trade_amount");
        out.insert(
            name,
            DeribitInstrument {
                properties: SymbolProperties {
                    tick_size: f("tick_size"),
                    step_size: step,
                    min_qty: step,
                    // Same value as the sibling `contract_size` field below, now ALSO carried on
                    // the grid so it travels with `SymbolProperties` wherever that goes (the PIT
                    // properties series, `Instrument`, the engine's multiplier grid). The struct
                    // field is retained: the r6 parity fixture pins it, and the option-chain path
                    // reads it directly.
                    contract_size: f("contract_size"),
                    // The class is the VENUE'S OWN `kind`, and this parser has ALREADY matched it
                    // exactly: the `continue` at the top of the loop admits only rows whose
                    // `kind` is the literal `"option"`, so every entry reaching here is one the
                    // venue itself called an option. Reading the same field a second time here
                    // could only ever disagree with the gate that let the row in.
                    // Deribit's other kinds (`future`, `future_combo`, `option_combo`, `spot`)
                    // never arrive here; `crates/bridges/deribit/src/catalog.rs`'s
                    // `asset_class_of` is the crate's ONE classifier over `kind` +
                    // `settlement_period`, and both of the venue's instrument payloads call it —
                    // the catalog list and `execution/mod.rs`'s singular `parse_get_instrument` grid.
                    // ⚠ NOT derived from the `BASE-DDMMMYY-STRIKE-C|P` instrument name, although
                    // `option_base_asset` parses exactly that shape a few lines below — a name
                    // test is the implicit encoding
                    // `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove.
                    asset_class: Some(AssetClass::Option),
                    // Deribit DOES publish `tick_size_steps`, but parsing it is deliberately NOT
                    // in this lane: the catalog row and the singular `public/get_instrument` read
                    // attach the scheme (`crates/bridges/deribit/src/catalog.rs`'s
                    // `properties_of`), this smoke/parity lane does not, though
                    // the vike-data `kind=properties` codec does carry a `tick_scheme` column now
                    // (`crates/vike-store/src/datafusion_hist/codec/properties.rs`'s `decode_tick_scheme`).
                    // Absent = the scalar `tick_size` IS the grid, exactly as before — and it
                    // stays absent through `..Default::default()`, which also covers
                    // max_qty/min_notional (no per-instrument caps) and `taker_hold_ms` (Deribit
                    // declares no venue hold). FRU rather than an exhaustive literal so a new
                    // `SymbolProperties` field costs this parser nothing.
                    ..Default::default()
                },
                contract_size: f("contract_size"),
                base_asset: base,
            },
        );
    }
    out
}

/// The read-side `parse_instrument_name` regex (`^([A-Z]+)-(\d{1,2}[A-Z]{3}\d{2})-(\d+)-([CP])$`)
/// reduced to the exec-side need: the base currency, or None for a non-option name. The FALLBACK
/// only — [`parse_deribit_option_instruments`] asks it solely for a row that publishes no
/// `base_currency` (the frozen r6 rows).
fn option_base_asset(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 4 {
        return None;
    }
    let (base, expiry, strike, cp) = (parts[0], parts[1], parts[2], parts[3]);
    let base_ok = !base.is_empty() && base.bytes().all(|b| b.is_ascii_uppercase());
    let expiry_ok = expiry.is_ascii()
        && expiry.len() >= 5
        && expiry.len() <= 7
        && expiry[..expiry.len() - 5].bytes().all(|b| b.is_ascii_digit())
        && expiry[expiry.len() - 5..expiry.len() - 2].bytes().all(|b| b.is_ascii_uppercase())
        && expiry[expiry.len() - 2..].bytes().all(|b| b.is_ascii_digit());
    let strike_ok = !strike.is_empty() && strike.bytes().all(|b| b.is_ascii_digit());
    let cp_ok = vike_marketdata::OptionKind::from_cp(cp).is_some();
    (base_ok && expiry_ok && strike_ok && cp_ok).then(|| base.to_string())
}

/// How long a refused foreign instrument is remembered, so a burst of orders on a symbol the venue
/// does not list (or of the other money model) costs ONE blocking read on the exec thread.
const FOREIGN_REFUSAL_TTL: Duration = Duration::from_secs(30);

/// **The [`MoneyModel`] of the row of an instrument an account may ADD to its engine** (decision 0233), read from
/// its `public/get_instrument` row (decision 0170): whether an order's amount is US dollars (`Inverse`) or the base
/// coin (`Linear`), and the coin the contract settles in, upper case (`BTC`, `ETH`, `USDC`: the one the label lookup
/// is scoped by). One engine holds ONE model: the ledger is one currency and an inverse engine runs on a configured
/// seed, a linear one on the wallet (`docs/ops/deribit-any-symbol-plan.md`), so the instruments a mounted client may
/// also sign are the ones of the same model ([`admits_in_words`]). A perpetual, a dated future, or a USDC-settled
/// (linear) option; or why the row is not one this build trades. A spot pair, a coin-settled option, a combo and a
/// quanto contract are each refused BY NAME (a guess at the model is the defect 0170 removed).
///
/// * A dated future counts money exactly as a perpetual of its margining does (an amount of US
///   dollars when it is `reversed`, of the base coin when it is `linear`). It adds an EXPIRY the
///   engine does not fold: the venue settles the position at its time and the node learns of it from
///   the next reconcile pass.
/// * A linear option is priced `qty · premium` in USDC, which the one-currency ledger books exactly
///   FOR A LONG (the premium is the whole risk, and the venue's own margin of a buy is that premium,
///   MEASURED: `private/get_margins` 8.6 USDC for 0.01 BTC at 860). A short's margin is not that
///   (122.26 for the same size), which is why the client sends an option sale on another instrument
///   than the mounted one reduce-only (see `VenueRestDeribit::long_only_sale`).
pub fn money_model_of_row(row: &Value) -> Result<MoneyModel, String> {
    use vike_model::Contract;
    let name = row.get("instrument_name").and_then(Value::as_str).unwrap_or("the instrument");
    let Some(contract) = crate::catalog::contract_of(row) else {
        return Err(format!(
            "{name}: the instrument row states no contract this build can value (a combo, or a \
             row without its margining or settlement currency)"
        ));
    };
    // a coin-settled option: the mount refuses it in the same words (`contract_value_of`)
    if let Some(why) = crate::catalog::coin_option_refusal(name, &contract) {
        return Err(why.to_string());
    }
    let settle = match &contract {
        Contract::Perp(spec) => &spec.settle,
        Contract::Future(spec) => &spec.settle,
        Contract::Option(spec) => &spec.settle,
        Contract::CryptoSpot => {
            return Err(format!(
                "{name} is a spot pair: Deribit reports no position for it (a holding is only a \
                 currency balance, the same wallet that is the collateral of the coin-margined \
                 contracts), so the node could not reconcile one"
            ));
        }
        _ => {
            return Err(format!(
                "{name} is not a perpetual, a dated future or a USDC option, the instruments an \
                 account may add"
            ));
        }
    };
    let Some(value) = vike_model::ContractValue::of(&contract, 1.0) else {
        return Err(format!(
            "{name} is a quanto contract, whose notional this build does not value"
        ));
    };
    Ok(MoneyModel { kind: value.kind, settle: settle.to_ascii_uppercase() })
}

/// The model of the row an engine is MOUNTED on: the same as [`money_model_of_row`] except that an engine
/// mounted on an option adds no other instrument (a mounted option may be sold short, a thing an added
/// one may not, so the two are never mixed in one engine).
pub fn money_model_of_mount_row(row: &Value) -> Result<MoneyModel, String> {
    let name = row.get("instrument_name").and_then(Value::as_str).unwrap_or("the instrument");
    if crate::catalog::asset_class_of(row) == Some(AssetClass::Option) {
        return Err(format!("{name} is an option; an engine mounted on one adds no instrument"));
    }
    money_model_of_row(row)
}

/// Whether an account mounted on `mounted`'s instrument may also trade `theirs`' (`name`), by the one rule
/// (`vike_model::MoneyModel::admits`): the same kind and the same settlement currency (one wallet), in this
/// bridge's words. An inverse engine keeps its books in its settle coin (decision 0258,
/// `crates/vike-execution/src/account.rs`'s `Account::set_coin_books`), and the venue keeps one wallet and one
/// margin per coin, so an inverse contract of ANOTHER coin (`ETH-PERPETUAL` beside `BTC-PERPETUAL`) is refused like
/// a linear one of another settlement.
pub(crate) fn admits_in_words(
    mounted: &MoneyModel,
    theirs: &MoneyModel,
    name: &str,
) -> Result<(), String> {
    use vike_model::NotionalKind::{Inverse, Linear};
    let word = |k| match k {
        Inverse => "INVERSE (its amount counts US dollars)",
        Linear => "LINEAR (its amount counts the base coin)",
    };
    mounted.admits(theirs).map_err(|refusal| match refusal {
        MoneyModelRefusal::Kind => format!(
            "{name} is {} but this account is mounted on a {} instrument, and one account \
             trades one money model; use an account mounted on a {} instrument",
            word(theirs.kind),
            word(mounted.kind),
            word(theirs.kind),
        ),
        MoneyModelRefusal::SettleCoin => {
            let what = if mounted.kind == Linear { "linear" } else { "inverse" };
            format!(
                "{name} settles in {} but this account's {what} instrument settles in {}: one wallet, \
                 one settlement currency; use an account mounted on a {} instrument",
                theirs.settle, mounted.settle, theirs.settle
            )
        }
    })
}

/// One instrument an order is signed on: the venue's own name for it, its order grid, and the
/// currency its label lookup is scoped by.
#[derive(Debug, Clone)]
pub struct InstrumentContext {
    /// The instrument name as the venue spells it.
    pub symbol: String,
    /// Tick, step and minimums from the row.
    pub properties: SymbolProperties,
    /// The settlement currency (`private/get_order_state_by_label` REQUIRES it, and answers `[]` with
    /// no error for a wrong one).
    pub currency: String,
}

/// Reads a foreign instrument's `public/get_instrument` `result` row (blocking, exec thread only),
/// or says why it cannot.
pub type InstrumentRows = Box<dyn Fn(&str) -> Result<Value, String> + Send + Sync>;

/// One foreign instrument's standing: resolved for the life of the client, or refused for a while.
enum Foreign {
    Ready(InstrumentContext),
    Refused(Instant, String),
}

/// The context of `name` from its row, or the reason this client does not sign on it. Pure.
fn context_from_row(
    name: &str,
    row: &Value,
    mounted: &MoneyModel,
) -> Result<InstrumentContext, String> {
    let listed = row.get("instrument_name").and_then(Value::as_str).unwrap_or("");
    if !listed.eq_ignore_ascii_case(name) {
        return Err(format!("the venue answered the row of `{listed}` when `{name}` was asked"));
    }
    let theirs = money_model_of_row(row)?;
    admits_in_words(mounted, &theirs, listed)?;
    let properties = crate::catalog::properties_of(row)
        .filter(|p| p.tick_size > 0.0 && p.step_size > 0.0)
        .ok_or_else(|| {
            format!("{listed}: the instrument row publishes no tick size or no minimum amount")
        })?;
    Ok(InstrumentContext { symbol: listed.to_string(), properties, currency: theirs.settle })
}

pub struct VenueRestDeribit {
    /// the persistent authed order WS (Mutex: VenueRest takes &self; JSON-RPC calls
    /// mutate the socket)
    pub transport: Mutex<DeribitTransport>,
    /// The mounted instrument's VENUE name (`BTC-PERPETUAL`): every request of it signs this.
    pub symbol: String,
    /// The mounted instrument's USER name (decision 0290: `BTCUSD.P`), its kept row's: the name an order on it
    /// carries. The venue name itself where this process keeps no row of it (a test that holds no list).
    user: String,
    pub properties: SymbolProperties,
    /// The MOUNTED instrument's settlement currency (the row's `settlement_currency`; the token before
    /// the first `-` only where the row could not be read).
    pub currency: String,
    /// order client id <-> venue order_id (cancel is by order_id): the shared registry, of which only the order client id -> order_id
    /// direction is read here.
    order_ids: Mutex<OrderVenueIdRegistry<String>>,
    /// order client id -> the stamp of the order this client last sent under it: what a later label lookup
    /// compares a row's creation with. Grows as `order_ids` does.
    order_stamps: Mutex<HashMap<String, i64>>,
    /// The mounted instrument's money model, when its row was read: `None` refuses every other
    /// instrument (an unknown model is never defaulted).
    model: Option<MoneyModel>,
    /// Reads a foreign instrument's row; `None` = this client signs on the mounted instrument only.
    rows: Option<InstrumentRows>,
    /// Foreign instruments by upper-case name.
    foreign: Mutex<HashMap<String, Foreign>>,
    /// [`SUBMIT_HOLD_LIMIT`], unless a test shortened it.
    submit_hold_limit: Duration,
    /// The wall clock the delivery guard judges an order at ([`Self::delivery_refusal`]), unless a test set another.
    now: fn() -> i64,
}

/// The limit price as it goes on the wire: snapped to the tick IN FORCE at `px` (half-to-even
/// `round_price` on a tiered grid) and formatted to that SAME tier tick. A price above a tier
/// boundary formatted on the scalar BASE grid is what the venue rejects — the order-entry landmine
/// the tiered grid closes. With NO scheme (futures/perps, and any option the venue reports a flat
/// grid for) it is LITERALLY `format_to_step_f(px, tick_size)`, so the wire is byte-identical
/// (pinned in `tick_scheme_order_format_tests`). Shared by a submit and a modify, so the two cannot
/// snap one price two ways. `properties` is the grid of the instrument the order is signed on: the
/// mounted one's only for the mounted symbol.
fn wire_price_on(properties: &SymbolProperties, px: f64) -> f64 {
    let price_str = match &properties.tick_scheme {
        Some(scheme) => format_to_step_f(scheme.round_price(px), scheme.tick_at(px)),
        None => format_to_step_f(px, properties.tick_size),
    };
    price_str.parse::<f64>().unwrap_or(0.0)
}

/// A lookup's answer that says nothing about the order: its fate stays unknown, never "absent".
fn no_answer(method: &str, what: &str) -> VenueApiError {
    VenueApiError {
        code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
        msg: format!("{method}: {what}"),
    }
}

/// A JSON-RPC error object as the error it carries. A `code` that is missing or not an integer is the
/// [`E_TIMEOUT_AMBIGUOUS`](vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS) sentinel, never `0`, the definite
/// NOT SENT one: the venue answered, so the frame left, and an answer naming no code says nothing about the write.
fn venue_error(err: &Value) -> VenueApiError {
    VenueApiError {
        code: err
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or(vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS),
        msg: err.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

/// A failed `private/cancel` as the cancel's error. One the submit's rule leaves open (the transport's sentinel:
/// a lost reply or an order frame's `13009`; the venue's `13888`, server trouble and an unmapped code: `TAXONOMY`'s
/// `submit_outcome_unknown`) may have cancelled the order, so it carries the sentinel (the shared
/// `vike_bridge_core::rest::cancel_rejected` words the outcome as unknown) and keeps the venue's words, instead of
/// reading as a refused cancel; any other keeps the venue's code and words.
fn cancel_failed(exc: CallError) -> VenueApiError {
    let exc = VenueApiError::from(exc);
    if !crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(exc.code) {
        return exc;
    }
    VenueApiError { code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS, msg: exc.msg }
}

/// The rows of a lookup's answer: only a list is one.
fn rows_of<'a>(method: &str, answer: &'a Value) -> Result<&'a [Value], VenueApiError> {
    let rows = answer.as_array().ok_or_else(|| no_answer(method, "the answer is not a list"))?;
    Ok(rows.as_slice())
}

/// The venue order id of the NEWEST of `rows` not created before `not_before_ms` (`None` takes every
/// row): a client id reused inside the allowance, or by an order whose stamp nobody here holds, still
/// names the latest order. A row stating no `creation_timestamp` cannot be told apart, so it is read as
/// the order, and rows stating none keep the venue's order; a row naming no order is no answer.
fn order_id_among<'a>(
    method: &str,
    rows: impl DoubleEndedIterator<Item = &'a Value>,
    not_before_ms: Option<i64>,
) -> Result<Option<String>, VenueApiError> {
    fn created(row: &Value) -> Option<i64> {
        row.get("creation_timestamp").and_then(Value::as_i64)
    }
    // `rev`: of equals `max_by_key` keeps the last, so this is the first the venue listed.
    let row = rows
        .filter(|row| created(row).zip(not_before_ms).is_none_or(|(at, floor)| at >= floor))
        .rev()
        .max_by_key(|row| created(row));
    let Some(row) = row else {
        return Ok(None);
    };
    row.get("order_id")
        .filter(|id| !id.is_null())
        .map(json_text)
        .filter(|id| !id.is_empty())
        .map(Some)
        .ok_or_else(|| no_answer(method, "a row names no order"))
}

/// The end an order object of the venue states, when it ended without a fill: the only truth to tell about a
/// modify that found it so (a post-only edit that would cross can end the order, `cancel_reason`
/// `edit_post_only_reject`). `None` for an order that still rests or filled: a fill is `user.trades`'s.
/// A refusal may come as ONE state word, `rejected:<reason>` (`crate::execution::event_mapper::is_rejected`).
/// Stamped as an amend's answer is (`amend_answer_time`): the order's `last_update_timestamp`, else now.
fn ended_by_venue(order_client_id: &str, order: &Value) -> Option<Event> {
    let state = order.get("order_state").and_then(Value::as_str)?;
    let word = match state {
        "cancelled" => "cancelled",
        s if crate::execution::event_mapper::is_rejected(s) => "rejected",
        _ => return None,
    };
    let why = order
        .get("cancel_reason")
        .and_then(Value::as_str)
        .filter(|why| !why.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::execution::event_mapper::reject_reason(order));
    let why = if why.is_empty() { "no reason given" } else { why.as_str() };
    let crossed = crate::execution::event_mapper::is_post_only_cross(order);
    Some(Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.to_string(),
        reason: format!("deribit {word} the order: {why}").into(),
        timestamp: amend_answer_time(order.get("last_update_timestamp").and_then(Value::as_i64)),
        post_only_would_cross: crossed,
        // The venue's refusal of an order it had taken: a `rejected` state, or a post-only cross.
        refusal_id: vike_model::events::refusal_id_of(
            vike_model::OrderRefusalId::VenueRefused.word(),
        )
        .filter(|_| word == "rejected" || crossed),
    }))
}

/// The `private/edit` params on the grid `properties` (`VenueRestDeribit::build_edit_params` is the mounted
/// instrument's).
fn edit_params_on(
    properties: &SymbolProperties,
    order_id: &str,
    projected: &OrderRequest,
) -> Value {
    let amount =
        format_to_step_f(projected.qty, properties.step_size).parse::<f64>().unwrap_or(0.0);
    // A post-only order stays one across an edit, and refuses to cross, as its submit said.
    let maker_only = projected.time_in_force == TimeInForce::PostOnly;
    let mut params = json!({
        "order_id": order_id,
        "amount": amount,
        "post_only": maker_only,
    });
    if maker_only {
        params["reject_post_only"] = json!(true);
    }
    // A limit price rides a limit and a stop_limit; a stop_market has none (its price is its trigger).
    if matches!(projected.order_type, OrderType::Limit | OrderType::StopLimit)
        && let Some(px) = projected.price
    {
        params["price"] = json!(wire_price_on(properties, px));
    }
    // An iceberg's visible slice is restated on every edit, as the trigger is below: what the venue does
    // with an edit that leaves it out is unmeasured, and it must never come back showing the whole size.
    if projected.order_type == OrderType::Limit
        && let Some(shown) = projected.display_quantity
    {
        params["display_amount"] =
            json!(format_to_step_f(shown, properties.step_size).parse::<f64>().unwrap_or(0.0));
    }
    // `private/edit` takes `trigger_price` for an untriggered stop (measured on the testnet), on the same
    // tick grid as a limit price; it is sent whether or not this edit moves it, because the venue wants the
    // trigger on every edit of a trigger order.
    if projected.order_type.is_trigger()
        && let Some(tp) = projected.trigger_price
    {
        params["trigger_price"] = json!(wire_price_on(properties, tp));
    }
    if projected.reduce_only {
        params["reduce_only"] = json!(true);
    }
    params
}

/// The Deribit wire `type` of a single-instrument `request`: the six kinds this platform names, each
/// to its own native order. A take-profit with a `price` is a `take_limit`, as hyperliquid's trigger
/// path reads a trigger kind (market-or-limit by whether `price` is set); a stop with a `price` stays
/// `stop_market` (only `stop_limit` is a stop with a limit). A market-to-limit is `market_limit`, which
/// the venue's changelog (2021-05-14) states is the same law: the remainder of a partial fill rests as
/// a limit at the price the filled part executed at. No limit price rides it (none is written: the
/// shared `order_terms_refusal` refuses one before an order gets here). A `trailing` is `trailing_stop`,
/// its trail riding `trigger_offset` (the venue's "positive deviation from the price peak at which the
/// order triggers", in price; `private/buy`'s parameters, read 2026-11-08): the venue's trailing stop
/// takes no activation price, which [`trigger_refusal`] refuses by name. `None` for any other kind (an
/// `Unrecognized` word, a `trailing_limit`: the venue has no trailing stop whose fill is a limit, or a
/// kind added after this table): [`kind_refusal`] refuses it before the wire. It used to go out as a
/// `market` order.
fn wire_type(request: &OrderRequest) -> Option<&'static str> {
    match request.order_type {
        OrderType::Market => Some("market"),
        OrderType::Limit => Some("limit"),
        OrderType::Stop => Some("stop_market"),
        OrderType::StopLimit => Some("stop_limit"),
        OrderType::TakeProfit if request.price.is_some() => Some("take_limit"),
        OrderType::TakeProfit => Some("take_market"),
        OrderType::MarketToLimit => Some("market_limit"),
        OrderType::Trailing => Some("trailing_stop"),
        _ => None,
    }
}

/// A trailing stop's `trigger_offset`: its trail on the instrument's flat tick grid (floored, as a
/// trigger price is: `wire_price_on`), `0.0` for a trail under one tick ([`trigger_refusal`] refuses it).
fn trail_on_grid(properties: &SymbolProperties, trail: f64) -> f64 {
    format_to_step_f(trail, properties.tick_size).parse::<f64>().unwrap_or(0.0)
}

/// The settlement currency a deribit instrument NAME states: the token before the first `-`, or the part after
/// its `_` for a USDC or USDT instrument (`BTC-PERPETUAL` and `BTC-27MAR26-60000-C` settle in BTC,
/// `ETH_USDC-PERPETUAL` in USDC). The combo fence's one rule, which needs no row read.
pub(crate) fn settle_of_name(name: &str) -> &str {
    let head = name.split('-').next().unwrap_or(name);
    head.split_once('_').map_or(head, |(_, settle)| settle)
}

/// **A combo `request` with every leg named as the venue names it** (decision 0290: a strategy names a leg by its USER
/// name, `crate::names::venue_name_of`), `None` for a single order (no leg to respell).
///
/// # Errors
/// A leg no kept row names, in words naming it.
fn venue_named_legs(request: &OrderRequest) -> Result<Option<OrderRequest>, String> {
    if request.combo_legs.is_empty() {
        return Ok(None);
    }
    let mut named = request.clone();
    for leg in &mut named.combo_legs {
        leg.symbol = crate::names::venue_name_of(&leg.symbol).map_err(|why| why.to_string())?;
    }
    Ok(Some(named))
}

/// Why a COMBO cannot go out from an account whose wallet is `currency` (decision 0258: one engine keeps one
/// settle coin, and the venue keeps one wallet per coin): a leg whose name settles in another currency, which
/// includes a combo whose legs settle in different coins. A terminal `OrderRejected` with this text, before
/// `private/create_combo` registers anything. `None` for a single order and a combo of the account's own coin.
/// Only the COIN half of `vike_model::MoneyModel::admits` (`vike_model::same_currency`): a combo's legs may mix
/// kinds in one coin's wallet (an inverse future beside an option), so its kind half would refuse a combo this
/// venue takes.
fn combo_settle_refusal(currency: &str, request: &OrderRequest) -> Option<String> {
    let leg = request
        .combo_legs
        .iter()
        .find(|leg| !vike_model::same_currency(settle_of_name(&leg.symbol), currency))?;
    Some(format!(
        "deribit: the combo leg `{}` settles in {} but this account's instrument settles in {currency}: one \
         wallet, one settlement currency, so every leg of a combo settles in it; refused, nothing sent",
        leg.symbol,
        settle_of_name(&leg.symbol).to_ascii_uppercase()
    ))
}

/// Why `request`'s KIND cannot go out from this bridge (a terminal `OrderRejected` with this text, before any
/// read, frame or `private/create_combo`): a single order whose kind [`wire_type`] has no wire type for; a
/// combo that is neither a market nor a limit; a limit combo that names no net price; and a combo that
/// asks to show only part of itself (`display_qty`: the combo params carry no `display_amount`).
/// `VenueRestDeribit::resolve_combo_order` builds a market combo and a priced limit combo only, and sent each of
/// the other two as a combo MARKET order. `None` for every order the bridge builds.
fn kind_refusal(request: &OrderRequest) -> Option<String> {
    let kind = &request.order_type;
    if request.combo_legs.is_empty() {
        return wire_type(request).is_none().then(|| {
            format!("deribit builds no `{kind}` order; refused, never sent as a market order")
        });
    }
    if request.display_quantity.is_some() {
        return Some(
            "deribit shows no part of a combo order (no `display_amount` on a combo); refused, \
             never sent at its full size"
                .to_string(),
        );
    }
    match kind {
        OrderType::Market => None,
        OrderType::Limit if request.price.is_some() => None,
        OrderType::Limit => Some(
            "deribit: a `limit` combo order names no net price; refused, never sent as a combo \
             market order"
                .to_string(),
        ),
        _ => Some(format!(
            "deribit builds no `{kind}` combo order; refused, never sent as a market order"
        )),
    }
}

/// Why a TRIGGER order cannot go out as it stands, before any frame is written (a terminal
/// `OrderRejected` with this text). The venue would refuse each of these itself, but its message
/// does not name the cause as plainly, and for an option the refusal is the venue's own rule:
/// trigger orders exist for futures and perpetuals only. A `trailing` states no trigger price: the
/// venue's `trailing_stop` has no activation, so one that states it (vike's activation rides
/// `trigger_price`) is refused by name, and so is a trail under one tick. `None` for a plain order.
/// Beside the text, its id: `VenueCapability` for what the venue has no order for, `NotSent` for a term.
fn trigger_refusal(
    request: &OrderRequest,
    properties: &SymbolProperties,
) -> Option<(String, OrderRefusalId)> {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    if !request.order_type.is_trigger() {
        return None;
    }
    let kind = request.order_type.as_str();
    let positive = |p: Option<f64>| p.is_some_and(|p| p.is_finite() && p > 0.0);
    if request.order_type == OrderType::Trailing {
        if let Some(at) = request.trigger_price {
            let why = format!(
                "deribit's trailing_stop has no activation price; this trailing order states one \
                 ({at}): refused, nothing sent"
            );
            return Some((why, VenueCapability));
        }
        match request.trail {
            Some(t) if positive(Some(t)) && trail_on_grid(properties, t) > 0.0 => {}
            Some(t) if positive(Some(t)) => {
                let why = format!(
                    "deribit: the trail {t} is under the instrument's tick {}; refused, nothing sent",
                    properties.tick_size
                );
                return Some((why, NotSent));
            }
            _ => return Some(("deribit trailing needs a trail above zero".to_string(), NotSent)),
        }
    } else if !positive(request.trigger_price) {
        return Some((format!("deribit {kind} needs a positive trigger price"), NotSent));
    }
    if request.order_type == OrderType::StopLimit && !positive(request.price) {
        return Some(("deribit stop_limit needs a positive limit price".to_string(), NotSent));
    }
    if properties.asset_class == Some(AssetClass::Option) {
        let why = format!(
            "deribit takes no {kind} order on an option (trigger orders exist for futures and \
             perpetuals only)"
        );
        return Some((why, VenueCapability));
    }
    None
}

/// Why an order's `display_qty` (an iceberg's visible slice, the venue's `display_amount`) cannot go out
/// as it stands, before any frame is written (a terminal `OrderRejected` with this text): it rides a
/// plain LIMIT only, it must be a positive number, and the venue has no iceberg on an OPTION (the venue
/// matrix: iceberg and trigger orders do not work on options and combos). Never sent at the order's full
/// visible size, never dropped. The venue's own size rules (a display of at least 100 times the
/// instrument's minimum, a hidden part under 100 times the visible one) stay the venue's to refuse, in
/// its words. `None` for an order with no `display_qty`. Its id as [`trigger_refusal`]'s.
fn display_refusal(
    request: &OrderRequest,
    properties: &SymbolProperties,
) -> Option<(String, OrderRefusalId)> {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    let shown = request.display_quantity?;
    if request.order_type != OrderType::Limit {
        let why = format!(
            "deribit shows part of a limit order only (`display_amount`); a {} order with a \
             display quantity is refused, never sent at its full size",
            request.order_type
        );
        return Some((why, VenueCapability));
    }
    if !(shown.is_finite() && shown > 0.0) {
        return Some((format!("deribit needs a positive display quantity, not {shown}"), NotSent));
    }
    if properties.asset_class == Some(AssetClass::Option) {
        return Some((
            "deribit takes no iceberg (`display_amount`) on an option; refused, never sent at its \
             full size"
                .to_string(),
            VenueCapability,
        ));
    }
    None
}

/// The venue's code for a POST-ONLY order it refused because it would have taken liquidity
/// (`post_only_reject`, answered when `reject_post_only` is set; MEASURED on the testnet by
/// `tests/deribit_post_only_smoke.rs`).
const POST_ONLY_REJECT: i64 = 11054;

/// The typed cause of a refused submit: [`RejectCause::PostOnlyWouldCross`] for the venue's own
/// `post_only_reject` code on an order that asked to be post-only, else `None`.
fn post_only_cause(request: &OrderRequest, code: i64) -> Option<vike_model::events::RejectCause> {
    (request.time_in_force == TimeInForce::PostOnly && code == POST_ONLY_REJECT)
        .then_some(vike_model::events::RejectCause::PostOnlyWouldCross)
}

/// [`VenueRestDeribit::build_order_params`] for the instrument `symbol` on the grid `properties`.
fn order_params_on(symbol: &str, properties: &SymbolProperties, request: &OrderRequest) -> Value {
    let wire = wire_type(request);
    let is_limit = request.order_type == OrderType::Limit;
    let amount = format_to_step_f(request.qty, properties.step_size).parse::<f64>().unwrap_or(0.0);
    let mut params = json!({
        "instrument_name": symbol,
        "amount": amount,
        // `null` for a kind `kind_refusal` refuses: `submit_order` never sends one
        "type": wire,
        "label": request.order_client_id,
        // Deribit defaults post_only=true; a crossing/marketable order MUST set false
        "post_only": false,
    });
    if request.order_type.is_trigger() {
        // The trigger and a limit leg ride the same tick grid as a limit price. `trigger` is
        // REQUIRED by the venue on these types; a request that names no series gets `last_price`
        // (`venue_trigger_by`'s default law).
        if let Some(px) = request.trigger_price {
            params["trigger_price"] = json!(wire_price_on(properties, px));
        }
        let source = request.trigger_by.unwrap_or(TriggerBy::Last);
        params["trigger"] = json!(venue_trigger_by(VENUE, source).wire().unwrap_or("last_price"));
        if matches!(wire, Some("stop_limit" | "take_limit"))
            && let Some(px) = request.price
        {
            params["price"] = json!(wire_price_on(properties, px));
        }
        if wire == Some("trailing_stop")
            && let Some(trail) = request.trail
        {
            params["trigger_offset"] = json!(trail_on_grid(properties, trail));
        }
    }
    if is_limit {
        if let Some(px) = request.price {
            // Tiered grid (Deribit options carry a `tick_scheme`): snap the limit to the tick
            // IN FORCE at this price (half-to-even `round_price`) and format to that SAME tier
            // tick. A price above a tier boundary formatted on the scalar BASE grid is what the
            // venue rejects — the order-entry landmine this closes. With NO scheme (futures/
            // perps, and any option the venue reports a flat grid for) the `None` arm is
            // LITERALLY today's expression `format_to_step_f(px, tick_size)`, so the wire is
            // byte-identical (pinned in `tick_scheme_order_format_tests` below).
            params["price"] = json!(wire_price_on(properties, px));
        }
        // An iceberg: the venue shows `display_amount` of it, on the amount's own lot grid. Absent
        // unless asked, so every other limit's frame is byte-identical (`display_refusal` judged it).
        if let Some(shown) = request.display_quantity {
            params["display_amount"] =
                json!(format_to_step_f(shown, properties.step_size).parse::<f64>().unwrap_or(0.0));
        }
        if request.time_in_force == TimeInForce::PostOnly {
            // POST-ONLY is not a `time_in_force` here but the flag pair: `post_only` alone makes the
            // venue silently RE-PRICE a crossing order to the best bid/ask (the trap this file's
            // doc names), `reject_post_only` makes it REFUSE one — the answer a maker-only order
            // is owed. `time_in_force` stays the venue default (`good_til_cancelled`).
            params["post_only"] = json!(true);
            params["reject_post_only"] = json!(true);
        } else {
            match vike_venues::venue_time_in_force::venue_time_in_force(
                VENUE,
                request.time_in_force,
            ) {
                vike_venues::venue_time_in_force::TimeInForceOutcome::Mapped(w)
                | vike_venues::venue_time_in_force::TimeInForceOutcome::Coerced {
                    wire: w, ..
                } => {
                    params["time_in_force"] = json!(w);
                }
                vike_venues::venue_time_in_force::TimeInForceOutcome::Ignored { .. }
                | vike_venues::venue_time_in_force::TimeInForceOutcome::NotEmitted
                | vike_venues::venue_time_in_force::TimeInForceOutcome::Unsupported => {}
            }
        }
    }
    if request.reduce_only {
        params["reduce_only"] = json!(true);
    }
    params
}

impl VenueRestDeribit {
    pub fn new(
        transport: DeribitTransport,
        symbol: &str,
        properties: SymbolProperties,
        currency: &str,
    ) -> Self {
        VenueRestDeribit {
            transport: Mutex::new(transport),
            symbol: symbol.to_string(),
            user: vike_catalog::kept_user_name_of_raw("deribit", symbol)
                .unwrap_or_else(|| symbol.to_string()),
            properties,
            currency: currency.to_string(),
            order_ids: Mutex::default(),
            order_stamps: Mutex::new(HashMap::new()),
            model: None,
            rows: None,
            foreign: Mutex::new(HashMap::new()),
            submit_hold_limit: SUBMIT_HOLD_LIMIT,
            now: vike_model::now_ms,
        }
    }

    /// Override `SUBMIT_HOLD_LIMIT` (the offline tests reach the end of the budget without waiting for
    /// it).
    #[must_use]
    pub fn with_submit_hold_limit(mut self, limit: Duration) -> Self {
        self.submit_hold_limit = limit;
        self
    }

    /// Override the clock the delivery guard reads (an offline test signs on a captured dated row whose day has
    /// passed on the wall clock).
    #[must_use]
    pub fn with_clock(mut self, now: fn() -> i64) -> Self {
        self.now = now;
        self
    }

    /// **The delivery guard** (decision 0233 as amended on 2026-11-22: okx's rule for every venue). `None` when
    /// `request` may be sent, else why not: an OPENING order on a dated future or option is refused inside its last
    /// 24 h and every order once it is delivered, judged by `vike_venues::venue_delivery_guard::delivery_verdict` from
    /// the order's USER name (the mounted one's when the order names none). A sale this client sends reduce-only
    /// ([`Self::long_only_sale`]) can only close, so it is judged as one. A combo is not guarded: its legs name no
    /// single day.
    fn delivery_refusal(
        &self,
        request: &OrderRequest,
        properties: &SymbolProperties,
    ) -> Option<String> {
        let name =
            if request.symbol.is_empty() { self.user.as_str() } else { request.symbol.as_str() };
        let reduce_only = request.reduce_only || self.long_only_sale(request, properties);
        match delivery_verdict(VENUE, name, (self.now)(), reduce_only) {
            DeliveryGuardVerdict::Taken => None,
            DeliveryGuardVerdict::OpeningInsideWindow { left_ms } => Some(format!(
                "deribit: {name} is delivered in {} h and takes no OPENING order inside its last 24 h: the venue \
                 settles a position at the delivery and the engine books no settlement, so it would hold a position \
                 the venue closed. A reduce-only order is still taken; trade a later expiry",
                left_ms / vike_model::MS_PER_HOUR
            )),
            DeliveryGuardVerdict::Delivered => Some(format!(
                "deribit: {name} was delivered at its expiry (08:00 UTC of the day in its name) and no longer \
                 trades; the order was not sent"
            )),
        }
    }

    /// Whether a step that can take `needs` still ends inside the submit's hold.
    fn hold_left(&self, started: Instant, needs: Duration) -> bool {
        started.elapsed() + needs <= self.submit_hold_limit
    }

    /// **Let this client sign on instruments other than the mounted one** (any-symbol): `model` is the
    /// mounted instrument's, `rows` reads another instrument's `public/get_instrument` row on first
    /// use. An order on an instrument of the SAME money model is then signed on that instrument, on
    /// its own grid; every other order is refused with the reason and nothing reaches the wire.
    ///
    /// shortcut: a resolved instrument's grid is kept for the life of the client, so a venue tick change
    /// is seen after a restart while the engine's own copy is refreshed hourly; upgrade to a timed
    /// re-read if the venue ever retunes a live perpetual's tick.
    #[must_use]
    pub fn with_foreign_instruments(mut self, model: MoneyModel, rows: InstrumentRows) -> Self {
        self.model = Some(model);
        self.rows = Some(rows);
        self
    }

    /// Whether an order's USER name is the mounted instrument's (or names none, as the engine's adapters read it).
    fn is_mounted(&self, symbol: &str) -> bool {
        symbol.is_empty() || symbol.eq_ignore_ascii_case(&self.user)
    }

    /// Whether `request` is a SALE of an OPTION that is not the mounted instrument: such a sale goes out
    /// reduce-only (decision 0233), so an added option is only ever LONG. A long option's premium is its
    /// whole risk and the ledger books it exactly; a short's margin is not the premium (MEASURED on the
    /// testnet: 122.26 USDC against 8.6 for the same 0.01 BTC call), which the engine does not model. The
    /// venue enforces it: a reduce-only sale with no long is refused (code 11030
    /// `invalid_reduce_only_order`) and one beyond the position is cut to it. The mounted option, which
    /// may be sold short today, keeps its behaviour.
    fn long_only_sale(&self, request: &OrderRequest, properties: &SymbolProperties) -> bool {
        request.side < 0
            && properties.asset_class == Some(AssetClass::Option)
            && !self.is_mounted(&request.symbol)
    }

    /// The instrument, grid and currency an order on `symbol` is signed with: the mounted client's own
    /// for the mounted symbol (or an empty one, as the engine's adapters read it), else the foreign
    /// instrument resolved on first use (a blocking read) and kept.
    fn context_for(&self, symbol: &str) -> Result<InstrumentContext, String> {
        if self.is_mounted(symbol) {
            return Ok(InstrumentContext {
                symbol: self.symbol.clone(),
                properties: self.properties,
                currency: self.currency.clone(),
            });
        }
        let (Some(model), Some(rows)) = (&self.model, &self.rows) else {
            return Err(format!(
                "this deribit client signs on its mounted instrument `{}` only (it cannot sign on \
                 `{symbol}`: no foreign-instrument source, or its own row could not be read)",
                self.symbol
            ));
        };
        // the order's USER name is signed as its kept row's venue name (decision 0290); no row is a refusal
        let key = crate::names::venue_name_of(symbol).map_err(|why| why.to_string())?;
        match lock(&self.foreign).get(&key) {
            Some(Foreign::Ready(ctx)) => return Ok(ctx.clone()),
            Some(Foreign::Refused(at, why)) if at.elapsed() < FOREIGN_REFUSAL_TTL => {
                return Err(why.clone());
            }
            _ => {}
        }
        // The read runs with no lock held.
        let built =
            rows(&key).map_err(|why| format!("cannot sign on `{symbol}`: {why}")).and_then(|row| {
                context_from_row(&key, &row, model)
                    .map_err(|why| format!("cannot sign on `{symbol}`: {why}"))
            });
        let mut held = lock(&self.foreign);
        match built {
            Ok(ctx) => {
                held.insert(key, Foreign::Ready(ctx.clone()));
                Ok(ctx)
            }
            Err(why) => {
                held.insert(key, Foreign::Refused(Instant::now(), why.clone()));
                Err(why)
            }
        }
    }

    /// The settlement currency of an order's instrument when this client has it WITHOUT a read (the
    /// mounted one, or a foreign one already resolved): the audit-T1 re-query uses it and never guesses. `symbol` is
    /// an order's USER name, or a combo's first leg already respelled to the venue's name (`venue_named_legs`).
    fn known_currency(&self, symbol: &str) -> Option<String> {
        if self.is_mounted(symbol) || symbol == self.symbol {
            return Some(self.currency.clone());
        }
        // only a name this client resolved is in the map, so a name it holds as the venue's is looked up as such
        let key = crate::names::venue_name_of(symbol).unwrap_or_else(|_| symbol.to_string());
        match lock(&self.foreign).get(&key) {
            Some(Foreign::Ready(ctx)) => Some(ctx.currency.clone()),
            _ => None,
        }
    }

    /// Every settlement currency this client has signed an order in, the mounted one first.
    fn known_currencies(&self) -> Vec<String> {
        let mut out = vec![self.currency.clone()];
        for foreign in lock(&self.foreign).values() {
            if let Foreign::Ready(ctx) = foreign
                && !out.contains(&ctx.currency)
            {
                out.push(ctx.currency.clone());
            }
        }
        out
    }

    /// The `private/edit` params for a modify of the resting `order` to `projected` (the resting
    /// terms with the new quantity and price folded in). Pure, so the wire shape is fixture-pinned.
    ///
    /// * `amount` is the new TOTAL size — `private/edit` REQUIRES it, so a price-only modify sends
    ///   the resting size — formatted to the instrument's step like a submit's.
    /// * `price` is sent for a limit order, through `wire_price_on`.
    /// * `post_only` is FORCED FALSE, as on submit: Deribit defaults it to true, and a post-only edit
    ///   that would cross is silently re-priced just below the spread instead of filling, so an
    ///   operator's marketable modify would rest at a price nobody asked for.
    /// * `reduce_only` is carried when the order is reduce-only, so an edit never widens it.
    ///
    /// The MOUNTED instrument's grid, for the fixture tests; `edit_order` builds on the grid of the
    /// order's own instrument.
    #[cfg(test)]
    pub(crate) fn build_edit_params(&self, order_id: &str, projected: &OrderRequest) -> Value {
        edit_params_on(&self.properties, order_id, projected)
    }

    /// Pure, golden-gated. amount/price are NUMBERS (Python float(format_qty(...))).
    ///
    /// tif step-2 (FLIPPED): the limit path consumes deribit's `venue_time_in_force` row — Gtc stays
    /// NotEmitted (no `time_in_force` param, the venue's server default `good_til_cancelled`
    /// rules; byte-identical to pre-flip), Ioc/Fok/Day map to Deribit's own TIF vocabulary
    /// (`immediate_or_cancel`/`fill_or_kill`/`good_til_day`). Gtd is Unsupported and gated at
    /// submit (`deny_unsupported_time_in_force`) — if one reaches this builder anyway, no param is
    /// emitted (venue default), the gate being the loud guard. Market orders keep today's
    /// no-TIF shape (the table's resting-path scope).
    pub fn build_order_params(&self, request: &OrderRequest) -> Value {
        order_params_on(&self.symbol, &self.properties, request)
    }

    /// Audit A3 resync: recent order history (`private/get_order_history_by_instrument`) for the
    /// post-reconnect replay — a BARE array. `include_unfilled` is required so cancelled/rejected
    /// orders that never traded (the non-fill terminals A3 recovers) are not omitted.
    pub fn get_order_history(&self, count: u32) -> Result<Value, CallError> {
        self.private_result(
            "private/get_order_history_by_instrument",
            &json!({
                "instrument_name": self.symbol,
                "count": count,
                "include_old": true,
                "include_unfilled": true,
            }),
        )
    }

    /// Audit A3 resync: recent user trades (`private/get_user_trades_by_instrument`) for the replay.
    /// The result is `{trades, has_more}`; this returns the unwrapped `trades` array.
    pub fn get_user_trades(&self, count: u32) -> Result<Value, CallError> {
        let result = self.private_result(
            "private/get_user_trades_by_instrument",
            &json!({ "instrument_name": self.symbol, "count": count, "sorting": "asc" }),
        )?;
        Ok(result.get("trades").cloned().unwrap_or(json!([])))
    }

    /// Audit T1: re-query one order by its `label` (= our idempotent order_client_id) over the WS.
    /// `Ok(Some(id))` = at the venue, `Ok(None)` = the venue lists nothing under the label right now
    /// (empty result), `Err` = the query itself failed. `private/get_order_state_by_label` returns an
    /// array of orders.
    ///
    /// ⚠ **`Ok(None)` feeds a terminal reject, so what this endpoint REPORTS is load-bearing:
    /// if it answered with OPEN orders only, an order that filled instantly would re-query empty
    /// and be rejected while the fill stood.** It does not. MEASURED on Deribit testnet
    /// 2026-08-25, both terminal shapes, account left flat:
    ///
    /// * a rested limit, then cancelled → `get_order_state_by_label` → `"order_state":"cancelled"`
    /// * a market order, then filled → `get_order_state_by_label` → `"order_state":"filled"`
    ///
    /// So an empty array does not mean the order merely ended. Neither measurement says how soon a NEW
    /// order shows, and a frame may still be on its way when a quick re-dial asks, which is why one empty
    /// answer does not reject ([`Self::ask_by_label_twice`]). Deribit changing the endpoint reopens the
    /// rest: re-measure, do not re-argue.
    ///
    /// ⚠ **`currency` is the SETTLEMENT currency of the ORDER'S instrument, and a wrong one is
    /// silent.** MEASURED on testnet 2026-10-17: the endpoint requires a currency, refuses `any` and a
    /// non-currency with -32602, and answers `[]` with NO error for a valid currency that is not the
    /// order's own. So an empty answer licenses the terminal reject above only when the caller asked
    /// in the currency the order settles in ([`Self::known_currency`]); anything else is inconclusive.
    ///
    /// ⚠ **Only a list is an answer** ([`rows_of`], [`order_id_among`]): anything else says nothing about
    /// the order, the `E_TIMEOUT_AMBIGUOUS` error, never "absent". A row created before `not_before_ms`
    /// is not the order asked about: an explicit client id reused after a restart must not be accepted
    /// under the old order's dead id.
    fn query_order_by_label(
        &self,
        order_client_id: &str,
        currency: &str,
        not_before_ms: Option<i64>,
    ) -> Result<Option<String>, CallError> {
        const METHOD: &str = "private/get_order_state_by_label";
        let result = self
            .private_result(METHOD, &json!({ "currency": currency, "label": order_client_id }))?;
        let rows = rows_of(METHOD, &result).map_err(CallError::Failed)?;
        order_id_among(METHOD, rows.iter(), not_before_ms).map_err(CallError::Failed)
    }

    /// `pub(crate)` (not private): `reconcile/mod.rs`'s `ReconcileClientDeribit` reuses this SAME
    /// signed JSON-RPC entry point — over its OWN `VenueRestDeribit`, and therefore its own
    /// `Mutex<DeribitTransport>`, so a reconcile read never takes the exec side's order
    /// transport (`ReconcileClientDeribit::connect`'s doc carries that argument; this comment claimed
    /// the opposite — a shared socket — from before that deviation landed).
    ///
    /// ⚠ That separation is what lets `ReconcileClientDeribit::call` RE-DIAL a dead socket on a
    /// transport failure. Nothing on the exec path may do the same blindly: a re-sent
    /// `private/buy` is a second order.
    pub(crate) fn private_result(&self, method: &str, params: &Value) -> Result<Value, CallError> {
        let response = self.transport.lock().unwrap().call(method, params)?;
        let (_rid, result, error) = parse_response(&response);
        if let Some(err) = error.filter(|e| !e.is_null()) {
            // The venue ANSWERED: its socket is alive.
            return Err(CallError::Failed(venue_error(&err)));
        }
        // ⚠ A reply stating neither is NO answer, and a real empty one is `[]` or an object. It used to read as
        // `null`, which the label lookup took for "the venue confirms absent" (the licence for a terminal
        // reject) and a cancel for "cancelled". The frame is on the wire, so the outcome is unknown: ambiguous.
        result.filter(|r| !r.is_null()).ok_or_else(|| {
            CallError::Failed(VenueApiError {
                code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                msg: format!("{method}: the venue's reply stated neither a result nor an error"),
            })
        })
    }

    pub fn detach(&self) {
        self.transport.lock().unwrap().close();
    }

    /// Record this instrument's parsed properties grid into the PIT properties store (opt-in). Best-effort;
    /// the recorder no-ops when disabled and swallows store errors. Call after `new`.
    pub fn with_properties_recorder(
        self,
        rec: &std::sync::Arc<vike_data::PropertiesRecorder>,
    ) -> Self {
        rec.record("deribit", &self.symbol, self.properties, now_ns());
        self
    }
}

use vike_model::now_ns;

/// The pauses before each re-read of a combo book born `inactive` (`VenueRestDeribit::wait_combo_active`): five,
/// 300 ms each.
const COMBO_ACTIVE_PAUSES: [std::time::Duration; 5] = [std::time::Duration::from_millis(300); 5];

impl VenueRestDeribit {
    /// Resolve a combo `OrderRequest` (non-empty `combo_legs`) to its venue-side order params.
    ///
    /// Two venue calls, per the combo-book contract (see [`crate::execution::combo`]): register the leg
    /// structure with `private/create_combo` (which returns the CANONICAL combo — possibly the
    /// inverse of, or a ratio-reduced form of, what we asked for), then reconcile that against the
    /// spec to get the orientation/scale mapping. Any failure to reconcile is an `Err(reason)` the
    /// caller turns into a terminal `OrderRejected` — we never guess a direction or a net price.
    ///
    /// The combo's OWN tick/step grid is fetched from `public/get_instrument` on the combo id (the
    /// legs' grids do not apply to the combo book). A PARTIAL parse — either axis missing or zero
    /// — counts as a failed fetch ([`parse_combo_grid`] requires BOTH axes positive), never a
    /// silently-unquantized axis; on any failure we fall back to this client's configured leg
    /// grid (logged), which at worst leaves the value unquantized (`format_to_step`'s zero-step
    /// passthrough) rather than blocking the order.
    fn resolve_combo_order(&self, request: &OrderRequest) -> Result<(&'static str, Value), String> {
        let create = self
            .private_result("private/create_combo", &build_create_combo_params(&request.combo_legs))
            .map_err(|e| format!("create_combo failed: {} ({})", e.error().msg, e.error().code))?;
        let combo = parse_combo(&create).ok_or_else(|| ComboMapError::Unparseable.to_string())?;
        let combo = self.wait_combo_active(combo)?;
        let mapping = map_combo(&request.combo_legs, &combo.legs).map_err(|e| e.to_string())?;

        // The combo book's own grid; a failed OR PARTIAL fetch (either axis missing/zero) falls
        // back to this client's configured leg grid — loudly, never a half-used grid.
        let grid = self
            .private_result("public/get_instrument", &json!({ "instrument_name": combo.id }))
            .ok()
            .and_then(|r| parse_combo_grid(&r))
            .unwrap_or_else(|| {
                tracing::warn!(
                    target: "vike_deribit::execution::combo",
                    combo_id = %combo.id,
                    "combo grid fetch failed or partial — falling back to the configured leg grid"
                );
                (self.properties.tick_size, self.properties.step_size)
            });

        // A limit combo carries the SIGNED net; `price: None` (or a non-limit type) is a combo
        // MARKET order. The sign is never inspected — a credit combo's negative net rides through.
        let net = (request.order_type == OrderType::Limit).then_some(request.price).flatten();
        let order = build_combo_order(&combo.id, mapping, request.side, request.qty, net);
        let method = if order.side > 0 { "private/buy" } else { "private/sell" };
        tracing::info!(
            target: "vike_deribit::execution::combo",
            combo_id = %order.instrument_name,
            order_client_id = %request.order_client_id,
            numerator = mapping.numerator,
            den = mapping.den,
            side = order.side,
            "resolved combo instrument"
        );
        Ok((method, build_combo_order_params(&order, &request.order_client_id, grid.0, grid.1)))
    }

    /// A BRAND-NEW combo book is born `"inactive"` and flips `"active"` moments later — observed
    /// live on testnet (`state_timestamp` ~574 ms after `creation_timestamp`; the gate-flip smoke
    /// hit exactly this race). Rejecting on the first read would terminally reject the FIRST
    /// order on every newly registered combo structure, so poll `public/get_combo_details`
    /// briefly (bounded: 5 × 300 ms on the per-ORDER submit path, which is already synchronous
    /// venue I/O) before giving up with the [`ComboMapError::Inactive`] reject. An already-active
    /// combo (the idempotent-create common case) returns immediately with zero extra calls. The
    /// ladder is `vike_utility::retry::retry_on_pauses` over [`COMBO_ACTIVE_PAUSES`].
    fn wait_combo_active(
        &self,
        combo: crate::execution::combo::VenueCombo,
    ) -> Result<crate::execution::combo::VenueCombo, String> {
        if combo.is_active() {
            return Ok(combo);
        }
        let read = |attempt: u32| {
            // the first ask is the create's own answer, read inactive above: each read waits its pause first
            if attempt == 1 {
                return None;
            }
            let details = self
                .private_result("public/get_combo_details", &json!({"combo_id": combo.id}))
                .ok()?;
            parse_combo(&details).filter(crate::execution::combo::VenueCombo::is_active)
        };
        let found = vike_utility::retry::retry_on_pauses(
            &COMBO_ACTIVE_PAUSES,
            read,
            Option::is_some,
            std::thread::sleep,
        );
        match found {
            Some(re) => {
                tracing::info!(
                    target: "vike_deribit::execution::combo",
                    combo_id = %re.id,
                    "fresh combo book activated"
                );
                Ok(re)
            }
            None => Err(ComboMapError::Inactive(combo.id).to_string()),
        }
    }

    /// The shared submit tail: fire `method`/`params` at the order socket and fold the JSON-RPC
    /// reply into at most ONE terminal-or-accepted event: none when the order's fate stays unknown, and
    /// then the core's stuck-order ladder ends it (an order never silently vanishes on either path).
    /// Single-leg and combo submits share this so the emitter split, the order-id bookkeeping and the
    /// audit-T1 ambiguous-timeout recovery are identical on both paths. `started` is the top of
    /// `submit_order`, which the [`SUBMIT_HOLD_LIMIT`] counts from.
    fn dispatch_submit(
        &self,
        request: &OrderRequest,
        method: &str,
        params: &Value,
        started: Instant,
    ) -> Option<Event> {
        // ⚠ The `let` is LOAD-BEARING, not style. A `match self.transport.lock().unwrap().call(…)`
        // keeps the `MutexGuard` — a temporary in the scrutinee — alive until the END of the match,
        // so any arm that touches `self.transport` again re-locks a mutex this thread already
        // holds: `std::sync::Mutex` is not reentrant, and on Linux that is a hard self-deadlock of
        // the exec thread. The audit-T1 arm below has ALWAYS re-entered (`query_order_by_label` →
        // `private_result` → `self.transport.lock()`); it was reachable only on a WS response
        // timeout, which is why nothing had hit it. Widening the ambiguous door to every read-half
        // failure would have made that path ordinary. Binding to a `let` drops the guard at the
        // semicolon.
        lock(&self.order_stamps).insert(request.order_client_id.clone(), request.timestamp);
        let outcome = self.transport.lock().unwrap().call(method, params);
        match outcome {
            Ok(response) => {
                let (_rid, result, error) = parse_response(&response);
                if let Some(err) = error.filter(|e| !e.is_null()) {
                    let VenueApiError { code, msg } = venue_error(&err);
                    if crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(code) {
                        tracing::warn!(
                            target: "vike_deribit::execution::rest",
                            order_client_id = %request.order_client_id,
                            code,
                            msg = %msg,
                            "the venue's error leaves the order's fate open; asking by label (the order is NEVER re-sent)"
                        );
                        return self.resolve_ambiguous_submit(request, started);
                    }
                    tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, code, msg = %msg, "submit rejected");
                    return Some(Event::OrderRejected(OrderRejected {
                        cause: post_only_cause(request, code),
                        order_client_id: request.order_client_id.clone(),
                        reason: msg.into(),
                        timestamp: request.timestamp,
                        refusal_id: vike_model::events::refusal_id_of(
                            vike_model::OrderRefusalId::VenueRefused.word(),
                        ),
                    }));
                }
                let order_id = result
                    .as_ref()
                    .and_then(|r| r.get("order"))
                    .and_then(|o| o.get("order_id"))
                    .map(json_text)
                    .unwrap_or_default();
                // ⚠ A reply naming no order says neither placed nor refused: the frame is on the wire, so it
                // takes the lost answer's path (never re-sent), and no order is called placed without an id.
                if order_id.is_empty() {
                    tracing::warn!(
                        target: "vike_deribit::execution::rest",
                        order_client_id = %request.order_client_id,
                        "the submit's reply named no order; asking the venue by label (the order is NEVER re-sent)"
                    );
                    return self.resolve_ambiguous_submit(request, started);
                }
                self.order_ids.lock().unwrap().bind(&request.order_client_id, order_id.clone());
                Some(Event::OrderAccepted(OrderAccepted {
                    order_client_id: request.order_client_id.clone(),
                    venue_order_id: Some(order_id.into()),
                    timestamp: request.timestamp,
                }))
            }
            Err(exc) => {
                // ⚠⚠ ORDER SAFETY. The socket may be unusable, and this transport never re-dials
                // itself — so heal it HERE, before the audit-T1 re-query below needs a live socket
                // to ask over (a re-query on the dead socket just fails and we are back to
                // guessing). **A re-dial is NOT a re-send**: `connect()` closes, dials and
                // authenticates, and the order frame is never written a second time. Deribit does
                // not dedupe on `label`, so a resent `private/buy` is a SECOND REAL ORDER; nothing
                // on this path may resend, whatever the socket did. With no room left for the dial it
                // is skipped: the next order's own idle probe replaces the socket before its frame.
                if exc.is_dead_socket() && self.hold_left(started, redial_bound()) {
                    self.redial_order_socket(&request.order_client_id, exc.error());
                }
                let exc = VenueApiError::from(exc);
                if crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(exc.code) {
                    // audit T1: the request frame was already on the wire, so Deribit may have
                    // accepted it — re-query by label instead of emitting a false terminal reject
                    // (which would strand a phantom). Since 2026-08-25 this covers EVERY read-half
                    // failure, not only the response timeout.
                    self.resolve_ambiguous_submit(request, started)
                } else {
                    // Definite: the send never reached the wire, so no order exists to strand.
                    tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, code = exc.code, msg = %exc.msg, "submit rejected");
                    Some(Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: request.order_client_id.clone(),
                        // before `reason`, which moves `exc.msg`
                        refusal_id: vike_model::events::refusal_id_of(exc.refusal_id_word()),
                        reason: exc.msg.into(),
                        timestamp: request.timestamp,
                    }))
                }
            }
        }
    }

    /// Re-open the order socket after a transport failure. Best-effort and LOUD: a failed re-dial
    /// leaves the re-query to fail too, which [`vike_bridge_core::resolve_ambiguous_submit`] already
    /// answers with nothing (never a terminal): the core's stuck-order ladder decides.
    ///
    /// ⚠ It re-dials and NOTHING ELSE — see [`Self::dispatch_submit`] for why an order is never
    /// re-sent. Logged per event rather than latched behind a "log once" flag the way
    /// `reconcile/mod.rs` does: this fires per ORDER, at an order boundary, which is exactly the
    /// granularity the workspace's logging rule asks to instrument.
    fn redial_order_socket(&self, order_client_id: &str, cause: &VenueApiError) {
        tracing::warn!(
            target: "vike_deribit::execution::rest",
            order_client_id,
            code = cause.code,
            msg = %cause.msg,
            "order-WS unusable — re-dialing so the status re-query has a live socket (the order is NEVER re-sent)"
        );
        if let Err(e) = self.transport.lock().unwrap().connect() {
            tracing::error!(
                target: "vike_deribit::execution::rest",
                order_client_id,
                error = %e,
                "order-WS re-dial failed; the submit resolves on whatever evidence is available"
            );
        }
    }

    /// The audit-T1 tail: re-query the order by its `label` and map the answer through the SHARED
    /// [`vike_bridge_core::resolve_ambiguous_submit`] (consumed verbatim — order landed → adopt, anything
    /// else → nothing, never a false terminal, and the core's stuck-order ladder decides; this venue has
    /// no active confirm, so that is the ladder's last-resort reject at 60 to 75 s unless the stream or
    /// a replay speaks first). An empty answer is asked twice ([`Self::ask_by_label_twice`]): the second
    /// ask may still adopt an order whose frame was on its way.
    ///
    /// The one thing done BESIDE that mapping: an adopted order's venue id is recorded in
    /// `order_ids`. Without it every cancel of the order would pay a label lookup first
    /// ([`Self::find_order_id_by_label`]) -- correct, but a round trip the adopting client already
    /// holds the answer to, and this path fires on every read-half failure.
    ///
    /// The re-query is asked in the currency the ORDER'S instrument settles in, a combo's being its first
    /// leg's ([`Self::known_currency`]); one this client cannot name makes the query inconclusive
    /// (nothing, never a reject), because a wrong currency answers empty without an error.
    fn resolve_ambiguous_submit(&self, request: &OrderRequest, started: Instant) -> Option<Event> {
        let order_client_id = &request.order_client_id;
        // A combo's `symbol` is empty at submit, which `known_currency` reads as the MOUNTED instrument:
        // the first leg names the currency instead, since a wrong one answers `[]` with no error.
        let instrument =
            request.combo_legs.first().map_or(request.symbol.as_str(), |leg| leg.symbol.as_str());
        let query = match self.known_currency(instrument) {
            Some(currency) => self.ask_by_label_twice(request, &currency, started),
            None => Err(VenueApiError {
                code: 0,
                msg: format!(
                    "no settlement currency is known for `{instrument}`, so its label cannot be \
                     looked up"
                ),
            }),
        };
        match &query {
            Ok(Some(order_id)) => {
                self.order_ids.lock().unwrap().bind(order_client_id, order_id.clone());
            }
            Ok(None) => {}
            Err(why) => tracing::warn!(
                target: "vike_deribit::execution::rest",
                %order_client_id,
                reason = %why.msg,
                "the order's fate stays unknown: nothing is said, the stuck-order ladder decides"
            ),
        }
        vike_bridge_core::resolve_ambiguous_submit(order_client_id, request.timestamp, query)
    }

    /// The lookup behind an ambiguous submit. An empty answer is asked once more after
    /// [`LABEL_LAG_WAIT`], on the exec thread, and only a second empty answer is `Ok(None)`, which says
    /// nothing either. An ask that cannot end inside the submit's hold ([`SUBMIT_HOLD_LIMIT`]) is not
    /// made, and that is the inconclusive `Err`. A row older than
    /// this order, past [`LABEL_ROW_CLOCK_SLACK_MS`], is not this order.
    fn ask_by_label_twice(
        &self,
        request: &OrderRequest,
        currency: &str,
        started: Instant,
    ) -> Result<Option<String>, VenueApiError> {
        let order_client_id = &request.order_client_id;
        let not_before = Some(request.timestamp.saturating_sub(LABEL_ROW_CLOCK_SLACK_MS));
        let no_room = || VenueApiError {
            code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
            msg: format!(
                "the submit has held the order thread for {:?}, with no room left to ask by label",
                started.elapsed()
            ),
        };
        if !self.hold_left(started, reply_bound()) {
            return Err(no_room());
        }
        if let Some(order_id) = self.query_order_by_label(order_client_id, currency, not_before)? {
            return Ok(Some(order_id));
        }
        if !self.hold_left(started, LABEL_LAG_WAIT + reply_bound()) {
            return Err(no_room());
        }
        std::thread::sleep(LABEL_LAG_WAIT);
        self.query_order_by_label(order_client_id, currency, not_before)
            .map_err(VenueApiError::from)
    }
}

impl VenueRestDeribit {
    /// **Modify a resting limit order in place with `private/edit`**, answering EVERY outcome with an
    /// event (never `[]`, the default no-op's silence): `OrderModified` when the venue took the edit
    /// (or, after an ambiguous transport failure, when the order is observed live at the requested
    /// terms), `OrderCanceled` when the venue's answer or the order read back says the order ended,
    /// `OrderModifyRejected` (non-terminal; the order keeps its terms) otherwise.
    ///
    /// * **A crossing edit of a post-only order ends it, in a SUCCESS reply** whose order says `rejected`
    ///   and `edit_post_only_reject` (measured on testnet 2026-11-06, `tests/deribit_post_only_smoke.rs`).
    ///   A refused one still reads the order back, because no event but this one would tell the engine for
    ///   a limit until a reconcile pass: only that `cancel_reason` makes a limit's `user.orders` row an
    ///   event (`crate::execution::event_mapper`'s `map_deribit_order_row`).
    ///
    /// * Only a single-leg LIMIT, STOP or STOP_LIMIT order is editable here: a market order does not
    ///   rest, a take-profit's trigger is not what the modify seam's one price names, and a combo is a
    ///   venue-listed instrument with its own book, so those are rejected with the reason. A STOP's
    ///   TRIGGER moves natively (`private/edit` takes `trigger_price`), which is what lets the amend
    ///   table (`vike_venues::venue_amend_mode`) call deribit's stop `Native`.
    /// * The venue order id is the one the submit recorded in `order_ids`, else the one the venue
    ///   lists under our `label` (the order client id) — the cancel's lookup, shared
    ///   ([`Self::find_order_id_by_label`]).
    /// * `private/edit` keeps the venue `order_id` and the label, so the order client id map and every later
    ///   cancel stay valid, and the user-data stream's order frames for the edited order carry the
    ///   same order client id.
    /// * **An edit is never re-sent after a transport failure** (the submit rule, for the same
    ///   reason: the frame may already have landed). A dead socket is re-dialed so the NEXT command
    ///   has one, and an ambiguous failure is SETTLED by asking the venue what the order is now
    ///   ([`Self::settle_edit`]) — the answer the error code alone cannot give. So is a venue error the
    ///   submit's rule leaves open (`TAXONOMY`'s `submit_outcome_unknown`: `13888`, the server's trouble, an
    ///   unmapped code).
    fn edit_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let order_client_id = order.order_client_id.clone();
        let reject = |reason: String, family: OrderRefusalId| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order_client_id.clone(),
                reason: reason.into(),
                timestamp: amend_answer_time(None),
                refusal_id: vike_model::events::refusal_id_of(family.word()),
            })]
        };
        if !order.combo_legs.is_empty() {
            return reject(
                "deribit modify does not edit a combo order (cancel and re-submit instead)"
                    .to_string(),
                OrderRefusalId::VenueCapability,
            );
        }
        if !matches!(order.order_type, OrderType::Limit | OrderType::Stop | OrderType::StopLimit) {
            return reject(
                "deribit modify edits a resting limit, stop or stop_limit order only (a take-profit \
                 or a market order is cancelled and re-submitted instead)"
                    .to_string(),
                OrderRefusalId::VenueCapability,
            );
        }
        // The modify seam carries ONE price: the TRIGGER of a stop and the limit price of every other
        // kind (`vike_model::modified_price_is_trigger`), the field the order's FSM moves too.
        let mut projected = vike_model::modified_order(order, new_qty, new_price, None);
        let positive = |p: Option<f64>| p.is_some_and(|p| p.is_finite() && p > 0.0);
        let terms_ok = match projected.order_type {
            OrderType::Stop => positive(projected.trigger_price),
            OrderType::StopLimit => positive(projected.trigger_price) && positive(projected.price),
            _ => positive(projected.price),
        };
        if !(projected.qty.is_finite() && projected.qty > 0.0) || !terms_ok {
            return reject(
                "deribit modify needs a positive finite quantity and price (a stop's trigger)"
                    .to_string(),
                OrderRefusalId::NotSent,
            );
        }
        // The edit is formatted on the grid of the ORDER'S instrument (the mounted one's for the
        // mounted symbol), the grid it was submitted on.
        let ctx = match self.context_for(&order.symbol) {
            Ok(ctx) => ctx,
            Err(why) => return reject(why, OrderRefusalId::NotSent),
        };
        let known = self.order_ids.lock().unwrap().venue_order_id_of(&order_client_id).cloned();
        let order_id = match known {
            Some(id) => id,
            None => match self.find_order_id_by_label(&order_client_id) {
                Ok(Some(id)) => id,
                // the order no longer rests: nothing of it is left to edit
                Ok(None) => {
                    return reject(
                        "the venue holds no order under this order client id, so there is nothing \
                         to edit"
                            .to_string(),
                        OrderRefusalId::AmendRefused,
                    );
                }
                // a failed READ before the edit: no edit frame was written
                Err(exc) => {
                    return reject(
                        format!(
                            "deribit modify could not look the order up by its label: {}",
                            exc.msg
                        ),
                        OrderRefusalId::NotSent,
                    );
                }
            },
        };
        // an added option's sale was sent reduce-only; an edit that dropped the flag would widen it
        projected.reduce_only |= self.long_only_sale(order, &ctx.properties);
        let params = edit_params_on(&ctx.properties, &order_id, &projected);
        // ⚠ Bound to a `let` before the match: a `MutexGuard` temporary in a scrutinee lives to the
        // end of the match and `settle_edit` below re-locks the same non-reentrant mutex.
        let outcome = self.transport.lock().unwrap().call("private/edit", &params);
        match outcome {
            Ok(response) => {
                let (_rid, result, error) = parse_response(&response);
                if let Some(err) = error.filter(|e| !e.is_null()) {
                    let VenueApiError { code, msg } = venue_error(&err);
                    // The submit's rule: a code that leaves the write open says nothing about the edit, so the
                    // order is read back, as for a lost reply.
                    if crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(code) {
                        tracing::warn!(
                            target: "vike_deribit::execution::rest",
                            %order_client_id,
                            code,
                            msg = %msg,
                            "the venue's error leaves the edit's fate open; reading the order back (the edit is NEVER re-sent)"
                        );
                        return self.settle_edit(
                            &order_client_id,
                            &order_id,
                            &projected,
                            &ctx.properties,
                            new_qty,
                            new_price,
                        );
                    }
                    tracing::warn!(target: "vike_deribit::execution::rest", code, msg = %msg, "edit rejected");
                    // the venue said no to the edit, whatever the read-back finds of the order
                    let venue_refused = |reason| reject(reason, OrderRefusalId::VenueRefused);
                    let refused = if msg.is_empty() {
                        format!("deribit refused the edit (code {code})")
                    } else {
                        msg
                    };
                    if projected.time_in_force != TimeInForce::PostOnly {
                        return venue_refused(refused);
                    }
                    let state = self.private_result(
                        "private/get_order_state",
                        &json!({ "order_id": order_id }),
                    );
                    return match state {
                        Ok(state) => ended_by_venue(&order_client_id, &state)
                            .map_or_else(|| venue_refused(refused), |ended| vec![ended]),
                        Err(exc) => venue_refused(format!(
                            "{refused}; the order could not be re-read ({}), so it is UNKNOWN \
                             whether it still rests (the next reconcile pass settles it)",
                            exc.error().msg
                        )),
                    };
                }
                // ⚠ A reply naming no order says neither edited nor refused: settled like a lost reply, never a success.
                let Some(answered) = result.as_ref().and_then(|r| r.get("order")) else {
                    tracing::warn!(
                        target: "vike_deribit::execution::rest",
                        %order_client_id,
                        "the edit's reply named no order; reading the order back (the edit is NEVER re-sent)"
                    );
                    return self.settle_edit(
                        &order_client_id,
                        &order_id,
                        &projected,
                        &ctx.properties,
                        new_qty,
                        new_price,
                    );
                };
                if let Some(ended) = ended_by_venue(&order_client_id, answered) {
                    return vec![ended];
                }
                let venue_order_id = answered
                    .get("order_id")
                    .map(json_text)
                    .filter(|id| !id.is_empty())
                    .unwrap_or(order_id);
                vec![Event::OrderModified(OrderModified {
                    order_client_id,
                    venue_order_id: Some(venue_order_id.into()),
                    new_qty,
                    new_price,
                    new_trigger_price: None,
                    // the edited order's own stamp of the edit
                    timestamp: amend_answer_time(
                        answered.get("last_update_timestamp").and_then(|t| t.as_i64()),
                    ),
                })]
            }
            Err(exc) => {
                if exc.is_dead_socket() {
                    self.redial_order_socket(&order_client_id, exc.error());
                }
                let exc = VenueApiError::from(exc);
                if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS {
                    self.settle_edit(
                        &order_client_id,
                        &order_id,
                        &projected,
                        &ctx.properties,
                        new_qty,
                        new_price,
                    )
                } else {
                    // Definite: the frame never reached the wire, the order is untouched.
                    tracing::warn!(target: "vike_deribit::execution::rest", code = exc.code, msg = %exc.msg, "edit not sent");
                    vec![modify_rejected(&order_client_id, &exc, amend_answer_time(None))]
                }
            }
        }
    }

    /// After an AMBIGUOUS edit (the frame was on the wire; the reply never came, named no order, or was a
    /// venue error that leaves the write open), ask the venue what
    /// the order is NOW and report that, because the transport error cannot say whether the edit
    /// landed: live at the requested terms → `OrderModified`; stated `cancelled` or `rejected` →
    /// `OrderCanceled`; anything else (live at other terms, filled, or the query itself failing) →
    /// `OrderModifyRejected` naming the doubt. Never a guess in either direction: the
    /// reconcile/gap-sentinel path re-converges the resting terms.
    fn settle_edit(
        &self,
        order_client_id: &str,
        order_id: &str,
        projected: &OrderRequest,
        properties: &SymbolProperties,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let reject = |reason: String, family: OrderRefusalId| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: amend_answer_time(None),
                refusal_id: vike_model::events::refusal_id_of(family.word()),
            })]
        };
        let state =
            self.private_result("private/get_order_state", &json!({ "order_id": order_id }));
        let Ok(state) = state else {
            return reject(
                "deribit modify: the reply was lost and the order could not be re-read, so it is \
                 UNKNOWN whether the edit landed (the next reconcile pass settles it)"
                    .to_string(),
                OrderRefusalId::OutcomeUnknown,
            );
        };
        if let Some(ended) = ended_by_venue(order_client_id, &state) {
            return vec![ended];
        }
        // A resting limit is `open`; a stop that has not fired is `untriggered`.
        let open = matches!(
            state.get("order_state").and_then(|v| v.as_str()),
            Some("open" | "untriggered")
        );
        let amount = state.get("amount").and_then(json_number);
        let want_amount = format_to_step_f(projected.qty, properties.step_size).parse::<f64>().ok();
        // The terms the edit moves: the limit price of a limit and a stop_limit, the trigger of a stop.
        let price_ok = !matches!(projected.order_type, OrderType::Limit | OrderType::StopLimit)
            || state.get("price").and_then(json_number)
                == projected.price.map(|p| wire_price_on(properties, p));
        let trigger_ok = !projected.order_type.is_trigger()
            || state.get("trigger_price").and_then(json_number)
                == projected.trigger_price.map(|p| wire_price_on(properties, p));
        if open && price_ok && trigger_ok && amount == want_amount {
            return vec![Event::OrderModified(OrderModified {
                order_client_id: order_client_id.to_string(),
                venue_order_id: Some(order_id.into()),
                new_qty,
                new_price,
                new_trigger_price: None,
                // the order read back states when it last changed: the edit that landed
                timestamp: amend_answer_time(
                    state.get("last_update_timestamp").and_then(|t| t.as_i64()),
                ),
            })];
        }
        // the order read back says the edit did not land: the amend could not complete
        reject(
            "deribit modify: the reply was lost and the order is not at the requested terms (it is \
             untouched or filled), so the edit did not take effect"
                .to_string(),
            OrderRefusalId::AmendRefused,
        )
    }
}

impl VenueRest for VenueRestDeribit {
    /// Modify a resting limit or stop order in place (`private/edit`): [`VenueRestDeribit::edit_order`]. EVERY
    /// outcome is an event, `OrderModified` or a non-terminal `OrderModifyRejected` (the trait's
    /// default no-op answered nothing at all, so the control door accepted a modify and the venue
    /// never heard of it).
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.edit_order(order, new_qty, new_price)
    }

    /// `[OrderSubmitted, OrderAccepted|OrderRejected]` — the emitter split: Rust emits
    /// `OrderSubmitted` synchronously and the venue's reply decides the second event. With no reply and a
    /// lookup that finds nothing there is no second event (`Self::dispatch_submit`): the stream's fill or the core's
    /// backstop speaks.
    ///
    /// A request with non-empty `combo_legs` routes through the combo book
    /// ([`Self::resolve_combo_order`]) instead of the single-leg `build_order_params`; a combo that
    /// cannot be registered or reconciled yields a terminal `OrderRejected` carrying the reason.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let started = Instant::now();
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        })];
        // tif step-2 gate: a TIF deribit cannot express (Gtd — no good-till-DATE, only
        // good_til_day) is a LOUD terminal reject; the wire is never touched, never a silent
        // default (pinned in `time_in_force_table_tests` below). Applies to combo limits too — the combo
        // params carry no TIF either.
        if let Some(reject) =
            vike_venues::venue_time_in_force::deny_unsupported_time_in_force(VENUE, request)
        {
            events.push(reject);
            return events;
        }
        // A kind this bridge builds no order for is refused by name here, before an instrument read or a
        // combo registration: it used to go out as a MARKET order. A combo's legs are USER names (decision 0290) and
        // the venue registers its own: each is respelled from its kept row (a leg no row names is refused), and a
        // combo with a leg of another settlement currency than the account's is refused (decision 0258).
        let mut named = None;
        let refusal = match kind_refusal(request) {
            // shortcut: a limit combo with no price anywhere is tagged so too; split kind_refusal if one arrives
            Some(reason) => Some((reason, OrderRefusalId::VenueCapability)),
            None => match venue_named_legs(request) {
                Ok(respelled) => {
                    named = respelled;
                    combo_settle_refusal(&self.currency, named.as_ref().unwrap_or(request))
                        .map(|reason| (reason, OrderRefusalId::NotSent))
                }
                Err(reason) => Some((reason, OrderRefusalId::NotSent)),
            },
        };
        let request = named.as_ref().unwrap_or(request);
        if let Some((reason, family)) = refusal {
            tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, %reason, "order kind refused before the wire");
            events.push(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: reason.into(),
                timestamp: request.timestamp,
                refusal_id: vike_model::events::refusal_id_of(family.word()),
            }));
            return events;
        }
        let (method, params) = if request.combo_legs.is_empty() {
            let method = if request.side > 0 { "private/buy" } else { "private/sell" };
            // The order is signed on ITS instrument, on that instrument's own grid: the mounted
            // client's for the mounted symbol, a foreign instrument of the same money model
            // otherwise. Anything else is refused here, before a frame is written.
            match self.context_for(&request.symbol) {
                Ok(ctx) => {
                    if let Some((reason, family)) = trigger_refusal(request, &ctx.properties)
                        .or_else(|| display_refusal(request, &ctx.properties))
                    {
                        tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, %reason, "trigger or iceberg order refused before the wire");
                        events.push(Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: request.order_client_id.clone(),
                            reason: reason.into(),
                            timestamp: request.timestamp,
                            refusal_id: vike_model::events::refusal_id_of(family.word()),
                        }));
                        return events;
                    }
                    if let Some(reason) = self.delivery_refusal(request, &ctx.properties) {
                        tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, %reason, "order refused by the delivery guard before the wire");
                        events.push(Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: request.order_client_id.clone(),
                            reason: reason.into(),
                            timestamp: request.timestamp,
                            refusal_id: vike_model::events::refusal_id_of(
                                OrderRefusalId::NotSent.word(),
                            ),
                        }));
                        return events;
                    }
                    let mut params = order_params_on(&ctx.symbol, &ctx.properties, request);
                    if self.long_only_sale(request, &ctx.properties) {
                        params["reduce_only"] = json!(true);
                    }
                    (method, params)
                }
                Err(reason) => {
                    tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, symbol = %request.symbol, %reason, "submit refused: no instrument to sign on");
                    events.push(Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: request.order_client_id.clone(),
                        reason: reason.into(),
                        timestamp: request.timestamp,
                        refusal_id: vike_model::events::refusal_id_of(
                            OrderRefusalId::NotSent.word(),
                        ),
                    }));
                    return events;
                }
            }
        } else {
            match self.resolve_combo_order(request) {
                Ok(resolved) => resolved,
                Err(reason) => {
                    tracing::warn!(target: "vike_deribit::execution::combo", order_client_id = %request.order_client_id, %reason, "combo submit rejected");
                    events.push(Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: request.order_client_id.clone(),
                        reason: reason.into(),
                        timestamp: request.timestamp,
                        // the combo's registration failed or did not map: the order never left
                        refusal_id: vike_model::events::refusal_id_of(
                            OrderRefusalId::NotSent.word(),
                        ),
                    }));
                    return events;
                }
            }
        };
        // A foreign instrument's row and a combo's registration spend the order's time: the age is asked again.
        if let Some(why) = vike_bridge_core::exec_actor::too_old_to_post(request) {
            events.push(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: why.into(),
                timestamp: request.timestamp,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::NotSent.word(),
                ),
            }));
            return events;
        }
        events.extend(self.dispatch_submit(request, method, &params, started));
        events
    }

    /// Idempotent cancel. On a DEAD SOCKET the cancel is re-dialed and re-sent ONCE — and that is
    /// a deliberately different answer from the submit path's "never re-send", justified by
    /// idempotency rather than by the failure being any less ambiguous:
    ///
    /// * `private/cancel` names one immutable venue `order_id`, so a re-send can only ever act on
    ///   THAT order. It cannot create anything, and it cannot reach a second order the way a
    ///   resent `private/buy` would (Deribit does not dedupe on `label`).
    /// * If the first attempt already took effect, the retry comes back in [`NOT_FOUND`], which
    ///   this method has always swallowed as success. So "already cancelled" and "cancelled now"
    ///   are the same outcome by construction.
    ///
    /// The alternative — heal the socket but report the failure — leaves a live order un-cancelled
    /// on a venue the process can now reach again, which is the wrong direction for a call whose
    /// whole purpose is REMOVING risk. A cancel that genuinely fails still surfaces: `ExecutionClientRest`
    /// turns it into a NON-terminal `OrderCancelRejected` advisory, never a terminal, and one whose outcome
    /// the error leaves open says so ([`cancel_failed`]).
    ///
    /// **A map MISS is not "nothing to cancel".** A client that did not submit the order (a restart
    /// whose engine adopted it with `reregister_orders`, a second start) has no `order_client_id -> order_id`
    /// entry, so the id is asked of the venue BY LABEL ([`Self::find_order_id_by_label`], our order client id
    /// being the label). Only the venue answering "no order under that label" is the no-op `Ok(())`;
    /// a failed lookup is the error it is, never a success. A map HIT sends no lookup at all.
    ///
    /// ⚠ **An id the venue calls GONE is not yet "nothing to cancel".** A trigger order that FIRES ends
    /// its own `SLTS-` id and goes on as a NEW order under the same label (MEASURED for a `stop_market`,
    /// whose market child fills and is gone): a `stop_limit` or `take_limit` child is a LIMIT order that
    /// RESTS, so swallowing the stale id's `NOT_FOUND` reported `OrderCanceled` for an order the venue
    /// still held. So a gone id is followed by one label lookup, and a different id under the label is
    /// cancelled in its place (the same id, or none, is the old `Ok(())`; a failed lookup is an error).
    ///
    /// **A failed label lookup sent no cancel** (decision 0303): its lost reply is NOT SENT (`code` 0), never the
    /// sentinel, which `vike_bridge_core::rest::cancel_rejected` words as a cancel of unknown outcome; a venue's
    /// refusal of it keeps the venue's code. A cancel frame's own sentinel (a lost reply, an order frame's 13009) is
    /// that unknown outcome.
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        use vike_bridge_core::rest::OrderCancelAck;
        let unsent = |e: VenueApiError| match e.code {
            vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                VenueApiError { code: 0, msg: e.msg }
            }
            _ => e,
        };
        let known = self.order_ids.lock().unwrap().venue_order_id_of(order_client_id).cloned();
        let order_id = match known {
            Some(id) => id,
            None => match self.find_order_id_by_label(order_client_id).map_err(unsent)? {
                Some(id) => id,
                None => return Ok(OrderCancelAck::AlreadyGone), // the venue holds nothing under this label
            },
        };
        if self.cancel_redialing(&order_id, order_client_id).map_err(cancel_failed)? {
            return Ok(OrderCancelAck::Canceled);
        }
        match self.find_order_id_by_label(order_client_id).map_err(unsent)? {
            Some(current) if current != order_id => {
                self.order_ids.lock().unwrap().bind(order_client_id, current.clone());
                let canceled =
                    self.cancel_redialing(&current, order_client_id).map_err(cancel_failed)?;
                Ok(if canceled { OrderCancelAck::Canceled } else { OrderCancelAck::AlreadyGone })
            }
            _ => Ok(OrderCancelAck::AlreadyGone),
        }
    }
}

impl VenueRestDeribit {
    /// The venue order id for an order client id this client did not submit, asked of the venue by `label`
    /// ([`Self::query_order_by_label`], the T1 re-query -- no new RPC surface).
    ///
    /// ⚠ **An empty or blank order client id is REFUSED before any frame is sent.** An empty `label` is no
    /// filter on the venue side, so the lookup could answer with some OTHER order of the currency
    /// and the cancel would remove it in the unknown order's place. The refusal is an `Err`, which
    /// `ExecutionClientRest` turns into a non-terminal `OrderCancelRejected`.
    ///
    /// The lookup is a READ, so over a dead socket it re-dials and asks once more (the recon
    /// client's licence); only the `private/cancel` that follows is a write. It returns the newest order
    /// the venue lists under the label; had that one already gone terminal, the cancel comes back in
    /// [`NOT_FOUND`] and is swallowed.
    ///
    /// ⚠ **An explicit client id can be reused**, so a row created before the order this client last
    /// sent under it, past [`LABEL_ROW_CLOCK_SLACK_MS`], is not that order and is never cancelled in its
    /// place. An order this client did not send has no stamp here: the cancel seam carries the order client id alone
    /// and `ExecutionClient::adopt_restored` the order client id and the symbol, so a restart's adopted order is told
    /// by "newest" alone, and an old order alone under its id is still taken for it.
    ///
    /// **Any instrument, so any currency.** The order may sit on any instrument this account trades
    /// (a restart's adopted order), and the lookup is scoped by the instrument's settlement currency,
    /// answering `[]` WITHOUT an error for a wrong one. So every currency this client signed in is
    /// asked (the mounted one first), and only when ALL answer empty, and this client can sign
    /// foreign instruments at all, is the account's unfiltered open-order list read for the label
    /// (measured to span every currency): only an OPEN order can be cancelled or edited, so that list
    /// is complete for the two callers. A failed read anywhere is an `Err`, never "the order is gone".
    /// A client that cannot sign foreign instruments asks the mounted currency alone, as it always did.
    fn find_order_id_by_label(
        &self,
        order_client_id: &str,
    ) -> Result<Option<String>, VenueApiError> {
        if order_client_id.trim().is_empty() {
            return Err(VenueApiError {
                code: 0,
                msg: "cancel refused: empty order client id (an empty label matches any order)"
                    .to_string(),
            });
        }
        let not_before = lock(&self.order_stamps)
            .get(order_client_id)
            .map(|stamp| stamp.saturating_sub(LABEL_ROW_CLOCK_SLACK_MS));
        for currency in self.known_currencies() {
            let asked = match self.query_order_by_label(order_client_id, &currency, not_before) {
                Err(exc) if exc.is_dead_socket() => {
                    self.redial_order_socket(order_client_id, exc.error());
                    self.query_order_by_label(order_client_id, &currency, not_before)
                }
                other => other,
            };
            if let Some(order_id) = asked? {
                return Ok(Some(order_id));
            }
        }
        if self.rows.is_none() {
            return Ok(None);
        }
        const OPEN: &str = "private/get_open_orders";
        let open = match self.private_result(OPEN, &json!({})) {
            Err(exc) if exc.is_dead_socket() => {
                self.redial_order_socket(order_client_id, exc.error());
                self.private_result(OPEN, &json!({}))?
            }
            other => other?,
        };
        let listed = rows_of(OPEN, &open)?
            .iter()
            .filter(|o| o.get("label").and_then(Value::as_str) == Some(order_client_id));
        order_id_among(OPEN, listed, not_before)
    }

    /// One `private/cancel` round trip: `Ok(true)` when the venue cancelled `order_id`, `Ok(false)`
    /// when it says the id no longer exists (its own already-gone codes, "unknown ≠ rejection": the
    /// caller decides what a gone id means, see [`VenueRest::cancel_order`]).
    fn cancel_once(&self, order_id: &str, order_client_id: &str) -> Result<bool, CallError> {
        match self.private_result("private/cancel", &json!({ "order_id": order_id })) {
            Ok(_) => Ok(true),
            Err(exc) if NOT_FOUND.contains(&exc.error().code) => {
                tracing::debug!(target: "vike_deribit::execution::rest", order_client_id, "cancel: order already gone");
                Ok(false)
            }
            Err(exc) => Err(exc),
        }
    }

    /// [`Self::cancel_once`], re-dialed and re-sent ONCE over a dead socket (the justification is
    /// [`VenueRest::cancel_order`]'s: one immutable id, so a re-send acts on that order only).
    fn cancel_redialing(&self, order_id: &str, order_client_id: &str) -> Result<bool, CallError> {
        match self.cancel_once(order_id, order_client_id) {
            Err(exc) if exc.is_dead_socket() => {
                self.redial_order_socket(order_client_id, exc.error());
                // Re-dial failed → this second attempt fails the same way and the ORIGINAL-shaped
                // error surfaces; no separate branch needed.
                self.cancel_once(order_id, order_client_id)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod instrument_class_tests {
    //! `parse_deribit_option_instruments` names the class it parsed
    //! (`docs/decisions/0061-an-instrument-names-its-kind.md`), and names it from the venue's own
    //! `kind` gate rather than from the instrument name.
    use serde_json::json;
    use vike_model::AssetClass;

    use super::parse_deribit_option_instruments;

    #[test]
    fn an_option_row_names_itself_an_option() {
        let payload = json!({"result": [{
            "instrument_name": "BTC-1JAN27-100000-C",
            "kind": "option",
            "tick_size": 0.0005,
            "min_trade_amount": 0.1,
            "contract_size": 1.0,
        }]});
        let out = parse_deribit_option_instruments(&payload);
        let instrument = &out["BTC-1JAN27-100000-C"];
        assert_eq!(instrument.properties.asset_class, Some(AssetClass::Option));
    }

    /// The base is the payload's `base_currency`, not the name: a REAL captured option row keeps
    /// its base, a USDC-settled option name the legacy grammar refuses is admitted with the base
    /// its row publishes, and a row whose base field disagrees with its name follows the field.
    #[test]
    fn the_base_asset_is_the_payloads_base_currency() {
        let all: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/deribit_get_instruments.json"))
                .expect("the fixture is valid JSON");
        let real = all["option_btc"]["result"][0].clone();
        let mut usdc = real.clone();
        usdc["instrument_name"] = json!("BTC_USDC-5OCT26-74000-C");
        let mut renamed = real.clone();
        renamed["instrument_name"] = json!("ETH-5OCT26-74000-C");
        let out = parse_deribit_option_instruments(&json!({"result": [real, usdc, renamed]}));
        assert_eq!(out["BTC-5OCT26-74000-C"].base_asset, "BTC");
        assert_eq!(
            out["BTC_USDC-5OCT26-74000-C"].base_asset, "BTC",
            "admitted by its payload base"
        );
        assert_eq!(out["ETH-5OCT26-74000-C"].base_asset, "BTC", "the field, never the name");
        // A row with NO base field still falls back to the legacy grammar (the r6 shape).
        let legacy = parse_deribit_option_instruments(&json!({"result": [
            {"instrument_name": "BTC-8JUL26-62000-C", "kind": "option", "tick_size": 0.0001},
            {"instrument_name": "not-an-option", "kind": "option"},
        ]}));
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy["BTC-8JUL26-62000-C"].base_asset, "BTC");
    }

    /// The gate, from the class's side: a `kind` this parser does not admit produces NO row at
    /// all, so there is no path by which a non-option is tagged `Option`. Both rows carry an
    /// option-SHAPED name, which is the point — the name is not what decides.
    #[test]
    fn a_non_option_kind_yields_no_row_to_tag() {
        let payload = json!({"result": [
            {"instrument_name": "BTC-PERPETUAL", "kind": "future", "tick_size": 0.5,
             "min_trade_amount": 10.0, "contract_size": 10.0},
            {"instrument_name": "BTC-1JAN27-100000-C", "kind": "option_combo", "tick_size": 0.0005,
             "min_trade_amount": 0.1, "contract_size": 1.0},
        ]});
        assert!(parse_deribit_option_instruments(&payload).is_empty());
    }
}

#[cfg(test)]
mod ended_by_venue_tests {
    //! An edit's answer (or the order read back) that says the order ended is an `OrderCanceled` in the
    //! venue's words, whichever of the two refusal spellings it uses.
    use serde_json::{Value, json};
    use vike_model::events::Event;

    use super::ended_by_venue;

    fn reason(order: Value) -> Option<String> {
        ended_by_venue("n0kd1", &order).map(|e| match e {
            Event::OrderCanceled(c) => c.reason.to_string(),
            other => panic!("an OrderCanceled, got {other:?}"),
        })
    }

    /// The state word the testnet CAPTURED on 2026-11-08 (`crates/bridges/deribit/tests/offline/stop_events.rs`'s
    /// `REFUSED_FIRED_STOP_FRAME`): the refusal and its reason in ONE word, no `reject_reason`.
    #[test]
    fn a_refusal_in_one_state_word_ends_the_order_with_its_reason() {
        let order = json!({"order_id": "USDC-1", "label": "n0kd1", "order_type": "limit",
                           "order_state": "rejected:invalid_reduce_only_order"});
        assert_eq!(
            reason(order.clone()).as_deref(),
            Some("deribit rejected the order: invalid_reduce_only_order")
        );
        let mut both = order;
        both["reject_reason"] = json!("reduce_only");
        assert_eq!(reason(both).as_deref(), Some("deribit rejected the order: reduce_only"));
    }

    /// The plain shapes read as before: the MEASURED crossing edit, a `reject_reason`, no reason at all,
    /// and an order that still rests or filled says nothing.
    #[test]
    fn the_plain_shapes_read_as_before() {
        let plain = |extra: Value| {
            let mut o = json!({"order_id": "USDC-1", "order_state": "rejected"});
            for (k, v) in extra.as_object().expect("an object") {
                o[k] = v.clone();
            }
            reason(o)
        };
        assert_eq!(
            plain(json!({"cancel_reason": "edit_post_only_reject"})).as_deref(),
            Some("deribit rejected the order: edit_post_only_reject")
        );
        assert_eq!(
            plain(json!({"reject_reason": "x"})).as_deref(),
            Some("deribit rejected the order: x")
        );
        assert_eq!(
            plain(json!({})).as_deref(),
            Some("deribit rejected the order: no reason given")
        );
        assert_eq!(
            reason(json!({"order_state": "cancelled", "cancel_reason": "user_request"})).as_deref(),
            Some("deribit cancelled the order: user_request")
        );
        for state in ["open", "untriggered", "filled", "rejectedx"] {
            assert_eq!(reason(json!({ "order_state": state })), None, "{state}");
        }
    }

    /// The end is stamped as an amend's answer is (the owner's ruling of 2026-11-12): the order's own
    /// `last_update_timestamp`, else when it was read. Never `0`.
    #[test]
    fn the_end_carries_the_orders_last_update_else_the_receipt_time() {
        let ts = |order: Value| match ended_by_venue("n0kd1", &order) {
            Some(Event::OrderCanceled(c)) => c.timestamp,
            other => panic!("an OrderCanceled, got {other:?}"),
        };
        let crossed = json!({"order_state": "cancelled", "cancel_reason": "edit_post_only_reject",
                             "last_update_timestamp": 1_794_161_615_502_i64});
        assert_eq!(ts(crossed), 1_794_161_615_502);
        let before = vike_model::now_ms();
        assert!(ts(json!({"order_state": "rejected"})) >= before);
    }
}

#[cfg(test)]
mod admits_in_words_tests {
    //! `admits_in_words` is `vike_model::MoneyModel::admits` in this bridge's two sentences, pinned whole: they
    //! ride the journaled `OrderRejected` of an instrument the mounted client will not sign.
    use vike_model::MoneyModel;
    use vike_model::NotionalKind::{Inverse, Linear};

    use super::admits_in_words;

    #[test]
    fn the_other_kind_and_the_other_settlement_are_refused_in_their_words() {
        let model = |kind, settle: &str| MoneyModel { kind, settle: settle.to_string() };
        let (btc, usdc) = (model(Inverse, "BTC"), model(Linear, "USDC"));
        assert_eq!(admits_in_words(&btc, &model(Inverse, "BTC"), "BTC-27MAR26"), Ok(()));
        assert_eq!(
            admits_in_words(&btc, &usdc, "BTC_USDC-PERPETUAL"),
            Err("BTC_USDC-PERPETUAL is LINEAR (its amount counts the base coin) but this account is mounted on a \
                 INVERSE (its amount counts US dollars) instrument, and one account trades one money model; use \
                 an account mounted on a LINEAR (its amount counts the base coin) instrument"
                .to_string())
        );
        assert_eq!(
            admits_in_words(&btc, &model(Inverse, "ETH"), "ETH-PERPETUAL"),
            Err("ETH-PERPETUAL settles in ETH but this account's inverse instrument settles in BTC: one wallet, \
                 one settlement currency; use an account mounted on a ETH instrument"
                .to_string())
        );
        assert_eq!(
            admits_in_words(&usdc, &model(Linear, "USDT"), "ETH_USDT-PERPETUAL"),
            Err("ETH_USDT-PERPETUAL settles in USDT but this account's linear instrument settles in USDC: one \
                 wallet, one settlement currency; use an account mounted on a USDT instrument"
                .to_string())
        );
    }
}

#[cfg(test)]
mod venue_error_tests {
    //! An error object the venue answered with is never read as NOT SENT (`0`): a missing or non-integer
    //! `code` is the sentinel, so the submit asks by label and the edit reads the order back.
    use serde_json::json;
    use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;

    use super::venue_error;

    #[test]
    fn an_error_object_naming_no_code_is_the_sentinel_and_a_code_is_kept() {
        let read = |err| {
            let e = venue_error(&err);
            (e.code, e.msg)
        };
        assert_eq!(read(json!({"message": "m"})), (E_TIMEOUT_AMBIGUOUS, "m".to_string()));
        assert_eq!(read(json!({"code": "10009"})), (E_TIMEOUT_AMBIGUOUS, String::new()));
        assert_eq!(
            read(json!({"code": 10009, "message": "not_enough_funds"})),
            (10009, "not_enough_funds".to_string())
        );
    }
}

#[path = "properties_recorder_tests.rs"]
#[cfg(test)]
mod properties_recorder_tests;

#[path = "time_in_force_table_tests.rs"]
#[cfg(test)]
mod time_in_force_table_tests;

#[path = "tick_scheme_order_format_tests.rs"]
#[cfg(test)]
mod tick_scheme_order_format_tests;
