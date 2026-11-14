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
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use serde_json::{Value, json};

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::rest::{VenueRest, amend_answer_time};
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModified, OrderModifyRejected, OrderRejected,
    OrderSubmitted,
};
use vike_model::{AssetClass, OrderRequest, OrderType, SymbolProperties, TimeInForce, TriggerBy};

use crate::execution::combo::{
    ComboMapError, build_combo_order, build_combo_order_params, build_create_combo_params,
    map_combo, parse_combo, parse_combo_grid,
};
use crate::transport::client::{DeribitTransport, is_dead_socket_error, redial_bound, reply_bound};
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

/// The venue's own errors that leave a submit's fate open. The venue's error page
/// (<https://docs.deribit.com/articles/errors.md>, read 2026-11-07; no capture of any exists) calls
/// 13888 `timed_out` "Server did not manage to process request when it was valid (`valid_until`)",
/// 11094 `internal_server_error` "Some unhandled error on server" and 13028 `temporarily_unavailable`
/// "The requested service is not responding or processing the response takes too long", and says of none
/// whether the order may stand. This client sends no `valid_until`, so the first has no stated cause here
/// either.
const OUTCOME_OPEN: [i64; 3] = [13888, 11094, 13028];

/// Does a failed submit with this `code` leave the order's fate UNKNOWN (it may stand)? The transport's
/// own sentinel, or a venue code in [`OUTCOME_OPEN`]: `true` asks by label
/// ([`VenueRestDeribit::resolve_ambiguous_submit`]), never a terminal reject. A clear refusal (no funds,
/// invalid params) and a code nobody has classified stay a reject.
fn submit_outcome_unknown(code: i64) -> bool {
    code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS || OUTCOME_OPEN.contains(&code)
}

/// Canonical venue id — the deribit `venue_time_in_force` row key (tif step-2).
const VENUE: &str = "deribit";

/// How long an empty label lookup waits before the one more ask that may license a terminal reject. When
/// the socket closes just after the frame and the re-dial is quick, the frame may still be on its way to
/// the venue. Nothing measured this lag: [`VenueRestDeribit::query_order_by_label`]'s evidence covers terminal
/// orders, not how soon a new one shows.
const LABEL_LAG_WAIT: Duration = Duration::from_secs(2);

/// When the core's last-resort reject of an order with no answer can fire first: at the daemon's 30 s and 15 s, on its
/// first 15 s sweep past 60 s of the order's registration (`crates/vike-core/src/runtime/watchdog/orders.rs`'s
/// `sweep_stuck_orders`). An answer later than that finds the order already rejected, and a live one rests untracked.
const CORE_REJECTS_FROM: Duration = Duration::from_secs(60);

/// **The most a submit may hold the exec thread**, from the top of `submit_order`, before it stops
/// re-dialing and asking. The order may first have waited `MAX_SUBMIT_WAIT` on the queue, and the two together
/// must end by [`CORE_REJECTS_FROM`]: 20 + 40 = 60. With every wait spent in full the path was 4 + 10 + 10 (a stale
/// socket's probe, dial and auth) + 10 (the reply) + 10 + 10 (the re-dial) + 10 (the lookup) = 64 s, and a
/// second ask alone would have made it 76 s. So every step after the frame runs only if it can end inside
/// this limit, and one that cannot is the order's unknown (nothing is said, the stuck-order ladder settles
/// it), never a reject: the same 64 s path now ends by 34 s, at the cost of its last steps. Not counted:
/// a re-auth inside the lookup's own `call`. A row read or a combo's registration ahead of the frame asks the order's
/// age again (`too_old_to_post`), and a close, which has no age, can answer later and folds by its fill.
const SUBMIT_HOLD_LIMIT: Duration = Duration::from_secs(40);
const _: () = assert!(
    vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT.as_secs() + SUBMIT_HOLD_LIMIT.as_secs()
        <= CORE_REJECTS_FROM.as_secs()
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

/// The venue's spelling of an instrument name typed in any case: upper case, but for the decimal point of
/// an option's strike, which the venue writes in lower case (`XRP_USDC-27MAR26-2d5-C`, MEASURED in the
/// testnet list: 2,908 USDC options, many with a `d`). The door sends this spelling already; the client
/// still asks for it, so a name typed elsewhere reaches the venue as the venue spells it.
fn venue_name(symbol: &str) -> String {
    let mut parts: Vec<String> = symbol.split('-').map(str::to_ascii_uppercase).collect();
    if parts.len() == 4 {
        parts[2] = parts[2].to_ascii_lowercase();
    }
    parts.join("-")
}

/// How long a refused foreign instrument is remembered, so a burst of orders on a symbol the venue
/// does not list (or of the other money model) costs ONE blocking read on the exec thread.
const FOREIGN_REFUSAL_TTL: Duration = Duration::from_secs(30);

/// **What an account's engine counts money in**, read from its mounted instrument's
/// `public/get_instrument` row (decision 0170): whether an order's amount is US dollars (`Inverse`) or
/// the base coin (`Linear`), and the coin the contract settles in. One engine holds ONE model: the
/// ledger is one currency and an inverse engine runs on a configured seed, a linear one on the wallet
/// (`docs/ops/deribit-any-symbol-plan.md`), so the instruments a mounted client may also sign are the
/// ones of the same model ([`MoneyModel::admits`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoneyModel {
    /// Linear or inverse.
    pub kind: vike_model::NotionalKind,
    /// The settlement currency, upper case (`BTC`, `ETH`, `USDC`): the one the label lookup is scoped by.
    pub settle: String,
}

impl MoneyModel {
    /// The model of the row of an instrument an account may ADD to its engine (decision 0233): a
    /// perpetual, a dated future, or a USDC-settled (linear) option; or why the row is not one this build
    /// trades. A spot pair, a coin-settled option, a combo and a quanto contract are each refused BY NAME
    /// (a guess at the model is the defect 0170 removed).
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
    pub fn of_row(row: &Value) -> Result<MoneyModel, String> {
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
            return Err(why);
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

    /// The model of the row an engine is MOUNTED on: the same as [`Self::of_row`] except that an engine
    /// mounted on an option adds no other instrument (a mounted option may be sold short, a thing an added
    /// one may not, so the two are never mixed in one engine).
    pub fn of_mount_row(row: &Value) -> Result<MoneyModel, String> {
        let name = row.get("instrument_name").and_then(Value::as_str).unwrap_or("the instrument");
        if crate::catalog::asset_class_of(row) == Some(AssetClass::Option) {
            return Err(format!(
                "{name} is an option; an engine mounted on one adds no instrument"
            ));
        }
        Self::of_row(row)
    }

    /// Whether an account mounted on `self`'s instrument may also trade `other`'s (`name`): the same
    /// kind and the same settlement currency (one wallet). An inverse engine keeps its books in its
    /// settle coin (decision 0258, `crates/vike-execution/src/account.rs`'s `Account::set_coin_books`), and
    /// the venue keeps one wallet and one margin per coin, so an inverse contract of ANOTHER coin
    /// (`ETH-PERPETUAL` beside `BTC-PERPETUAL`) is refused like a linear one of another settlement.
    pub fn admits(&self, other: &MoneyModel, name: &str) -> Result<(), String> {
        use vike_model::NotionalKind::{Inverse, Linear};
        let word = |k| match k {
            Inverse => "INVERSE (its amount counts US dollars)",
            Linear => "LINEAR (its amount counts the base coin)",
        };
        if self.kind != other.kind {
            return Err(format!(
                "{name} is {} but this account is mounted on a {} instrument, and one account \
                 trades one money model; use an account mounted on a {} instrument",
                word(other.kind),
                word(self.kind),
                word(other.kind),
            ));
        }
        if self.settle != other.settle {
            let what = if self.kind == Linear { "linear" } else { "inverse" };
            return Err(format!(
                "{name} settles in {} but this account's {what} instrument settles in {}: one wallet, \
                 one settlement currency; use an account mounted on a {} instrument",
                other.settle, self.settle, other.settle
            ));
        }
        Ok(())
    }
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
    let theirs = MoneyModel::of_row(row)?;
    mounted.admits(&theirs, listed)?;
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
    pub symbol: String,
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
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
    Some(Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.to_string(),
        reason: format!("deribit {word} the order: {why}").into(),
        timestamp: amend_answer_time(order.get("last_update_timestamp").and_then(Value::as_i64)),
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

/// Why a COMBO cannot go out from an account whose wallet is `currency` (decision 0258: one engine keeps one
/// settle coin, and the venue keeps one wallet per coin): a leg whose name settles in another currency, which
/// includes a combo whose legs settle in different coins. A terminal `OrderRejected` with this text, before
/// `private/create_combo` registers anything. `None` for a single order and a combo of the account's own coin.
fn combo_settle_refusal(currency: &str, request: &OrderRequest) -> Option<String> {
    let leg = request
        .combo_legs
        .iter()
        .find(|leg| !settle_of_name(&leg.symbol).eq_ignore_ascii_case(currency))?;
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
fn trigger_refusal(request: &OrderRequest, properties: &SymbolProperties) -> Option<String> {
    if !request.order_type.is_trigger() {
        return None;
    }
    let kind = request.order_type.as_str();
    let positive = |p: Option<f64>| p.is_some_and(|p| p.is_finite() && p > 0.0);
    if request.order_type == OrderType::Trailing {
        if let Some(at) = request.trigger_price {
            return Some(format!(
                "deribit's trailing_stop has no activation price; this trailing order states one \
                 ({at}): refused, nothing sent"
            ));
        }
        match request.trail {
            Some(t) if positive(Some(t)) && trail_on_grid(properties, t) > 0.0 => {}
            Some(t) if positive(Some(t)) => {
                return Some(format!(
                    "deribit: the trail {t} is under the instrument's tick {}; refused, nothing sent",
                    properties.tick_size
                ));
            }
            _ => return Some("deribit trailing needs a trail above zero".to_string()),
        }
    } else if !positive(request.trigger_price) {
        return Some(format!("deribit {kind} needs a positive trigger price"));
    }
    if request.order_type == OrderType::StopLimit && !positive(request.price) {
        return Some("deribit stop_limit needs a positive limit price".to_string());
    }
    if properties.asset_class == Some(AssetClass::Option) {
        return Some(format!(
            "deribit takes no {kind} order on an option (trigger orders exist for futures and \
             perpetuals only)"
        ));
    }
    None
}

/// Why an order's `display_qty` (an iceberg's visible slice, the venue's `display_amount`) cannot go out
/// as it stands, before any frame is written (a terminal `OrderRejected` with this text): it rides a
/// plain LIMIT only, it must be a positive number, and the venue has no iceberg on an OPTION (the venue
/// matrix: iceberg and trigger orders do not work on options and combos). Never sent at the order's full
/// visible size, never dropped. The venue's own size rules (a display of at least 100 times the
/// instrument's minimum, a hidden part under 100 times the visible one) stay the venue's to refuse, in
/// its words. `None` for an order with no `display_qty`.
fn display_refusal(request: &OrderRequest, properties: &SymbolProperties) -> Option<String> {
    let shown = request.display_quantity?;
    if request.order_type != OrderType::Limit {
        return Some(format!(
            "deribit shows part of a limit order only (`display_amount`); a {} order with a \
             display quantity is refused, never sent at its full size",
            request.order_type
        ));
    }
    if !(shown.is_finite() && shown > 0.0) {
        return Some(format!("deribit needs a positive display quantity, not {shown}"));
    }
    if properties.asset_class == Some(AssetClass::Option) {
        return Some(
            "deribit takes no iceberg (`display_amount`) on an option; refused, never sent at its \
             full size"
                .to_string(),
        );
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
            properties,
            currency: currency.to_string(),
            order_ids: Mutex::default(),
            order_stamps: Mutex::new(HashMap::new()),
            model: None,
            rows: None,
            foreign: Mutex::new(HashMap::new()),
            submit_hold_limit: SUBMIT_HOLD_LIMIT,
        }
    }

    /// Override `SUBMIT_HOLD_LIMIT` (the offline tests reach the end of the budget without waiting for
    /// it).
    #[must_use]
    pub fn with_submit_hold_limit(mut self, limit: Duration) -> Self {
        self.submit_hold_limit = limit;
        self
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

    fn is_mounted(&self, symbol: &str) -> bool {
        symbol.is_empty() || symbol.eq_ignore_ascii_case(&self.symbol)
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
        let key = venue_name(symbol);
        match locked(&self.foreign).get(&key) {
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
        let mut held = locked(&self.foreign);
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
    /// mounted one, or a foreign one already resolved): the audit-T1 re-query uses it and never guesses.
    fn known_currency(&self, symbol: &str) -> Option<String> {
        if self.is_mounted(symbol) {
            return Some(self.currency.clone());
        }
        match locked(&self.foreign).get(&venue_name(symbol)) {
            Some(Foreign::Ready(ctx)) => Some(ctx.currency.clone()),
            _ => None,
        }
    }

    /// Every settlement currency this client has signed an order in, the mounted one first.
    fn known_currencies(&self) -> Vec<String> {
        let mut out = vec![self.currency.clone()];
        for foreign in locked(&self.foreign).values() {
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
    pub fn get_order_history(&self, count: u32) -> Result<Value, VenueApiError> {
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
    pub fn get_user_trades(&self, count: u32) -> Result<Value, VenueApiError> {
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
    ) -> Result<Option<String>, VenueApiError> {
        const METHOD: &str = "private/get_order_state_by_label";
        let result = self
            .private_result(METHOD, &json!({ "currency": currency, "label": order_client_id }))?;
        order_id_among(METHOD, rows_of(METHOD, &result)?.iter(), not_before_ms)
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
    pub(crate) fn private_result(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<Value, VenueApiError> {
        let response = self.transport.lock().unwrap().call(method, params)?;
        let (_rid, result, error) = parse_response(&response);
        if let Some(err) = error.filter(|e| !e.is_null()) {
            return Err(VenueApiError {
                code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                msg: err.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(),
            });
        }
        // ⚠ A reply stating neither is NO answer, and a real empty one is `[]` or an object. It used to read as
        // `null`, which the label lookup took for "the venue confirms absent" (the licence for a terminal
        // reject) and a cancel for "cancelled". The frame is on the wire, so the outcome is unknown: ambiguous.
        result.filter(|r| !r.is_null()).ok_or_else(|| VenueApiError {
            code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
            msg: format!("{method}: the venue's reply stated neither a result nor an error"),
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
            .map_err(|e| format!("create_combo failed: {} ({})", e.msg, e.code))?;
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
        locked(&self.order_stamps).insert(request.order_client_id.clone(), request.timestamp);
        let outcome = self.transport.lock().unwrap().call(method, params);
        match outcome {
            Ok(response) => {
                let (_rid, result, error) = parse_response(&response);
                if let Some(err) = error.filter(|e| !e.is_null()) {
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                    let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    if submit_outcome_unknown(code) {
                        tracing::warn!(
                            target: "vike_deribit::execution::rest",
                            order_client_id = %request.order_client_id,
                            code,
                            msg,
                            "the venue's error leaves the order's fate open; asking by label (the order is NEVER re-sent)"
                        );
                        return self.resolve_ambiguous_submit(request, started);
                    }
                    tracing::warn!(target: "vike_deribit::execution::rest", code, msg, "submit rejected");
                    return Some(Event::OrderRejected(OrderRejected {
                        cause: post_only_cause(request, code),
                        order_client_id: request.order_client_id.clone(),
                        reason: msg.to_string().into(),
                        timestamp: request.timestamp,
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
                if is_dead_socket_error(&exc) && self.hold_left(started, redial_bound()) {
                    self.redial_order_socket(&request.order_client_id, &exc);
                }
                if submit_outcome_unknown(exc.code) {
                    // audit T1: the request frame was already on the wire, so Deribit may have
                    // accepted it — re-query by label instead of emitting a false terminal reject
                    // (which would strand a phantom). Since 2026-08-25 this covers EVERY read-half
                    // failure, not only the response timeout.
                    self.resolve_ambiguous_submit(request, started)
                } else {
                    // Definite: the send never reached the wire, so no order exists to strand.
                    tracing::warn!(target: "vike_deribit::execution::rest", code = exc.code, msg = %exc.msg, "submit rejected");
                    Some(Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: request.order_client_id.clone(),
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
    ///   ([`Self::settle_edit`]) — the answer the error code alone cannot give.
    fn edit_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let order_client_id = order.order_client_id.clone();
        let reject = |reason: String| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order_client_id.clone(),
                reason: reason.into(),
                timestamp: amend_answer_time(None),
            })]
        };
        if !order.combo_legs.is_empty() {
            return reject(
                "deribit modify does not edit a combo order (cancel and re-submit instead)"
                    .to_string(),
            );
        }
        if !matches!(order.order_type, OrderType::Limit | OrderType::Stop | OrderType::StopLimit) {
            return reject(
                "deribit modify edits a resting limit, stop or stop_limit order only (a take-profit \
                 or a market order is cancelled and re-submitted instead)"
                    .to_string(),
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
            );
        }
        // The edit is formatted on the grid of the ORDER'S instrument (the mounted one's for the
        // mounted symbol), the grid it was submitted on.
        let ctx = match self.context_for(&order.symbol) {
            Ok(ctx) => ctx,
            Err(why) => return reject(why),
        };
        let known = self.order_ids.lock().unwrap().venue_order_id_of(&order_client_id).cloned();
        let order_id = match known {
            Some(id) => id,
            None => match self.find_order_id_by_label(&order_client_id) {
                Ok(Some(id)) => id,
                Ok(None) => {
                    return reject(
                        "the venue holds no order under this order client id, so there is nothing \
                         to edit"
                            .to_string(),
                    );
                }
                Err(exc) => {
                    return reject(format!(
                        "deribit modify could not look the order up by its label: {}",
                        exc.msg
                    ));
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
                    let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                    tracing::warn!(target: "vike_deribit::execution::rest", code, msg, "edit rejected");
                    let refused = if msg.is_empty() {
                        format!("deribit refused the edit (code {code})")
                    } else {
                        msg.to_string()
                    };
                    if projected.time_in_force != TimeInForce::PostOnly {
                        return reject(refused);
                    }
                    let state = self.private_result(
                        "private/get_order_state",
                        &json!({ "order_id": order_id }),
                    );
                    return match state {
                        Ok(state) => ended_by_venue(&order_client_id, &state)
                            .map_or_else(|| reject(refused), |ended| vec![ended]),
                        Err(exc) => reject(format!(
                            "{refused}; the order could not be re-read ({}), so it is UNKNOWN \
                             whether it still rests (the next reconcile pass settles it)",
                            exc.msg
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
                if is_dead_socket_error(&exc) {
                    self.redial_order_socket(&order_client_id, &exc);
                }
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
                    reject(exc.msg)
                }
            }
        }
    }

    /// After an AMBIGUOUS edit (the frame was on the wire; the reply never came), ask the venue what
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
        let reject = |reason: String| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: amend_answer_time(None),
            })]
        };
        let state =
            self.private_result("private/get_order_state", &json!({ "order_id": order_id }));
        let Ok(state) = state else {
            return reject(
                "deribit modify: the reply was lost and the order could not be re-read, so it is \
                 UNKNOWN whether the edit landed (the next reconcile pass settles it)"
                    .to_string(),
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
        reject(
            "deribit modify: the reply was lost and the order is not at the requested terms (it is \
             untouched or filled), so the edit did not take effect"
                .to_string(),
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
        // combo registration: it used to go out as a MARKET order. So is a combo with a leg of another
        // settlement currency than the account's (decision 0258).
        if let Some(reason) =
            kind_refusal(request).or_else(|| combo_settle_refusal(&self.currency, request))
        {
            tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, %reason, "order kind refused before the wire");
            events.push(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: reason.into(),
                timestamp: request.timestamp,
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
                    if let Some(reason) = trigger_refusal(request, &ctx.properties)
                        .or_else(|| display_refusal(request, &ctx.properties))
                    {
                        tracing::warn!(target: "vike_deribit::execution::rest", order_client_id = %request.order_client_id, %reason, "trigger or iceberg order refused before the wire");
                        events.push(Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: request.order_client_id.clone(),
                            reason: reason.into(),
                            timestamp: request.timestamp,
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
    /// turns it into a NON-terminal `OrderCancelRejected` advisory, never a terminal.
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
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        use vike_bridge_core::rest::OrderCancelAck;
        let known = self.order_ids.lock().unwrap().venue_order_id_of(order_client_id).cloned();
        let order_id = match known {
            Some(id) => id,
            None => match self.find_order_id_by_label(order_client_id)? {
                Some(id) => id,
                None => return Ok(OrderCancelAck::AlreadyGone), // the venue holds nothing under this label
            },
        };
        if self.cancel_redialing(&order_id, order_client_id)? {
            return Ok(OrderCancelAck::Canceled);
        }
        match self.find_order_id_by_label(order_client_id)? {
            Some(current) if current != order_id => {
                self.order_ids.lock().unwrap().bind(order_client_id, current.clone());
                let canceled = self.cancel_redialing(&current, order_client_id)?;
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
        let not_before = locked(&self.order_stamps)
            .get(order_client_id)
            .map(|stamp| stamp.saturating_sub(LABEL_ROW_CLOCK_SLACK_MS));
        for currency in self.known_currencies() {
            let asked = match self.query_order_by_label(order_client_id, &currency, not_before) {
                Err(exc) if is_dead_socket_error(&exc) => {
                    self.redial_order_socket(order_client_id, &exc);
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
            Err(exc) if is_dead_socket_error(&exc) => {
                self.redial_order_socket(order_client_id, &exc);
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
    fn cancel_once(&self, order_id: &str, order_client_id: &str) -> Result<bool, VenueApiError> {
        match self.private_result("private/cancel", &json!({ "order_id": order_id })) {
            Ok(_) => Ok(true),
            Err(exc) if NOT_FOUND.contains(&exc.code) => {
                tracing::debug!(target: "vike_deribit::execution::rest", order_client_id, "cancel: order already gone");
                Ok(false)
            }
            Err(exc) => Err(exc),
        }
    }

    /// [`Self::cancel_once`], re-dialed and re-sent ONCE over a dead socket (the justification is
    /// [`VenueRest::cancel_order`]'s: one immutable id, so a re-send acts on that order only).
    fn cancel_redialing(
        &self,
        order_id: &str,
        order_client_id: &str,
    ) -> Result<bool, VenueApiError> {
        match self.cancel_once(order_id, order_client_id) {
            Err(exc) if is_dead_socket_error(&exc) => {
                self.redial_order_socket(order_client_id, &exc);
                // Re-dial failed → this second attempt fails the same way and the ORIGINAL-shaped
                // error surfaces; no separate branch needed.
                self.cancel_once(order_id, order_client_id)
            }
            other => other,
        }
    }
}

/// The figure the hold's const assert stands on, against the core's own grace: a cut of the grace goes red here.
#[test]
fn the_core_rejects_from_the_figure_the_hold_is_summed_against() {
    // What the live root arms: `crates/vike-tradehub/src/tradehub_cli/live_runtime.rs`'s `submit_ack_timeout`.
    let ack = Duration::from_secs(30);
    let grace = vike_core::CoreConfig::default().submit_ack_confirm_grace;
    assert_eq!(ack + grace * 2, CORE_REJECTS_FROM);
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

#[path = "properties_recorder_tests.rs"]
#[cfg(test)]
mod properties_recorder_tests;

#[path = "time_in_force_table_tests.rs"]
#[cfg(test)]
mod time_in_force_table_tests;

#[path = "tick_scheme_order_format_tests.rs"]
#[cfg(test)]
mod tick_scheme_order_format_tests;
