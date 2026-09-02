//! Shared header-name-driven CSV helpers for the Databento and Tardis backfill parsers.
//! Both vendors emit a header row and only numeric/string fields with no embedded commas, so a
//! plain `split(',')` is sufficient and no `csv` crate is pulled. Parsers map column NAME → index
//! via `Header` so they are robust to column-order drift between schemas/versions.

use std::collections::HashMap;

/// Split one CSV line into fields. Safe for these vendors: numeric schemas have no quoted commas.
pub fn split_line(line: &str) -> Vec<&str> {
    line.trim_end_matches(['\r', '\n']).split(',').collect()
}

/// Column-name → index map parsed from the header line.
pub struct Header {
    idx: HashMap<String, usize>,
}

impl Header {
    /// Build from the header line (first CSV row).
    pub fn parse(header_line: &str) -> Self {
        let idx = split_line(header_line)
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name.to_string(), i))
            .collect();
        Self { idx }
    }

    /// Field value for `name` in a split `row`, or `None` if the column is absent/short.
    pub fn get<'a>(&self, row: &[&'a str], name: &str) -> Option<&'a str> {
        self.idx.get(name).and_then(|&i| row.get(i)).copied()
    }
}

/// Split a CSV body into `(header, data-row lines)`. `None` when there is no header line. The
/// row-accessor set below (this + [`f64_of`]/[`i64_of`]/[`ts_ms`]) was byte-identical in the
/// databento and tardis parsers before it landed here — its natural shared home (finding F22).
pub fn header_and_rows(csv: &str) -> Option<(Header, std::str::Lines<'_>)> {
    let mut lines = csv.lines();
    let header = Header::parse(lines.next()?);
    Some((header, lines))
}

/// Parse the named column of a split `row` as `f64`. `None` if absent/short or unparsable.
pub fn f64_of(row: &[&str], h: &Header, name: &str) -> Option<f64> {
    h.get(row, name)?.parse::<f64>().ok()
}

/// Parse the named column of a split `row` as `i64`. `None` if absent/short or unparsable.
pub fn i64_of(row: &[&str], h: &Header, name: &str) -> Option<i64> {
    h.get(row, name)?.parse::<i64>().ok()
}

/// A raw integer timestamp column divided by `divisor` → epoch-ms. Databento passes `NS_PER_MS`
/// (ns → ms), Tardis `US_PER_MS` (µs → ms). `None` on parse failure. Routed through [`i64_of`] so
/// a `tardis`-only build (which does not call `i64_of` directly) still exercises it.
pub fn ts_ms(row: &[&str], h: &Header, name: &str, divisor: i64) -> Option<i64> {
    Some(i64_of(row, h, name)? / divisor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_maps_names_to_values_by_position() {
        let h = Header::parse("ts_event,price,size,symbol");
        let row = split_line("1000,42,7,ESZ4");
        assert_eq!(h.get(&row, "ts_event"), Some("1000"));
        assert_eq!(h.get(&row, "price"), Some("42"));
        assert_eq!(h.get(&row, "symbol"), Some("ESZ4"));
        assert_eq!(h.get(&row, "missing"), None);
    }

    #[test]
    fn split_line_strips_trailing_newline() {
        assert_eq!(split_line("a,b,c\r\n"), vec!["a", "b", "c"]);
    }

    #[test]
    fn row_accessors_and_ts_divisor() {
        let (h, mut rows) = header_and_rows("ts,px,qty\n1700000000123000000,42.5,7\n").unwrap();
        let line = rows.next().unwrap();
        let row = split_line(line);
        assert_eq!(i64_of(&row, &h, "ts"), Some(1_700_000_000_123_000_000));
        assert_eq!(f64_of(&row, &h, "px"), Some(42.5));
        assert_eq!(i64_of(&row, &h, "qty"), Some(7));
        assert_eq!(f64_of(&row, &h, "missing"), None);
        // ns → ms and µs → ms via the divisor parameter.
        assert_eq!(ts_ms(&row, &h, "ts", 1_000_000), Some(1_700_000_000_123));
        assert_eq!(ts_ms(&row, &h, "ts", 1_000), Some(1_700_000_000_123_000));
    }
}
