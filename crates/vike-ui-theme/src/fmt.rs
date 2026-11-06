//! Number formatting helpers, deduped into one home. Before this crate these lived as
//! near-identical siblings across the GUI crates: two thousands-groupers (`fmt_count` u64 /
//! [`fmt_thousands`] f64) plus two K/M/B compactors ([`fmt_count_compact`] u64 /
//! [`fmt_compact`] f64) coexisted. Bodies here are verbatim from their originals
//! (`vike-data-manager/src/view.rs` for the u64 family, `vike-chart/src/chart/fmt.rs` for the f64
//! family) — behavior is byte-identical, pinned by the tests below.
//!
//! Byte counts are NOT here: the byte-count display lives in `vike_util::bytes`, where the CLI
//! and the startup preflight, which cannot link an egui crate, name the same home. The GUI's data
//! screens speak `vike_util::bytes::iec_bytes` (`1536 -> "1.5 KiB"`).
//!
//! Two type families on purpose: the `u64` helpers are for exact integer counts (the Data-Manager
//! grid); the `f64` helpers are for the chart's scaled/mapped readouts. They format differently
//! (e.g. [`fmt_count_compact`] always shows one decimal, `430_000 -> "430.0K"`, while
//! [`fmt_compact`] trims trailing zeros, `3_450_000.0 -> "3.45M"`) — that difference is deliberate
//! and preserved, so they are NOT collapsed into one function.

/// Thousands-separated integer row count: `1234567 -> "1,234,567"`.
pub fn fmt_count(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in bytes.iter().enumerate() {
        if i != 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c as char);
    }
    out
}

/// Compact K/M/B integer count for the space-constrained venue rollup header, e.g.
/// `2_145_000_000 -> "2.1B"`. Falls back to the plain digits below 1000. Always one decimal.
pub fn fmt_count_compact(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")];
    for (div, suffix) in UNITS {
        if n >= div {
            return format!("{:.1}{suffix}", n as f64 / div as f64);
        }
    }
    n.to_string()
}

/// Thousands-grouped f64 with 2 decimals — the chart's default price/axis readout.
pub fn fmt_thousands(v: f64) -> String {
    fmt_thousands_prec(v, 2)
}

/// [`fmt_thousands`] with a caller-chosen decimal count — the engine behind the chart's precision
/// override (Linear/Log price readouts). `prec == 0` drops the fractional part entirely (no trailing
/// dot); comma grouping is preserved at every precision.
pub fn fmt_thousands_prec(v: f64, prec: usize) -> String {
    let neg = v < 0.0;
    let s = format!("{:.*}", prec, v.abs());
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s.as_str(), None),
    };
    let n = int.len();
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let sign = if neg { "-" } else { "" };
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// Compact K/M/B f64 label (chart volume axis): `950` → "950", `1200` → "1.2K", `3450000` →
/// "3.45M". Up to 2 decimals, trailing zeros trimmed.
pub fn fmt_compact(v: f64) -> String {
    let a = v.abs();
    let (div, suf) = if a >= 1e9 {
        (1e9, "B")
    } else if a >= 1e6 {
        (1e6, "M")
    } else if a >= 1e3 {
        (1e3, "K")
    } else {
        (1.0, "")
    };
    if suf.is_empty() {
        return format!("{:.0}", v);
    }
    let s = format!("{:.2}", v / div);
    format!("{}{}", s.trim_end_matches('0').trim_end_matches('.'), suf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_count_adds_thousands_separators() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(7), "7");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
    }

    #[test]
    fn fmt_count_compact_scales_to_k_m_b() {
        assert_eq!(fmt_count_compact(500), "500");
        assert_eq!(fmt_count_compact(2_145_000_000), "2.1B");
        assert_eq!(fmt_count_compact(430_000), "430.0K");
    }

    #[test]
    fn fmt_thousands_groups_and_signs() {
        assert_eq!(fmt_thousands(0.0), "0.00");
        assert_eq!(fmt_thousands(1234.5), "1,234.50");
        assert_eq!(fmt_thousands(-1_000_000.0), "-1,000,000.00");
        assert_eq!(fmt_thousands_prec(1234.567, 0), "1,235");
        assert_eq!(fmt_thousands_prec(1234.5, 3), "1,234.500");
    }

    #[test]
    fn fmt_compact_volume_labels() {
        assert_eq!(fmt_compact(0.0), "0");
        assert_eq!(fmt_compact(950.0), "950");
        assert_eq!(fmt_compact(1_200.0), "1.2K");
        assert_eq!(fmt_compact(3_450_000.0), "3.45M");
        assert_eq!(fmt_compact(7_100_000_000.0), "7.1B");
    }
}
