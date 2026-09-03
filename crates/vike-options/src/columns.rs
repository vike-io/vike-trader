//! Pure column model for the options-chain grid (no egui) — ports
//! `vike-trader-app data/options/columns.py`.
//!
//! Defines the per-side field set for the two views — "chain" (TradingView/TradeStation style:
//! Theor/Spread/Bid%/Ask%/Distance/Rel dist/Volume) and "greeks" (Δ/Γ/Θ/V) — plus the value +
//! display computation for each field. The UI consumes these to build cells, so the
//! math/formatting is unit-tested without a widget. Contract notes:
//! - `header`/`kind` panic on an unknown field (the Python twin's dict `KeyError` — a
//!   programmer error; every field in the two view lists is covered).
//! - The Python twin's `not spot` / `not q.mark` falsiness (None OR 0.0 → N/A) is ported as
//!   explicit `None`-or-`0.0` checks — never conflate absent with zero elsewhere.
//! - `cell_value`/`fmt` are parity sites: pure-arithmetic fields gate exact, `theor` follows
//!   the greeks tier (see `tests/oracle_parity.rs`). `r` is a plain parameter (see
//!   `crate::greeks`).

use crate::greeks::black_scholes_price;
use crate::model::OptionQuote;

/// Per-side field order, CENTRE -> OUTER (i.e. the order on the puts side, left to right).
/// The calls side uses the reverse so the table mirrors around the central Strike/IV.
/// NB: "annbid"/"annask" (annualized yield) and "ltp" (last traded price) are intentionally
/// omitted from the displayed chain — their value/format logic is kept below (still
/// unit-tested) so the columns can be re-enabled by adding them back here.
pub const CHAIN_FIELDS: [&str; 9] =
    ["volume", "distance", "reldist", "bid", "ask", "spread", "theor", "bidpct", "askpct"];
pub const GREEKS_FIELDS: [&str; 9] =
    ["volume", "oi", "bid", "ask", "mark", "delta", "gamma", "theta", "vega"];

/// Column header label for a field — twin of `columns.HEADERS`.
pub fn header(field: &str) -> &'static str {
    match field {
        "volume" => "Volume",
        "distance" => "Distance",
        "reldist" => "Rel dist",
        "bid" => "Bid",
        "ask" => "Ask",
        "spread" => "Spread",
        "theor" => "Theor",
        "ltp" => "LTP",
        "bidpct" => "Bid %",
        "askpct" => "Ask %",
        "annbid" => "Ann bid %",
        "annask" => "Ann ask %",
        "oi" => "OI",
        "mark" => "Mark",
        "iv" => "IV",
        "delta" => "Δ",
        "gamma" => "Γ",
        "theta" => "Θ",
        "vega" => "V",
        _ => panic!("unknown options column field {field:?}"),
    }
}

/// Value kind -> formatting; "bar" renders a magnitude bar behind an integer (volume) — twin
/// of `columns._KIND` / `columns.kind`.
pub fn kind(field: &str) -> &'static str {
    match field {
        "volume" => "bar",
        "oi" => "int",
        "distance" | "bid" | "ask" | "theor" | "ltp" | "mark" => "px",
        "reldist" | "spread" | "bidpct" | "askpct" | "annbid" | "annask" | "iv" => "pct",
        "delta" | "gamma" | "theta" | "vega" => "g",
        _ => panic!("unknown options column field {field:?}"),
    }
}

const DASH: &str = "—";

/// Raw numeric value for one (field, quote) given the chain context, or `None` if N/A — twin
/// of `columns.cell_value`. `r` feeds the `theor` Black–Scholes price (Python uses its
/// import-time default; pass 0.0 to match).
pub fn cell_value(
    field: &str,
    q: Option<&OptionQuote>,
    spot: Option<f64>,
    dte: i64,
    r: f64,
) -> Option<f64> {
    let q = q?;
    // the Python twin's `not spot` (None OR 0.0 — a zero spot can't scale/measure distance)
    let truthy_spot = spot.filter(|s| *s != 0.0);
    match field {
        "volume" => q.volume,
        "oi" => q.open_interest,
        "bid" => q.bid,
        "ask" => q.ask,
        "mark" => q.mark,
        "ltp" => q.last,
        "iv" => q.iv,
        "delta" => q.delta,
        "gamma" => q.gamma,
        "theta" => q.theta,
        "vega" => q.vega,
        "distance" => spot.map(|s| (q.strike - s).abs()),
        "reldist" => truthy_spot.map(|s| (q.strike - s).abs() / s),
        "bidpct" => match (truthy_spot, q.bid) {
            (Some(s), Some(bid)) => Some(bid / s),
            _ => None,
        },
        "askpct" => match (truthy_spot, q.ask) {
            (Some(s), Some(ask)) => Some(ask / s),
            _ => None,
        },
        "spread" => match (q.bid, q.ask, q.mark.filter(|m| *m != 0.0)) {
            (Some(bid), Some(ask), Some(mark)) => Some((ask - bid) / mark),
            _ => None,
        },
        "theor" => {
            let t = dte as f64 / 365.0;
            black_scholes_price(spot?, q.strike, t, q.iv?, q.kind, r)
        }
        "annbid" | "annask" => {
            // annualized premium yield: (premium / strike) * (365 / days)
            let premium = if field == "annbid" { q.bid } else { q.ask };
            let premium = premium.filter(|p| *p != 0.0)?;
            if q.strike <= 0.0 {
                return None;
            }
            Some((premium / q.strike) * (365.0 / dte.max(1) as f64))
        }
        _ => None,
    }
}

/// Display string for a raw value per the field's kind — twin of `columns.fmt`
/// (Python's `,`-grouped `:,.2f` / `:,.0f`, `:.2f%`, `:.3f` formats).
pub fn fmt(value: Option<f64>, field: &str) -> String {
    let Some(v) = value else { return DASH.to_string() };
    match kind(field) {
        "pct" => format!("{:.2}%", v * 100.0),
        "int" | "bar" => group_thousands(&format!("{v:.0}")),
        "g" => format!("{v:.3}"),
        _ => group_thousands(&format!("{v:.2}")), // px
    }
}

/// Insert `,` thousands separators into a plain `-?\d+(\.\d+)?` decimal string — the Rust twin
/// of Python's `,` format spec (Rust's formatter has no grouping).
fn group_thousands(s: &str) -> String {
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", s),
    };
    let (int_part, frac) = match rest.find('.') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let n = int_part.len();
    let mut out = String::with_capacity(s.len() + n / 3);
    out.push_str(sign);
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.push_str(frac);
    out
}

/// Strike label for the centre spine: drop the trailing ".00" on whole strikes (BTC "64,000")
/// while keeping fractional ones (VIX "14.5") — twin of `OptionsTab._fmt_strike` (UI-side in
/// Python, but pure string math: matches how Deribit/TradingView print strikes).
pub fn fmt_strike(strike: f64) -> String {
    let s = group_thousands(&format!("{strike:.2}"));
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}
