//! Normalized options-chain data model + small pure helpers — ports
//! `vike-trader-app data/options/model.py`.
//!
//! Providers translate their native payloads into these types; the UI only ever consumes
//! [`OptionChain`] / [`Expiry`], so it never knows which feed produced them. Contract notes:
//! - Quote fields are `Option<f64>` (the Python dataclass's `float | None`); absent stays
//!   absent, never 0.0.
//! - `rows` are ascending by strike; expiries settle ~08:00 UTC ([`expiry_ms`]).
//! - Malformed ISO dates panic (the Python twin raises `ValueError` — a programmer error,
//!   not bad data: providers only pass parse-validated dates).
//! - DTE / `limit_strikes` are pure-arithmetic parity sites (hex-bit tier — see
//!   `tests/oracle_parity.rs`); never reclassify them to the relative tier.

use chrono::{DateTime, NaiveDate};

const MONTH_ABBR: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Call/put — the Python twin's `Literal["C", "P"]`, made unrepresentable-if-invalid (the
/// oracle's `ValueError` branch for a bad kind disappears by construction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionKind {
    Call,
    Put,
}

impl OptionKind {
    /// Parse the venue wire letter (`"C"` / `"P"`).
    pub fn from_cp(s: &str) -> Option<Self> {
        match s {
            "C" => Some(Self::Call),
            "P" => Some(Self::Put),
            _ => None,
        }
    }

    /// The venue wire letter (`"C"` / `"P"`).
    pub fn as_cp(self) -> &'static str {
        match self {
            Self::Call => "C",
            Self::Put => "P",
        }
    }
}

/// One option quote, normalized — twin of `model.OptionQuote` (frozen dataclass).
#[derive(Debug, Clone, PartialEq)]
pub struct OptionQuote {
    pub strike: f64,
    pub kind: OptionKind,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub last: Option<f64>,
    pub mark: Option<f64>,
    /// Implied vol as a decimal (0.62 == 62%).
    pub iv: Option<f64>,
    pub open_interest: Option<f64>,
    pub volume: Option<f64>,
    pub delta: Option<f64>,
    pub gamma: Option<f64>,
    /// Per calendar day.
    pub theta: Option<f64>,
    /// Per 1pp change in IV (0.01 decimal).
    pub vega: Option<f64>,
    pub in_the_money: Option<bool>,
    /// Venue contract id (Deribit); `None` for equity feeds.
    pub instrument_name: Option<String>,
}

impl OptionQuote {
    /// A quote with every optional field absent — the Python dataclass's defaults.
    pub fn new(strike: f64, kind: OptionKind) -> Self {
        Self {
            strike,
            kind,
            bid: None,
            ask: None,
            last: None,
            mark: None,
            iv: None,
            open_interest: None,
            volume: None,
            delta: None,
            gamma: None,
            theta: None,
            vega: None,
            in_the_money: None,
            instrument_name: None,
        }
    }
}

/// One expiry with its display metadata — twin of `model.Expiry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expiry {
    /// ISO "YYYY-MM-DD".
    pub date: String,
    /// Days to expiry (>= 0).
    pub dte: i64,
    /// Display label, e.g. "02 Jul" / "0DTE".
    pub label: String,
}

/// Call + put at one strike — twin of `model.StrikeRow`.
#[derive(Debug, Clone, PartialEq)]
pub struct StrikeRow {
    pub strike: f64,
    pub call: Option<OptionQuote>,
    pub put: Option<OptionQuote>,
}

/// Underlying asset class — the Python twin's `Literal["crypto", "equity"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetClass {
    Crypto,
    Equity,
}

/// One expiry's full chain snapshot — twin of `model.OptionChain`.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionChain {
    /// "BTC", "^VIX".
    pub underlying: String,
    pub asset_class: AssetClass,
    pub underlying_price: Option<f64>,
    pub expiry: Expiry,
    /// Snapshot epoch ms (UTC).
    pub asof_ms: i64,
    /// "deribit" | "yfinance".
    pub source: String,
    /// Ascending by strike.
    pub rows: Vec<StrikeRow>,
}

fn parse_iso(date_iso: &str) -> NaiveDate {
    let mut parts = date_iso.split('-').map(|p| p.parse::<i32>());
    let mut next = || {
        parts
            .next()
            .and_then(Result::ok)
            .unwrap_or_else(|| panic!("malformed ISO date {date_iso:?}"))
    };
    let (y, m, d) = (next(), next(), next());
    NaiveDate::from_ymd_opt(y, m as u32, d as u32)
        .unwrap_or_else(|| panic!("invalid calendar date {date_iso:?}"))
}

/// Epoch ms of an option expiry (Deribit/most US options settle ~08:00 UTC) — twin of
/// `model._expiry_ms` (only ever called with its default `hour_utc=8`, kept fixed here).
pub fn expiry_ms(date_iso: &str) -> i64 {
    parse_iso(date_iso)
        .and_hms_opt(8, 0, 0)
        .expect("08:00:00 is always valid")
        .and_utc()
        .timestamp_millis()
}

/// Build an [`Expiry`] (DTE + human label) for an ISO date relative to `now_ms` — twin of
/// `model.make_expiry`.
///
/// DTE is a CALENDAR-day difference (expiry's UTC date minus today's UTC date), not a floored
/// elapsed-ms count: late in the UTC day the ms-floor rounds BOTH today and tomorrow to 0, so
/// the expiry strip rendered two "0DTE" pills. With the calendar diff only the contract that
/// actually settles today reads 0DTE; tomorrow is 1 (and shows its date label).
pub fn make_expiry(date_iso: &str, now_ms: i64) -> Expiry {
    let exp_date = parse_iso(date_iso);
    let today = DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(|| panic!("now_ms {now_ms} out of range"))
        .date_naive();
    let dte = (exp_date - today).num_days().max(0);
    let label = if dte == 0 {
        "0DTE".to_string()
    } else {
        use chrono::Datelike;
        format!("{:02} {}", exp_date.day(), MONTH_ABBR[exp_date.month0() as usize])
    };
    Expiry { date: date_iso.to_string(), dte, label }
}

/// Window the chain to `n` strikes BELOW the spot price and `n` strikes AT/ABOVE it — a
/// symmetric "±n strikes" window of 2n rows centred on the spot marker, so ±3 shows exactly
/// 3 rows above the spot band and 3 below, ±6 shows 6 each side, etc. (The spot price falls
/// between the two middle strikes, so the marker band lands dead-centre with n rows on each
/// side.) Twin of `model.limit_strikes`.
///
/// Returns the chain unchanged for `n = None` / `n == 0` / no spot. Rows stay ascending by
/// strike.
pub fn limit_strikes(chain: OptionChain, n: Option<usize>) -> OptionChain {
    let (Some(n), Some(spot)) = (n.filter(|&n| n > 0), chain.underlying_price) else {
        return chain;
    };
    // Split point: the first strike at/above spot. Take n strikes below it and n at/above it,
    // so the spot marker (inserted at this split) gets exactly n rows on each side — no
    // lopsided +1 strike.
    let split = chain.rows.iter().position(|r| r.strike >= spot).unwrap_or(chain.rows.len());
    let lo = split.saturating_sub(n);
    let hi = (split + n).min(chain.rows.len());
    if lo == 0 && hi == chain.rows.len() {
        return chain;
    }
    let rows = chain.rows[lo..hi].to_vec();
    OptionChain { rows, ..chain }
}
