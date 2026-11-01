//! The `--entries` CSV reader that `cheap_np_depth` and `cheap_np_askgate` both read.

use std::path::PathBuf;

/// One entry row read from the `--entries` CSV (`cheap_np_run --signals-out`, joined to the CLOB
/// `token_id` of the entered outcome).
#[derive(Debug, Clone)]
pub struct Entry {
    pub sts: i64,
    pub ts_ms: i64,
    pub oidx: u8,
    pub ask: f64,
    pub edge: f64,
    pub won: f64,
    pub token_id: String,
}

pub fn read_entries(path: &PathBuf) -> Result<Vec<Entry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("sts,") {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 {
            continue;
        }
        let parse = |i: usize| -> Result<f64, String> {
            f[i].parse::<f64>().map_err(|e| format!("field {i} of {line:?}: {e}"))
        };
        out.push(Entry {
            sts: f[0].parse().map_err(|e| format!("sts of {line:?}: {e}"))?,
            ts_ms: f[1].parse().map_err(|e| format!("ts_ms of {line:?}: {e}"))?,
            oidx: f[2].parse().map_err(|e| format!("oidx of {line:?}: {e}"))?,
            ask: parse(3)?,
            edge: parse(4)?,
            won: parse(5)?,
            token_id: f[6].trim().to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! Characterisation of the reader as `cheap_np_depth` and `cheap_np_askgate` each carried a
    //! copy of it: what is skipped, what is read, and the exact refusal text, so the one shared
    //! definition provably answers what both copies answered.

    use super::*;

    const HEADER: &str = "sts,ts_ms,outcome_index,ask,edge,won,token_id";

    fn read(text: &str) -> Result<Vec<Entry>, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.csv");
        std::fs::write(&path, text).unwrap();
        read_entries(&path)
    }

    #[test]
    fn a_header_row_and_blank_lines_are_skipped_and_a_good_row_is_read_field_by_field() {
        let got =
            read(&format!("{HEADER}\n\n   \n1700000000,1700000001500,1,0.125,0.0625,1, tok7 \n"))
                .unwrap();
        assert_eq!(got.len(), 1);
        let e = &got[0];
        assert_eq!((e.sts, e.ts_ms, e.oidx), (1_700_000_000, 1_700_000_001_500, 1));
        assert_eq!((e.ask, e.edge, e.won), (0.125, 0.0625, 1.0));
        assert_eq!(e.token_id, "tok7", "the token is trimmed");
    }

    #[test]
    fn crlf_line_ends_and_surrounding_whitespace_read_like_plain_lines() {
        let got = read(&format!("{HEADER}\r\n  10,20,0,0.5,0.25,0,abc  \r\n")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].sts, got[0].ts_ms, got[0].oidx), (10, 20, 0));
        assert_eq!(got[0].token_id, "abc");
    }

    #[test]
    fn a_short_row_is_skipped_silently_and_extra_fields_are_ignored() {
        let got = read("1,2,0,0.5,0.25,0\n3,4,1,0.5,0.25,1,tok,extra,fields\n").unwrap();
        assert_eq!(got.len(), 1, "the six-field row is skipped, not refused");
        assert_eq!((got[0].sts, got[0].token_id.as_str()), (3, "tok"));
    }

    #[test]
    fn a_non_numeric_price_field_refuses_the_file_naming_the_field_and_the_line() {
        let err =
            read(&format!("{HEADER}\n1,2,0,0.5,0.25,1,ok\n1,2,0,abc,0.25,1,tok\n")).unwrap_err();
        assert_eq!(err, "field 3 of \"1,2,0,abc,0.25,1,tok\": invalid float literal");
        let err = read("1,2,0,0.5,0.25,x,tok\n").unwrap_err();
        assert_eq!(err, "field 5 of \"1,2,0,0.5,0.25,x,tok\": invalid float literal");
    }

    #[test]
    fn a_non_numeric_integer_field_refuses_the_file_naming_the_column() {
        assert_eq!(
            read("x,2,0,0.5,0.25,1,tok\n").unwrap_err(),
            "sts of \"x,2,0,0.5,0.25,1,tok\": invalid digit found in string"
        );
        assert_eq!(
            read("1,2.5,0,0.5,0.25,1,tok\n").unwrap_err(),
            "ts_ms of \"1,2.5,0,0.5,0.25,1,tok\": invalid digit found in string"
        );
        assert_eq!(
            read("1,2,300,0.5,0.25,1,tok\n").unwrap_err(),
            "oidx of \"1,2,300,0.5,0.25,1,tok\": number too large to fit in target type"
        );
    }

    #[test]
    fn an_empty_file_and_a_header_only_file_read_as_no_entries() {
        assert!(read("").unwrap().is_empty());
        assert!(read(&format!("{HEADER}\n")).unwrap().is_empty());
    }

    #[test]
    fn a_missing_file_is_an_error_that_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.csv");
        let err = read_entries(&path).unwrap_err();
        assert!(err.starts_with(&format!("{}: ", path.display())), "{err}");
    }
}
