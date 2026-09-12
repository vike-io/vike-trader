//! **Does a price that sits exactly ON a fine venue grid reach the venue whole?** The LIVE demo
//! smoke for the quantizer fix in `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image`
//! — the first thing in this tree that puts one of those strings on a real wire and then asks the
//! venue what it believes it received.
//!
//! Run it (network + demo credentials; `#[ignore]`d, order-placing):
//!
//! ```text
//! cargo test -p vike-binance --test binance_fine_tick_price_smoke -- --ignored --nocapture
//! ```
//!
//! # The defect this exists for
//!
//! `crates/vike-model/src/scalar.rs`'s `round_to_step` ends in a multiplication — `n as f64 *
//! step` — and where the step's own f64 image sits BELOW its decimal value that product can land
//! one f64 ULP under the grid point. `crates/vike-bridge-core/src/format.rs`'s `quantize_to_step`
//! then reads a ratio of `n - ε`, truncates it to `n - 1`, and emits a WHOLE STEP less: a whole
//! tick off a limit price on a live order. The chain is the production one —
//! `crates/vike-exec/src/risk.rs`'s `check_inner` writes a `round_to` result into the outgoing
//! request, and `crates/bridges/binance/src/family/order_map.rs`'s `build_spot_order_params`
//! hands it to `format_to_step_f`.
//!
//! # What this proves that the offline tests cannot
//!
//! `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`'s
//! `a_grid_multiple_survives_round_to_step_then_the_wire_format` proves the STRING is right,
//! `crates/vike-bridge-core/tests/wire_quantizer_probe.rs` measures the quantizers, and
//! `fixtures/r6/format.json` freezes the byte output. All three stop at the edge of this process.
//! Nothing in the tree had ever asked the other end of the socket what price it was holding. This
//! test does exactly that and only that: rest a limit at an exact multiple of a FINE tick, read
//! Binance's own echo of `price`, and require it to be the multiple that was ordered.
//!
//! Note what would NOT have caught the defect: acceptance. A price one tick low is a perfectly
//! valid order and the venue takes it happily. The order-lifecycle trio in
//! `.github/workflows/live-smokes.yml` cannot catch it either — those rest far-from-market prices
//! on COARSE ticks, where the exposed multiplication never drifts.
//!
//! # What it still does NOT prove
//!
//!   - Only the PRICE leg, and only on binance spot. The `quantity` leg runs through the same
//!     `format_to_step_f` against `stepSize`, which has exposed rungs of its own; this test logs
//!     the venue's `origQty` echo and deliberately does not gate on it.
//!   - Nothing about a FILLED order: the order is far from market by construction and never
//!     trades, so this is a statement about the RESTING price only.
//!   - Nothing about any other venue's wire.
//!   - It is a witness, not a sweep: one symbol, one multiple, one direction, one run.
//!   - ⚠ **The QUANTIZER half is proven before the socket, not by it.** Assertion (1) below fails
//!     on a regressed `format_to_step_f` without sending a byte, so in a pre-fix tree this test
//!     goes red having placed no order and the venue is never consulted. What the venue's echo
//!     adds is the half nothing in this tree had: that a price on a FINE grid is accepted and held
//!     as sent rather than re-snapped, rejected, or rounded at the far end. Read the green as two
//!     claims, not one.
//!
//! # Why binance spot, and why the raw order query rather than the ReconClient
//!
//! Binance spot because (a) its `build_spot_order_params` is a named call site of the fixed
//! function, (b) its `/exchangeInfo` grid reaches ticks fine enough for the exposed family to be
//! plausibly SERVED, and (c) it echoes a resting limit price back as a decimal string.
//!
//! ⚠ That echo does not come through the reconcile seam, and the reason is a real gap rather than
//! a preference: `crates/vike-model/src/reports.rs`'s `OrderStatusReport` carries `avg_px` and no
//! limit-price field at all, so `BinanceReconClient`'s report cannot express the number this test
//! is about (`avg_px` is `0.0` while the order is unfilled). The price is therefore read from
//! `GET /api/v3/order` by `origClientOrderId` — the same endpoint, and the same
//! `binance_broker_coid` re-prefixing, that `crates/bridges/binance/src/spot.rs`'s
//! `query_order_orderid` already uses. The ReconClient is still used for what it CAN say: the
//! order is visible, then it is gone. `crates/bridges/binance/src/spot.rs`'s `connect` is asked
//! for the same price a third way, through the crate's own open-order parser.
//!
//! # The gate, and the skip that is not a pass
//!
//! Double-gated exactly like every other `*_smoke.rs` here (see
//! `crates/bridges/binance/tests/binance_reconcile_smoke.rs`'s
//! `binance_reconcile_order_lifecycle_smoke`): network, plus `BINANCE_DEMO_*` in the credential
//! store, self-skipping with a `tracing::warn!` and an early return when absent. No credential
//! value is ever logged.
//!
//! ⚠ There is a SECOND self-skip and it is the one to read carefully. The venue may simply not
//! serve a symbol whose tick is in the exposed family, or may serve one this account cannot fund.
//! That is a legitimate outcome and it is reported as a `SKIP:` line carrying the whole survey —
//! how many symbols were examined, how many were tradable, how many carried an exposed tick, and
//! how many fell out at each later wall. The counts are printed so that "found nothing" can be
//! told apart from "looked at nothing".
//!
//! ⚠⚠ **A SKIP AND A PROOF ARE THE SAME EXIT CODE, and no line of this file can change that.**
//! `libtest` has two outcomes and a self-skipping test spends one of them; the survey is evidence
//! for a READER, not a signal a script can branch on. Both skip banners therefore go out through
//! `println!` as well as `tracing::warn!` — `vike_log::test_init` honours `RUST_LOG`/`VIKE_LOG`, so
//! a `warn` alone is invisible under `RUST_LOG=error` and the run is then silently green — but
//! `--nocapture` and a human are still what tell the three green outcomes apart (no credentials, no
//! usable symbol, an actual proof). Only the last one prints `fine-tick smoke green`. If this test
//! is ever enrolled in `.github/workflows/live-smokes.yml`, it needs that lane's skip-honesty step
//! (or an opt-in strict mode, which would be a new environment variable and therefore a
//! `vike_ops::settings::SETTINGS` row) before its green means anything unattended.

use std::collections::HashMap;

use vike_binance::BinanceReconClient;
use vike_binance::family::order_map::binance_broker_coid;
use vike_binance::spot::{
    BinanceSpotRest, DEMO_REST, PATH_ACCOUNT, PATH_EXCHANGE_INFO, PATH_ORDER, PATH_TICKER,
    PATH_TIME, parse_symbol_properties,
};
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::format::{format_to_step_f, py_f64_str};
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::recon::ReconClient;
use vike_model::clock::now_ms;
use vike_model::events::Event;
use vike_model::{SymbolProperties, round_to_step};

/// The far-from-market multiplier — the same one
/// `crates/bridges/binance/tests/binance_reconcile_smoke.rs`'s
/// `binance_reconcile_order_lifecycle_smoke` rests at. A BUY at half the mark cannot cross.
const FAR_FROM_MARKET: f64 = 0.5;

/// The CLOSEST to a reference price this test will ever rest, as a fraction of it.
///
/// `crates/bridges/binance/tests/binance_tif_smoke.rs` records that `0.9x` mark is "comfortably
/// below the bid" on this demo venue, so a BUY there still rests. The ceiling exists because the
/// price band below is a CLAMP, and a clamp must not be allowed to walk a candidate into the book.
///
/// It is applied TWICE, against two different references, because the two answer different
/// objections. Against the last trade it bounds the search band. Against the live book's lowest
/// ask (see [`BestAsk`]) it is the thing that actually proves a BUY cannot cross — the last trade
/// on a thin demo symbol can be old enough to say nothing about where the book is.
const NEVER_FILLS: f64 = 0.9;

/// Headroom held inside the venue's own `PERCENT_PRICE_BY_SIDE` floor, when the band has room for
/// it. See the band computation in [`choose_exposed_candidate`] for why it yields rather than
/// closing the band.
///
/// `binance_tif_smoke.rs` and `binance_capture_smoke.rs` both record the other half: `0.5x` mark
/// "trips filter -1013 on the demo venue". So that floor is not a constant this file may pick — it
/// is read per symbol out of `/exchangeInfo` — and this is the margin held above it, because the
/// venue's filter references a five-minute average price while the only reference available here
/// is the last trade. A symbol moving fast enough to defeat that margin fails LOUDLY with the
/// venue's own message and nothing rests; it is an operational miss, not a verdict about the
/// quantizer.
const PERCENT_FILTER_MARGIN: f64 = 1.10;

/// How many grid multiples either side of the far-from-market target are tried while looking for
/// one that actually reproduces the defect's precondition.
///
/// A multiple is a witness only when `n as f64 * tick` lands strictly BELOW the grid point's own
/// float — an exposed tick makes that possible, not certain. Every candidate stays inside the
/// price band regardless, so widening this window can move the price by ticks but never into the
/// book.
const MULTIPLE_SEARCH_WINDOW: i128 = 256;

/// Notional headroom over the symbol's `minNotional`, as in the sibling lifecycle smoke.
const NOTIONAL_MARGIN: f64 = 1.6;

/// Free quote-asset balance required, as a multiple of the order's notional, before a symbol is
/// considered fundable. An order rejected for funding teaches nothing about the wire format.
const FUNDING_HEADROOM: f64 = 2.0;

/// The double-gate every `*_smoke.rs` in this crate uses: load the workspace credential store,
/// look up `BINANCE_DEMO_API_KEY`/`_API_SECRET`, and return `None` (after a warning) when absent so
/// the caller can self-skip. Only the FACT of absence is ever logged.
///
/// ⚠ The `println!` is not decoration and not a duplicate: this skip's whole visibility rests on
/// one line of output, and `vike_log::test_init` builds its filter from `RUST_LOG`/`VIKE_LOG` — so
/// under `RUST_LOG=error` the `tracing::warn!` alone is dropped and an unconfigured box reads as a
/// green proof. See the module doc's second skip note for what this still cannot fix.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("binance", Environment::Demo, &vars);
    if creds.is_none() {
        println!("SKIP: BINANCE_DEMO creds absent — nothing was measured");
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
    }
    creds
}

// ---------------------------------------------------------------------------------------------
// Exact decimal arithmetic, in integers.
//
// Everything in this section is integer-only on purpose. The subject of this test is a value that
// is wrong by less than an ULP right up until it is wrong by a whole step, so a float comparison
// anywhere in the verification would be the defect judging itself. `rust_decimal` is deliberately
// not reached for either: it is not a dependency of this crate, and adding one would strand
// `Cargo.lock`.
// ---------------------------------------------------------------------------------------------

/// A terminating decimal held exactly: `value == digits / 10^scale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dec {
    digits: i128,
    scale: u32,
}

fn all_ascii_digits(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_digit())
}

/// Parse a PLAIN decimal string (no exponent) into its exact integer pair.
///
/// An exponent form returns `None` rather than being interpreted. Every string compared in this
/// file is either a venue field or `py_f64_str` output, both positional; guessing at an unexpected
/// shape is how a verification quietly stops verifying.
fn parse_dec(s: &str) -> Option<Dec> {
    let s = s.trim();
    if s.is_empty() || s.contains(['e', 'E']) {
        return None;
    }
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    if !all_ascii_digits(int_part) || !all_ascii_digits(frac_part) {
        return None;
    }
    let magnitude = format!("{int_part}{frac_part}").parse::<i128>().ok()?;
    let digits = if negative { -magnitude } else { magnitude };
    Some(Dec { digits, scale: u32::try_from(frac_part.len()).ok()? })
}

/// `d` re-expressed at a scale at least as deep as its own. `None` on overflow, or on a shallower
/// target — every caller passes the deeper of the scales involved.
fn at_scale(d: Dec, scale: u32) -> Option<i128> {
    let deepen = scale.checked_sub(d.scale)?;
    d.digits.checked_mul(10i128.checked_pow(deepen)?)
}

/// EXACT decimal equality — never a tolerance, and never a float compare.
///
/// Trailing zeros are not a difference: Binance echoes a price padded to the symbol's own
/// precision (`"0.00012300"` where this side spelled `"0.000123"`), and a padded zero is not a
/// different price. Both sides are lifted to the deeper scale and compared as integers.
fn decimal_eq(a: &str, b: &str) -> bool {
    let (Some(x), Some(y)) = (parse_dec(a), parse_dec(b)) else {
        return false;
    };
    let scale = x.scale.max(y.scale);
    match (at_scale(x, scale), at_scale(y, scale)) {
        (Some(xs), Some(ys)) => xs == ys,
        _ => false,
    }
}

/// How many whole `step`s `got` sits away from `want`, when that is a whole number of them.
///
/// This is what makes a red run readable. The failure hunted here is not "some wrong number", it
/// is "exactly one step low", and a message that says `-1` names the defect while a message
/// quoting two long decimals leaves the reader to subtract them.
fn steps_off(got: &str, want: &str, step: Dec) -> Option<i128> {
    let (g, w) = (parse_dec(got)?, parse_dec(want)?);
    let scale = g.scale.max(w.scale).max(step.scale);
    let difference = at_scale(g, scale)?.checked_sub(at_scale(w, scale)?)?;
    let unit = at_scale(step, scale)?;
    if unit == 0 || difference % unit != 0 {
        return None;
    }
    Some(difference / unit)
}

/// The EXACT decimal `n · step`, rendered at the step's own scale — which is the scale
/// `quantize_to_step` rescales its output to, so this is the string the wire format owes us.
fn render_multiple(n: i128, step: Dec) -> Option<String> {
    let product = n.checked_mul(step.digits)?;
    if step.scale == 0 {
        return Some(product.to_string());
    }
    let unit = 10i128.checked_pow(step.scale)?;
    let sign = if product < 0 { "-" } else { "" };
    let magnitude = product.checked_abs()?;
    let width = step.scale as usize;
    let fraction = format!("{:0width$}", magnitude % unit);
    Some(format!("{sign}{}.{fraction}", magnitude / unit))
}

/// `x == mantissa · 2^exponent`, EXACTLY, for a finite positive `x`. Plain IEEE-754 field
/// extraction — no approximation anywhere, which is the whole point of using it below.
fn decompose(x: f64) -> Option<(u128, i32)> {
    if !(x.is_finite() && x > 0.0) {
        return None;
    }
    let bits = x.to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = u128::from(bits & 0x000f_ffff_ffff_ffff);
    if raw_exponent == 0 {
        Some((fraction, -1074))
    } else {
        Some((fraction | (1u128 << 52), raw_exponent - 1075))
    }
}

/// `x << shift`, or `None` if that would lose a bit.
fn shl_exact(x: u128, shift: u32) -> Option<u128> {
    if shift >= 128 || x.leading_zeros() < shift {
        return None;
    }
    Some(x << shift)
}

/// **THE SELECTION CRITERION.** Does the f64 image of this step sit STRICTLY BELOW the decimal it
/// spells?
///
/// This is the whole reason a symbol is chosen or passed over, so it is COMPUTED rather than
/// looked up in a list of known-bad ticks. The measured exposed set is a MANTISSA family, not a
/// magnitude one — a list of it would be both incomplete and unable to explain itself, while the
/// property is what the defect actually keys on.
///
/// **Why it decides the outcome.** `round_to_step` returns `n as f64 * fl(step)`. When
/// `fl(step) < step` that product is pulled toward the low side of the true grid point `n·step`,
/// and once it lands below that point's own float, its shortest-round-trip decimal is below
/// `n·step` too — which is exactly the input on which the pre-fix `quantize_to_step` truncated a
/// whole step away. When `fl(step) >= step` the product is `>= n·step` in real arithmetic and
/// rounding is monotone, so the result can never fall below the grid point's float and the
/// truncation has nothing to eat.
///
/// **How it is decided, exactly.** `step_f == mantissa · 2^exponent` and the decimal is
/// `m / 10^k`, both exact, so `step_f < m / 10^k` is the integer question
/// `mantissa · 10^k · 2^exponent < m`, cross-multiplied into `u128` with every operation checked.
///
/// `None` means the comparison overflowed and the answer is UNKNOWN. That is deliberately not
/// folded into `false`: an undecidable step is counted as such in the survey, so a skip report can
/// never quietly claim a grid was examined when part of it was not.
fn f64_image_sits_below_its_decimal(step_f: f64, step: Dec) -> Option<bool> {
    let (mantissa, exponent) = decompose(step_f)?;
    let m = u128::try_from(step.digits).ok()?;
    let mut lhs = mantissa.checked_mul(10u128.checked_pow(step.scale)?)?;
    let mut rhs = m;
    if exponent >= 0 {
        lhs = shl_exact(lhs, u32::try_from(exponent).ok()?)?;
    } else {
        rhs = shl_exact(rhs, u32::try_from(-exponent).ok()?)?;
    }
    Some(lhs < rhs)
}

// ---------------------------------------------------------------------------------------------
// Selection over the LIVE grid.
// ---------------------------------------------------------------------------------------------

/// Why each symbol was passed over. Rendered field by field on a skip (see [`Survey::report`]), so
/// "no exposed symbol served" is a claim with evidence rather than a shrug.
///
/// ⚠ Every counter is a DISTINCT wall. An earlier draft folded three unrelated refusals into one
/// `no_witness` bucket, which is the same mistake in miniature that this whole test exists to
/// catch: a number that cannot tell you what it measured. `no_grid_witness` (the grid had no
/// multiple that reproduces the defect's precondition) and `unsized_order` (no order size clears
/// the venue's own floors) fail for reasons an operator would act on completely differently.
#[derive(Default, Debug)]
struct Survey {
    examined: usize,
    tradable: usize,
    exposed: usize,
    undecidable_tick: usize,
    no_live_price: usize,
    no_price_band: usize,
    no_grid_witness: usize,
    unsized_order: usize,
    over_max_qty: usize,
    unfunded: usize,
    could_cross: usize,
    book_unreadable: usize,
}

impl Survey {
    /// The skip report, spelled out rather than `{self:?}`.
    ///
    /// Written as an explicit read of every field on purpose: a `Debug` format is the kind of
    /// "read" that disappears the moment someone reorders the struct, and this string is the ONLY
    /// evidence a skipped run leaves behind.
    fn report(&self) -> String {
        format!(
            "examined={} tradable={} exposed={} undecidable_tick={} no_live_price={} \
             no_price_band={} no_grid_witness={} unsized_order={} over_max_qty={} unfunded={} \
             could_cross={} book_unreadable={}",
            self.examined,
            self.tradable,
            self.exposed,
            self.undecidable_tick,
            self.no_live_price,
            self.no_price_band,
            self.no_grid_witness,
            self.unsized_order,
            self.over_max_qty,
            self.unfunded,
            self.could_cross,
            self.book_unreadable,
        )
    }
}

/// The live ask side of one symbol's book, as [`choose_exposed_candidate`]'s last gate reads it.
///
/// Three outcomes rather than an `Option<f64>`, because "the venue served a book with no asks in
/// it" and "the book could not be read at all" must not resolve to the same answer: the first is
/// positive evidence that a BUY cannot cross, the second is no evidence either way.
enum BestAsk {
    /// The venue served a book and this is its lowest live ask.
    At(f64),
    /// The venue served a book with no ask side — a BUY cannot cross what is not there.
    Empty,
    /// The book could not be read. The last-trade band stands alone for this symbol.
    Unknown,
}

/// One symbol, and the exact grid point this test intends to put on the wire.
struct Candidate {
    symbol: String,
    base_asset: String,
    quote_asset: String,
    properties: SymbolProperties,
    /// The tick as the WIRE path spells it (`py_f64_str` of the parsed f64), held exactly.
    step: Dec,
    /// The grid multiple chosen — the `n` in `n · tick`.
    n: i128,
    /// `round_to_step`'s OWN output for that multiple: the value production would carry.
    price: f64,
    /// The exact decimal `n · tick` — what the venue must come back with.
    grid_decimal: String,
    /// How many f64 ULPs below the grid point's own float `price` landed. `>= 1` is the defect's
    /// precondition, and therefore the number that makes a run meaningful.
    ulps_below: i64,
    qty: f64,
}

/// One `/exchangeInfo` filter field as an f64, or `None` when the venue does not serve it.
fn filter_field(entry: &serde_json::Value, filter_type: &str, field: &str) -> Option<f64> {
    entry
        .get("filters")?
        .as_array()?
        .iter()
        .find(|f| f.get("filterType").and_then(|t| t.as_str()) == Some(filter_type))?
        .get(field)
        .and_then(json_num)
}

/// `/api/v3/ticker/price` over the whole venue, as a symbol to last-price map.
fn mark_prices(tickers: &serde_json::Value) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for row in tickers.as_array().into_iter().flatten() {
        let symbol = row.get("symbol").and_then(|s| s.as_str());
        let price = row.get("price").and_then(json_num);
        if let (Some(symbol), Some(price)) = (symbol, price) {
            out.insert(symbol.to_string(), price);
        }
    }
    out
}

/// `/api/v3/account` balances as an asset to free-amount map.
fn free_balances(account: &serde_json::Value) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    let rows = account.get("balances").and_then(|b| b.as_array());
    for row in rows.into_iter().flatten() {
        let asset = row.get("asset").and_then(|a| a.as_str());
        let free = row.get("free").and_then(json_num);
        if let (Some(asset), Some(free)) = (asset, free) {
            out.insert(asset.to_string(), free);
        }
    }
    out
}

/// What the venue's own `LOT_SIZE`/`NOTIONAL` floors leave as a usable order size.
enum OrderSize {
    /// A size clearing every floor with headroom.
    Ok(f64),
    /// No finite positive size exists at all (a degenerate filter grid).
    Unsized,
    /// The size the floors DEMAND is above the venue's own `maxQty` ceiling — a symbol whose
    /// cheapest legal order this far below the market is bigger than it will accept. Counted
    /// separately because it is a property of the symbol, not of the grid: passing it over is
    /// correct, and submitting would have earned a `-1013` that says nothing about the quantizer.
    OverMaxQty,
}

/// The order size: over `minNotional`, over `minQty`, and rounded UP to a whole lot.
///
/// The UP is load-bearing for the same reason this whole file exists: the wire quantizes size
/// DOWN, so a size sitting mid-lot arrives smaller than it was computed and can land back under a
/// floor. One extra whole step is the headroom.
fn order_qty(properties: &SymbolProperties, price: f64) -> OrderSize {
    let by_notional = if properties.min_notional > 0.0 {
        properties.min_notional * NOTIONAL_MARGIN / price
    } else {
        0.0
    };
    let mut qty = by_notional.max(properties.min_qty).max(properties.step_size);
    if properties.step_size > 0.0 {
        qty = ((qty / properties.step_size).ceil() + 1.0) * properties.step_size;
    }
    if !(qty.is_finite() && qty > 0.0) {
        return OrderSize::Unsized;
    }
    // `0.0` is this struct's absent-is-zero convention (`vike_model::SymbolProperties`), so an
    // unset ceiling must not read as "everything is too big".
    if properties.max_qty > 0.0 && qty > properties.max_qty {
        return OrderSize::OverMaxQty;
    }
    OrderSize::Ok(qty)
}

/// Walk the LIVE grid and return the first symbol satisfying every condition this test needs:
/// tradable, an exposed tick, a live mark, a resting price band the venue's own filters allow, a
/// multiple inside that band which actually reproduces the defect's precondition, and enough free
/// quote asset to fund the order.
///
/// The venue's own `/exchangeInfo` ordering breaks ties, so a run is reproducible against a given
/// grid. No symbol is named anywhere in this file: a hard-coded pair would go on passing the day
/// the venue coarsened its tick, which is the one failure this test must not be able to have.
///
/// `best_ask` is the LAST gate and is deliberately a callback: it is one live book read per
/// otherwise-fully-qualified candidate (see the crossing check at the bottom of the loop), not one
/// per symbol on the grid.
fn choose_exposed_candidate(
    info: &serde_json::Value,
    marks: &HashMap<String, f64>,
    free: &HashMap<String, f64>,
    best_ask: &dyn Fn(&str) -> BestAsk,
    survey: &mut Survey,
) -> Option<Candidate> {
    let properties_by_symbol = parse_symbol_properties(info);
    let symbols = info.get("symbols").and_then(|s| s.as_array());
    for entry in symbols.into_iter().flatten() {
        survey.examined += 1;
        let Some(symbol) = entry.get("symbol").and_then(|s| s.as_str()) else { continue };
        if entry.get("status").and_then(|s| s.as_str()) != Some("TRADING") {
            continue;
        }
        // An ABSENT flag is not a refusal: only an explicit `false` disqualifies a symbol, so a
        // venue that stops serving this field cannot silently empty the whole candidate set.
        if entry.get("isSpotTradingAllowed").and_then(|b| b.as_bool()) == Some(false) {
            continue;
        }
        let Some(&properties) = properties_by_symbol.get(symbol) else { continue };
        let tick = properties.tick_size;
        if !(tick.is_finite() && tick > 0.0) {
            continue;
        }
        survey.tradable += 1;

        // The tick as the WIRE path spells it: `format_to_step_f` routes the f64 through
        // `py_f64_str` before `quantize_to_step` sees a step at all, so that string — not the
        // venue's own `"0.00000100"` padding — is the decimal the truncation divides by.
        let Some(step) = parse_dec(&py_f64_str(tick)) else {
            survey.undecidable_tick += 1;
            continue;
        };
        match f64_image_sits_below_its_decimal(tick, step) {
            Some(true) => survey.exposed += 1,
            Some(false) => continue,
            None => {
                survey.undecidable_tick += 1;
                continue;
            }
        }

        let Some(&mark) = marks.get(symbol) else {
            survey.no_live_price += 1;
            continue;
        };
        if !(mark.is_finite() && mark > 0.0) {
            survey.no_live_price += 1;
            continue;
        }

        // The resting band. Its floor is the venue's OWN "too far from the market" bound, held off
        // by `PERCENT_FILTER_MARGIN`; its ceiling keeps the order unfillable. Resting just inside
        // that floor is the farthest-from-market price the venue will accept, which is exactly what
        // a leave-flat order wants.
        let multiplier_down = filter_field(entry, "PERCENT_PRICE_BY_SIDE", "bidMultiplierDown")
            .or_else(|| filter_field(entry, "PERCENT_PRICE", "multiplierDown"))
            .unwrap_or(0.0);
        let min_price = filter_field(entry, "PRICE_FILTER", "minPrice").unwrap_or(0.0);
        let floor = mark * multiplier_down;
        let high = mark * NEVER_FILLS;
        // ⚠ The margin is held above the venue's floor, but it may NEVER be allowed to close the
        // band, and a fixed `floor · PERCENT_FILTER_MARGIN` does exactly that whenever the floor
        // sits within a tenth of the never-fills ceiling. That is not a hypothetical shape here:
        // TWO test files in this crate record `0.5x` mark tripping `-1013` on this demo venue
        // (`binance_tif_smoke.rs` and `binance_capture_smoke.rs`, both attributing it to
        // `PERCENT_PRICE_BY_SIDE`), so the floor this venue actually serves is well above `0.5`,
        // and a flat 10% over a floor of `0.82` or higher would refuse EVERY symbol on the grid.
        // A test that skips for a reason nobody chose is the failure mode this whole file is
        // written against, and the alternative is cheap: too close to the filter earns a LOUD
        // `-1013` from the venue with nothing resting, so the margin yields to the ceiling rather
        // than the band yielding to the margin. Half the available room is the floor's share.
        let margined = floor * PERCENT_FILTER_MARGIN;
        let midway = floor + (high - floor) * 0.5;
        let low = margined.min(midway).max(min_price).max(tick);
        if !(low.is_finite() && high.is_finite()) || low >= high {
            survey.no_price_band += 1;
            continue;
        }

        // FAR FROM MARKET and an EXACT MULTIPLE do not fight each other: snapping to the grid
        // moves a price by at most half a tick, while the target sits half the mark away, so the
        // snap cannot walk it back toward the book. The clamp is what keeps that true when the
        // venue's own filter is tighter than `FAR_FROM_MARKET`.
        let target = (mark * FAR_FROM_MARKET).clamp(low, high);
        let n0 = (target / tick).round_ties_even();
        if !(n0.is_finite() && n0 >= 1.0) {
            survey.no_grid_witness += 1;
            continue;
        }
        let n0 = n0 as i128;

        let mut witness = None;
        for offset in -MULTIPLE_SEARCH_WINDOW..=MULTIPLE_SEARCH_WINDOW {
            let n = n0 + offset;
            if n < 1 {
                continue;
            }
            // The PRODUCTION chain, spelled as
            // `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` drives its ladder:
            // the snap `crates/vike-exec/src/risk.rs`'s `check_inner` performs, on the multiple we
            // mean to order.
            let price = round_to_step(n as f64 * tick, tick);
            if !(low..=high).contains(&price) {
                continue;
            }
            let Some(grid_decimal) = render_multiple(n, step) else { continue };
            let Ok(grid_float) = grid_decimal.parse::<f64>() else { continue };
            // The precondition, MEASURED rather than assumed: how far below the grid point's own
            // float did the multiplication land? Zero means this multiple proves nothing, however
            // exposed the tick is.
            let ulps_below = grid_float.to_bits() as i64 - price.to_bits() as i64;
            if ulps_below >= 1 {
                witness = Some((n, price, grid_decimal, ulps_below));
                break;
            }
        }
        let Some((n, price, grid_decimal, ulps_below)) = witness else {
            survey.no_grid_witness += 1;
            continue;
        };
        let qty = match order_qty(&properties, price) {
            OrderSize::Ok(qty) => qty,
            OrderSize::Unsized => {
                survey.unsized_order += 1;
                continue;
            }
            OrderSize::OverMaxQty => {
                survey.over_max_qty += 1;
                continue;
            }
        };

        let quote_asset = entry.get("quoteAsset").and_then(|s| s.as_str()).unwrap_or_default();
        let base_asset = entry.get("baseAsset").and_then(|s| s.as_str()).unwrap_or_default();
        if free.get(quote_asset).copied().unwrap_or(0.0) < qty * price * FUNDING_HEADROOM {
            survey.unfunded += 1;
            continue;
        }

        // ⚠ THE LEAVE-FLAT GATE, and the one hazard this test INTRODUCES that its siblings do not
        // have. Every other order-placing smoke in this crate rests on BTCUSDT, whose book is deep
        // and whose last trade is a fair statement of where the market is. This test cannot do
        // that: it must take whichever symbol carries a fine tick, which on a demo venue means an
        // illiquid one whose last trade may be hours old and nowhere near its live book. `0.9x a
        // stale print` is not far from market — it can sit ABOVE the live ask, and then the
        // "far-from-market, never-fills" order fills, on a symbol nobody chose, leaving a position
        // behind. So the last thing checked before a symbol is accepted is the BOOK: the price must
        // clear the lowest live ask by the same never-fills margin.
        //
        // `Unknown` (the venue would not serve a book) deliberately does NOT refuse — it is counted
        // and the last-trade band stands. Refusing there would let one missing endpoint turn the
        // whole test into a permanent silent skip, which is the failure mode this file is written
        // against; and the price is still bounded by `high` either way.
        match best_ask(symbol) {
            BestAsk::At(ask) => {
                // Spelled through `partial_cmp` rather than `!(price <= ceiling)` so that an
                // INCOMPARABLE pair (a NaN out of a malformed level) refuses instead of reading as
                // "clears": the direction that matters here is the safe one, and the negated form
                // silently answers `false` to both questions.
                let clears =
                    price.partial_cmp(&(ask * NEVER_FILLS)).is_some_and(std::cmp::Ordering::is_le);
                if !clears {
                    survey.could_cross += 1;
                    continue;
                }
            }
            BestAsk::Empty => {}
            BestAsk::Unknown => survey.book_unreadable += 1,
        }

        return Some(Candidate {
            symbol: symbol.to_string(),
            base_asset: base_asset.to_string(),
            quote_asset: quote_asset.to_string(),
            properties,
            step,
            n,
            price,
            grid_decimal,
            ulps_below,
            qty,
        });
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The smoke.
// ---------------------------------------------------------------------------------------------

/// ORDER-PLACING. Rests one far-from-market LIMIT BUY at an exact multiple of a fine tick, reads
/// the venue's own echo of the price, cancels, and confirms the order is gone.
#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn a_fine_tick_grid_price_reaches_the_venue_whole() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let signer = BinanceHmacSigner::new(&creds, now_ms);

    // The WHOLE grid, not one symbol: the SELECTION is the point (see the module doc).
    let info = transport.public(DEMO_REST, PATH_EXCHANGE_INFO, &[]).expect("exchangeInfo");
    let tickers = transport.public(DEMO_REST, PATH_TICKER, &[]).expect("ticker/price");
    let marks = mark_prices(&tickers);

    // Server-time skew: the read `crates/bridges/binance/src/spot.rs`'s `server_time_offset`
    // performs, done here because the client cannot be built until a symbol has been chosen.
    let time = transport.public(DEMO_REST, PATH_TIME, &[]).expect("server time");
    let server_ms = time.get("serverTime").and_then(|t| t.as_i64()).expect("serverTime");
    let clock_offset_ms = server_ms - now_ms();
    signer.set_offset_ms(clock_offset_ms);

    let account = transport.signed(DEMO_REST, PATH_ACCOUNT, "GET", &[], &signer).expect("account");
    let free = free_balances(&account);

    // The live book for ONE fully-qualified candidate at a time — the leave-flat gate inside the
    // selector. `/api/v3/depth` reached through the crate's own reader
    // (`vike_binance::market_data::fetch_depth_snapshot`), on its own bounded agent, so a hung
    // venue cannot stall the run.
    let best_ask = |symbol: &str| match vike_binance::market_data::fetch_depth_snapshot(
        DEMO_REST, symbol,
    ) {
        Ok((_, _, asks)) => {
            // The LOWEST ask, computed rather than read off the head of the array: this test does
            // not need to assume the venue sorts its book, and a zero-qty level is not a level.
            let lowest = asks
                .iter()
                .filter(|(px, qty)| *qty > 0.0 && px.is_finite() && *px > 0.0)
                .map(|(px, _)| *px)
                .fold(f64::INFINITY, f64::min);
            if lowest.is_finite() { BestAsk::At(lowest) } else { BestAsk::Empty }
        }
        Err(e) => {
            tracing::warn!(target: "vike_binance", "no book for {symbol}: {e} — the last-trade band stands alone for it");
            BestAsk::Unknown
        }
    };

    let mut survey = Survey::default();
    let candidate = choose_exposed_candidate(&info, &marks, &free, &best_ask, &mut survey);
    let Some(candidate) = candidate else {
        // NOT a pass. The venue served nothing this test could prove anything with, and the counts
        // say which wall it hit.
        //
        // ⚠ Printed as well as logged, and the `println!` is the load-bearing half: `test_init`
        // honours `RUST_LOG`/`VIKE_LOG`, so an operator running with `RUST_LOG=error` would get a
        // SILENT green from a `tracing::warn!` alone. `--nocapture` shows this line whatever the
        // filter says. It is still only a LINE — see this function's own residual note: a skip and
        // a proof are the same exit code, and nothing but a reader tells them apart.
        let report = survey.report();
        println!(
            "SKIP: no symbol on the binance spot demo grid is usable for the fine-tick proof — {report}"
        );
        tracing::warn!(
            target: "vike_binance",
            "SKIP: no symbol on the binance spot demo grid is usable for the fine-tick proof — {report}. \
             A tick is usable only when its f64 image sits below its own decimal (the criterion is \
             `f64_image_sits_below_its_decimal`, computed per symbol), the symbol is priced and fundable, \
             a grid multiple exists inside the venue's resting band that lands below the grid point's \
             own float, and the live book proves a BUY there cannot cross. `exposed` counts the ticks in \
             the family; the later counters say what became of them."
        );
        return;
    };

    tracing::info!(
        target: "vike_binance",
        "selected {} ({}/{}) — tick {} spelled {} on the wire; multiple n={}, round_to_step gave {} \
         which sits {} f64 ULP(s) below the grid point; intended wire decimal {}, qty {}",
        candidate.symbol, candidate.base_asset, candidate.quote_asset,
        candidate.properties.tick_size, py_f64_str(candidate.properties.tick_size),
        candidate.n, candidate.price, candidate.ulps_below, candidate.grid_decimal, candidate.qty
    );

    // (1) THE LOCAL HALF, asserted before a single byte goes out. A failure here is OURS — the
    // quantizer, not the venue — and the run stops without touching an account.
    let wire_price = format_to_step_f(candidate.price, candidate.properties.tick_size);
    assert!(
        decimal_eq(&wire_price, &candidate.grid_decimal),
        "the local quantizer lost the grid multiple BEFORE the wire: format_to_step_f({}, {}) = {} \
         but n={} on this grid is exactly {} ({:?} step(s) off). That is the PRE-FIX behaviour of \
         `crates/vike-bridge-core/src/format.rs`'s `quantize_to_step`, so the fix has regressed.",
        candidate.price,
        candidate.properties.tick_size,
        wire_price,
        candidate.n,
        candidate.grid_decimal,
        steps_off(&wire_price, &candidate.grid_decimal, candidate.step)
    );

    let submit_client = BinanceSpotRest {
        link_id: None,
        signer,
        transport,
        base_url: DEMO_REST.to_string(),
        symbol: candidate.symbol.clone(),
        properties: candidate.properties,
        base_asset: candidate.base_asset.clone(),
    };

    // The verification client is built BEFORE the submit, for two reasons that are both about the
    // window in which an order is resting.
    //
    //   - It is one fewer thing standing between the submit and the cancel.
    //   - Its signer gets the SAME server-clock offset the submit signer took. A fresh signer
    //     starts at zero offset and Binance's `recvWindow` is 5 s, so on a box whose clock has
    //     drifted further than that, every fetch below would fail `-1021` — a red run that names
    //     the clock rather than the quantizer, arriving while a live order is on the book.
    //
    // It is still a SEPARATE signer/transport pair from the submit client — the "fresh REST client
    // per purpose" idiom `binance_reconcile_order_lifecycle_smoke` follows; submit and reconcile
    // never share one instance.
    let recon_signer = BinanceHmacSigner::new(&creds, now_ms);
    recon_signer.set_offset_ms(clock_offset_ms);
    let recon_client = BinanceReconClient::spot(
        recon_signer,
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate()),
        DEMO_REST,
        candidate.symbol.clone(),
    );

    // `serde_json::Number` carries the f64 itself, so `candidate.price` reaches `OrderRequest`
    // bit-identical: no string round trip stands between the value measured above and the value
    // handed to `build_spot_order_params`.
    let coid = format!("vtgrid{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": candidate.symbol,
        "side": 1, "qty": candidate.qty, "order_type": "limit", "price": candidate.price,
        "ts": now_ms()
    }))
    .unwrap();
    let events = submit_client.submit_order(&request);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED — a venue FILTER rejection here is an operational miss rather \
         than a quantizer verdict, so read the venue's own message: {events:?}"
    );

    // Everything is READ first and JUDGED last, with the cancel in between, so that the failure
    // this test exists to catch cannot leave an order resting on a live book. The reads are kept
    // as `Result`s for the same reason: nothing between the submit and the cancel may panic.
    //
    // ⚠ Be honest about the size of that window rather than calling it minimal. FIVE round trips
    // stand between the submit and the cancel: the price echo, the reconcile fetch, and `connect`'s
    // own three (account, openOrders, ticker). Only the FIRST is load-bearing — assertions (3) and
    // (4) are corroboration through two other parsers, and they are what make the window five
    // instead of one. That is a deliberate trade, not an oversight: all three reads need the order
    // still resting, so buying a shorter window means giving up the corroboration. The reads are
    // ordered cheapest-first so a failing venue is more likely to fail before `connect` starts.
    // What remains is real: a network partition anywhere in here leaves a far-from-market,
    // non-filling order that an operator must cancel by hand.
    let rest = &submit_client.transport;
    let url = submit_client.base_url.as_str();
    let rest_signer = &submit_client.signer;
    let query = [
        ("symbol", candidate.symbol.clone()),
        ("origClientOrderId", binance_broker_coid(submit_client.link_id.as_deref(), &coid)),
    ];
    let echo = rest.signed(url, PATH_ORDER, "GET", &query, rest_signer);
    let resting = recon_client.fetch_order_status_reports(0);
    let snapshot = submit_client.connect();

    submit_client.cancel_order(&coid).expect("cancel");
    let after = recon_client
        .fetch_order_status_reports(0)
        .expect("fetch_order_status_reports after cancel");
    assert!(
        after.iter().all(|o| o.client_order_id.as_deref() != Some(coid.as_str())),
        "order {coid} must be gone from the reconcile reports after cancel: {after:?}"
    );

    // (2) THE ASSERTION THIS WHOLE TEST EXISTS FOR. Acceptance proved nothing — a price one tick
    // low is a valid order. This is the venue's OWN statement of what it was holding.
    let echo = echo.expect("GET /api/v3/order — the venue's own echo of the resting price");
    let echo_price = echo
        .get("price")
        .and_then(|p| p.as_str())
        .expect("binance echoes a resting LIMIT price as a decimal string")
        .to_string();
    tracing::info!(
        target: "vike_binance",
        "venue echo: price={echo_price} origQty={:?} (sent price {wire_price}, sent qty {})",
        echo.get("origQty").and_then(|q| q.as_str()),
        format_to_step_f(candidate.qty, candidate.properties.step_size)
    );
    assert!(
        decimal_eq(&echo_price, &candidate.grid_decimal),
        "THE VENUE IS HOLDING A DIFFERENT PRICE. {} multiple n={} on a {} tick: ordered {}, sent \
         {}, venue echoed {} — {:?} whole step(s) off. A `-1` here is exactly the defect that \
         `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` was added to close.",
        candidate.symbol,
        candidate.n,
        py_f64_str(candidate.properties.tick_size),
        candidate.grid_decimal,
        wire_price,
        echo_price,
        steps_off(&echo_price, &candidate.grid_decimal, candidate.step)
    );

    // (3) The reconcile seam still says what it CAN say — the order was really there — even though
    // `crates/vike-model/src/reports.rs`'s `OrderStatusReport` carries no limit price.
    let resting = resting.expect("fetch_order_status_reports");
    let found = resting.iter().find(|o| o.client_order_id.as_deref() == Some(coid.as_str()));
    let Some(row) = found else {
        panic!("order {coid} must appear in the reconcile reports while resting: {resting:?}");
    };
    assert_eq!(row.status, "ACCEPTED", "a resting NEW order normalizes to ACCEPTED");
    assert_eq!(row.side, 1, "BUY -> +1");

    // (4) ...and the crate's own open-order parser reads the same number a third way, through
    // `crates/bridges/binance/src/spot.rs`'s `connect`. The comparison is bit-exact: both sides
    // are the correctly-rounded f64 of the SAME decimal, so anything but equality is a real
    // divergence rather than a rounding artifact.
    let snapshot = snapshot.expect("connect/reconcile");
    let grid_float = candidate.grid_decimal.parse::<f64>().expect("the grid decimal parses");
    let adopted = snapshot.open_orders.iter().find(|o| o.request.client_order_id == coid);
    let Some(adopted) = adopted else {
        panic!("order {coid} must appear in the connect() snapshot while resting");
    };
    assert_eq!(
        adopted.request.price,
        Some(grid_float),
        "connect()'s open-order parse must read back the grid point {} it was sent",
        candidate.grid_decimal
    );

    tracing::info!(
        target: "vike_binance",
        "fine-tick smoke green: {} n={} on tick {} — the exact multiple {} was sent, echoed by the \
         venue, read back through connect(), and cancelled",
        candidate.symbol, candidate.n, py_f64_str(candidate.properties.tick_size),
        candidate.grid_decimal
    );
}

// ---------------------------------------------------------------------------------------------
// The instruments this smoke judges with, checked offline.
//
// These are NOT the smoke. They run in the ordinary lane with no network and no credentials, and
// they exist because an instrument that is wrong makes the live run lie in either direction — a
// silent skip that should have been a selection, or a green that measured nothing.
// ---------------------------------------------------------------------------------------------

mod instruments {
    use super::*;

    /// The ladder `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`'s `LADDER`
    /// sweeps, in f64 form. The decimal spelling is DERIVED here (`py_f64_str`) rather than paired,
    /// because the wire path derives it too.
    const RUNGS: [f64; 10] = [1e-8, 1e-7, 1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 5e-1, 1e0];

    /// Multiples swept per rung — the recording's own sample count.
    const MULTIPLES: i128 = 2000;

    /// The selection criterion must agree with the REAL chain, measured here rather than pinned
    /// from a remembered table.
    ///
    /// Only the sound direction is asserted. A value landing below the grid point's float REQUIRES
    /// the step's own image to sit below its decimal (if `fl(step) >= step` then
    /// `n · fl(step) >= n · step` in real arithmetic, and rounding to f64 is monotone, so the
    /// result cannot fall below the grid point's float). The converse is NOT a theorem — an
    /// exposed step makes a low landing possible, not certain — so this file never asserts that a
    /// rung the predicate calls exposed must lose something.
    #[test]
    fn the_criterion_agrees_with_the_real_chain_on_every_rung() {
        let mut rungs_that_drift = 0usize;
        for step_f in RUNGS {
            let step = parse_dec(&py_f64_str(step_f)).expect("py_f64_str is positional");
            let verdict = f64_image_sits_below_its_decimal(step_f, step);
            assert!(verdict.is_some(), "a ladder rung must be decidable: {step_f}");

            let mut below = 0usize;
            for n in 1..=MULTIPLES {
                let value = round_to_step(n as f64 * step_f, step_f);
                let grid = render_multiple(n, step).expect("a ladder multiple fits an i128");
                let grid_float = grid.parse::<f64>().expect("a plain decimal parses");
                if grid_float.to_bits() > value.to_bits() {
                    below += 1;
                }
            }
            if below > 0 {
                rungs_that_drift += 1;
                assert_eq!(
                    verdict,
                    Some(true),
                    "step {step_f} put {below}/{MULTIPLES} multiples below their grid point, so \
                     its f64 image MUST read as sitting below its decimal — the live selection \
                     would otherwise pass over the very rungs that carry the defect"
                );
            }
        }
        assert!(
            rungs_that_drift > 0,
            "no rung on the ladder drifted at all, so this test just proved nothing about the \
             criterion — either the chain changed or the sweep stopped reaching it"
        );
    }

    /// The worked example from `crates/vike-bridge-core/src/format.rs` — `5 · fl(1e-6)` is
    /// `0.0000049999999999999996` — is precisely the shape the live selection hunts for, and the
    /// fixed formatter carries it whole.
    #[test]
    fn the_worked_example_is_the_precondition_the_selection_looks_for() {
        let tick = 1e-6;
        let step = parse_dec(&py_f64_str(tick)).expect("py_f64_str is positional");
        let price = round_to_step(5.0 * tick, tick);
        let grid = render_multiple(5, step).expect("five micro-lots fit an i128");
        assert_eq!(grid, "0.000005");
        let grid_float = grid.parse::<f64>().expect("a plain decimal parses");
        assert!(
            grid_float.to_bits() as i64 - price.to_bits() as i64 >= 1,
            "round_to_step gave {price}, which must sit BELOW the grid point {grid_float}"
        );
        assert_eq!(f64_image_sits_below_its_decimal(tick, step), Some(true));
        assert!(decimal_eq(&format_to_step_f(price, tick), &grid));
    }

    #[test]
    fn a_padded_venue_echo_is_not_a_different_price() {
        assert!(decimal_eq("0.00012300", "0.000123"));
        assert!(decimal_eq("5", "5.000"));
        assert!(!decimal_eq("0.000122", "0.000123"));
        // ...and a malformed or exponent-form string is never quietly called equal.
        assert!(!decimal_eq("1e-4", "0.0001"));
        assert!(!decimal_eq("", "0.0001"));
        assert!(!decimal_eq("1.2.3", "1.2"));
    }

    #[test]
    fn one_step_low_reads_as_one_step_low() {
        let step = parse_dec("0.000001").expect("a plain decimal");
        assert_eq!(steps_off("0.000004", "0.000005", step), Some(-1));
        assert_eq!(steps_off("0.000005", "0.000005", step), Some(0));
        assert_eq!(steps_off("0.0000055", "0.000005", step), None);
    }

    #[test]
    fn a_multiple_renders_at_the_steps_own_scale() {
        let micro = parse_dec("0.000001").expect("a plain decimal");
        let unit = parse_dec("1.0").expect("a plain decimal");
        let quarter = parse_dec("0.25").expect("a plain decimal");
        assert_eq!(render_multiple(5, micro).as_deref(), Some("0.000005"));
        assert_eq!(render_multiple(123, unit).as_deref(), Some("123.0"));
        assert_eq!(render_multiple(4, quarter).as_deref(), Some("1.00"));
    }

    /// The size floors are checked against the size the VENUE parses back, not the one computed
    /// here — the wire quantizes quantity DOWN against `stepSize`, and a size that clears
    /// `minQty`/`minNotional` before that truncation can fail both after it. A rejected order is
    /// not a proof of anything, so this is the arithmetic that keeps a live run from spending its
    /// one submit on a `-1013`.
    #[test]
    fn an_order_size_clears_the_venue_floors_after_the_wire_has_truncated_it() {
        let price = 0.000_123_f64;
        let properties = SymbolProperties {
            tick_size: 1e-6,
            step_size: 0.1,
            min_qty: 1.0,
            min_notional: 10.0,
            ..Default::default()
        };
        let OrderSize::Ok(qty) = order_qty(&properties, price) else {
            panic!("a symbol with ordinary floors must be sizeable");
        };
        let on_wire: f64 = format_to_step_f(qty, properties.step_size)
            .parse()
            .expect("the wire size is a plain decimal");
        assert!(
            on_wire >= properties.min_qty,
            "the wire size {on_wire} fell under minQty {}",
            properties.min_qty
        );
        assert!(
            on_wire * price >= properties.min_notional,
            "the wire notional {} fell under minNotional {}",
            on_wire * price,
            properties.min_notional
        );
    }

    /// A symbol whose CHEAPEST legal order is bigger than the venue will accept is passed over,
    /// never submitted — and an ABSENT ceiling (`SymbolProperties`' absent-is-`0.0` convention) is
    /// not a ceiling of zero, which would have refused every symbol on the grid.
    #[test]
    fn a_size_the_venue_would_refuse_is_passed_over_rather_than_submitted() {
        let capped = SymbolProperties {
            step_size: 0.1,
            min_qty: 1.0,
            max_qty: 5.0,
            min_notional: 10.0,
            ..Default::default()
        };
        assert!(matches!(order_qty(&capped, 0.001), OrderSize::OverMaxQty));
        let uncapped = SymbolProperties { max_qty: 0.0, ..capped };
        assert!(matches!(order_qty(&uncapped, 0.001), OrderSize::Ok(_)));
    }

    /// An undecidable step is UNKNOWN, never "clean" — otherwise an overflow would silently look
    /// like a symbol that had been examined and cleared.
    #[test]
    fn the_criterion_declines_rather_than_guessing() {
        let too_deep = Dec { digits: 1, scale: 40 };
        assert_eq!(f64_image_sits_below_its_decimal(1e-40, too_deep), None);
        assert_eq!(decompose(0.0), None);
        assert_eq!(decompose(f64::NAN), None);
        assert_eq!(shl_exact(u128::MAX, 1), None);
        assert_eq!(parse_dec("nonsense"), None);
    }
}
