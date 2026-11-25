//! The ONE text envelope of the model kinds this workspace writes itself (the linear model, the
//! Gaussian process, the ensemble and the conformal wrapper): the writer and the reader side by
//! side, so a format can only change in both halves at once. LightGBM's own text is NOT this
//! format; [`crate::model::parse`] reads it.
//!
//! # The envelope, version 1
//!
//! ```text
//! vike-ml <kind> v1
//! <key>=<value>
//! ...
//! end of model
//! ```
//!
//! - The first line names the KIND and the format VERSION; a reader refuses any other kind or
//!   version (a best-effort read of an unknown layout returns plausible wrong numbers).
//! - Every key is REQUIRED, appears ONCE and in the kind's fixed order. An unknown or missing key is
//!   a refusal: LightGBM's parser skips unknown keys, which is how a new field can be dropped with
//!   every check green (`crates/vike-ml-training/CLAUDE.md`'s "The two pins, and what neither
//!   proves"); this format does not repeat that.
//! - A number is Rust's shortest round-tripping exponent form (`{:e}`), so a parsed model is the
//!   written one BIT FOR BIT. A non-finite number is refused at both ends.
//! - A list of numbers is space-separated on one line, its length fixed by the counts before it.
//! - A NESTED model (an ensemble member, the model a conformal wrapper calibrates) is
//!   `<key>=<byte count>`, a newline, exactly that many bytes of the nested text, and a newline. The
//!   nested text may be ANY model text, LightGBM's included.
//! - Lines end in `\n` only; the text ends with `end of model\n`.
//!
//! Errors are [`MlError::Parse`] with the 1-based line the reader stood on.

use std::fmt::Write as _;

use crate::error::MlError;

/// The first word of every text this module writes. A text that does not start with it is not
/// one of these kinds (`crate::any_model::AnyModel::from_text` reads it as LightGBM's).
pub const HEADER_PREFIX: &str = "vike-ml ";

/// The one format version every kind is written in.
pub const FORMAT_VERSION: &str = "v1";

/// The last line of every text this module writes.
const END: &str = "end of model";

/// The kind word on a text's first line, or `None` when the text is not one of these kinds.
pub(crate) fn kind_of(text: &str) -> Option<&str> {
    let first = text.split('\n').next()?;
    first.strip_prefix(HEADER_PREFIX)?.split(' ').next()
}

/// Builds one text, key after key.
pub(crate) struct Writer {
    out: String,
}

impl Writer {
    pub(crate) fn new(kind: &str) -> Self {
        Self { out: format!("{HEADER_PREFIX}{kind} {FORMAT_VERSION}\n") }
    }

    pub(crate) fn text(&mut self, key: &str, value: &str) {
        let _ = writeln!(self.out, "{key}={value}");
    }

    pub(crate) fn count(&mut self, key: &str, value: usize) {
        let _ = writeln!(self.out, "{key}={value}");
    }

    pub(crate) fn number(&mut self, key: &str, value: f64) {
        let _ = writeln!(self.out, "{key}={value:e}");
    }

    pub(crate) fn numbers(&mut self, key: &str, values: &[f64]) {
        self.out.push_str(key);
        self.out.push('=');
        for (i, value) in values.iter().enumerate() {
            if i > 0 {
                self.out.push(' ');
            }
            let _ = write!(self.out, "{value:e}");
        }
        self.out.push('\n');
    }

    pub(crate) fn nested(&mut self, key: &str, text: &str) {
        let _ = writeln!(self.out, "{key}={}", text.len());
        self.out.push_str(text);
        self.out.push('\n');
    }

    pub(crate) fn finish(mut self) -> String {
        self.out.push_str(END);
        self.out.push('\n');
        self.out
    }
}

/// Reads one text, key after key, in the order its kind's writer wrote them.
pub(crate) struct Reader<'a> {
    text: &'a str,
    position: usize,
    /// The 1-based number of the line the NEXT read starts on.
    line: usize,
}

impl<'a> Reader<'a> {
    /// A reader past the first line, which must name `kind` and [`FORMAT_VERSION`].
    pub(crate) fn open(text: &'a str, kind: &str) -> Result<Self, MlError> {
        let mut reader = Self { text, position: 0, line: 1 };
        let (at, first) = reader.next_line()?;
        let want = format!("{HEADER_PREFIX}{kind} {FORMAT_VERSION}");
        if first != want {
            return Err(MlError::Parse {
                line: at,
                what: format!("expected `{want}`, found `{first}`"),
            });
        }
        Ok(reader)
    }

    /// The 1-based line the next read starts on.
    pub(crate) fn line(&self) -> usize {
        self.line
    }

    fn next_line(&mut self) -> Result<(usize, &'a str), MlError> {
        let rest = &self.text[self.position..];
        let Some(end) = rest.find('\n') else {
            return Err(MlError::Parse {
                line: self.line,
                what: "the text ends before `end of model`".into(),
            });
        };
        let at = self.line;
        self.position += end + 1;
        self.line += 1;
        Ok((at, &rest[..end]))
    }

    /// The value of the next line, which must be `key=…`.
    pub(crate) fn text(&mut self, key: &str) -> Result<&'a str, MlError> {
        let (at, line) = self.next_line()?;
        match line.split_once('=') {
            Some((k, value)) if k == key => Ok(value),
            _ => {
                Err(MlError::Parse { line: at, what: format!("expected `{key}=`, found `{line}`") })
            }
        }
    }

    pub(crate) fn count(&mut self, key: &str) -> Result<usize, MlError> {
        let at = self.line;
        let raw = self.text(key)?;
        raw.parse::<usize>()
            .map_err(|_| MlError::Parse { line: at, what: format!("`{key}={raw}` is not a count") })
    }

    pub(crate) fn number(&mut self, key: &str) -> Result<f64, MlError> {
        let at = self.line;
        let raw = self.text(key)?;
        finite(raw, key, at)
    }

    /// The next line's list, which must hold exactly `length` numbers.
    pub(crate) fn numbers(&mut self, key: &str, length: usize) -> Result<Vec<f64>, MlError> {
        let at = self.line;
        let raw = self.text(key)?;
        let values = if raw.is_empty() {
            Vec::new()
        } else {
            raw.split(' ').map(|token| finite(token, key, at)).collect::<Result<Vec<_>, _>>()?
        };
        if values.len() != length {
            return Err(MlError::Parse {
                line: at,
                what: format!(
                    "`{key}` holds {} numbers; the counts above say {length}",
                    values.len()
                ),
            });
        }
        Ok(values)
    }

    /// A nested text: `key=<byte count>`, that many bytes, then a newline. Returns the line the
    /// key stood on (for a caller wrapping the nested parse's error) and the nested text.
    pub(crate) fn nested(&mut self, key: &str) -> Result<(usize, &'a str), MlError> {
        let at = self.line;
        let length = self.count(key)?;
        let start = self.position;
        let end = start.checked_add(length).filter(|&end| end < self.text.len());
        let Some(end) = end.filter(|&end| self.text.is_char_boundary(end)) else {
            return Err(MlError::Parse {
                line: at,
                what: format!("`{key}={length}` reaches past the end of the text"),
            });
        };
        if self.text.as_bytes()[end] != b'\n' {
            return Err(MlError::Parse {
                line: at,
                what: format!("the {length} bytes of `{key}` are not followed by a newline"),
            });
        }
        let nested = &self.text[start..end];
        self.line += nested.matches('\n').count() + 1;
        self.position = end + 1;
        Ok((at, nested))
    }

    /// The last line, `end of model`, and nothing after it.
    pub(crate) fn finish(mut self) -> Result<(), MlError> {
        let (at, line) = self.next_line()?;
        if line != END {
            return Err(MlError::Parse {
                line: at,
                what: format!("expected `{END}`, found `{line}`"),
            });
        }
        if self.position != self.text.len() {
            return Err(MlError::Parse {
                line: self.line,
                what: format!("text continues after `{END}`"),
            });
        }
        Ok(())
    }
}

fn finite(raw: &str, key: &str, at: usize) -> Result<f64, MlError> {
    match raw.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(value),
        _ => Err(MlError::Parse {
            line: at,
            what: format!("`{key}` holds `{raw}`, which is not a finite number"),
        }),
    }
}

/// A nested parse's error, re-anchored on the line of the key that holds the nested text.
pub(crate) fn nested_error(at: usize, what: &str, error: MlError) -> MlError {
    MlError::Parse { line: at, what: format!("{what}: {error}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(nested: &str) -> String {
        let mut w = Writer::new("sample");
        w.count("count", 3);
        w.number("number", -0.0);
        w.numbers("numbers", &[1.5, f64::MIN_POSITIVE / 8.0, -1e300]);
        w.nested("inner", nested);
        w.text("word", "mean");
        w.finish()
    }

    #[test]
    fn a_written_text_reads_back_bit_for_bit_in_its_key_order() {
        let text = sample("tree\nsome other format\n");
        assert_eq!(kind_of(&text), Some("sample"));
        let mut r = Reader::open(&text, "sample").unwrap();
        assert_eq!(r.count("count").unwrap(), 3);
        assert_eq!(r.number("number").unwrap().to_bits(), (-0.0f64).to_bits());
        let numbers = r.numbers("numbers", 3).unwrap();
        let want = [1.5, f64::MIN_POSITIVE / 8.0, -1e300];
        assert!(numbers.iter().zip(want).all(|(a, b)| a.to_bits() == b.to_bits()));
        let (at, inner) = r.nested("inner").unwrap();
        assert_eq!((at, inner), (5, "tree\nsome other format\n"));
        assert_eq!(r.line(), 9, "the nested text's lines are counted");
        assert_eq!(r.text("word").unwrap(), "mean");
        r.finish().unwrap();
    }

    #[test]
    fn every_departure_from_the_written_shape_is_refused() {
        let text = sample("x");
        let refuse = |bad: String, read: &dyn Fn(&str) -> Result<(), MlError>| {
            assert!(read(&bad).is_err(), "accepted: {bad:?}");
        };
        let full = |t: &str| -> Result<(), MlError> {
            let mut r = Reader::open(t, "sample")?;
            r.count("count")?;
            r.number("number")?;
            r.numbers("numbers", 3)?;
            r.nested("inner")?;
            r.text("word")?;
            r.finish()
        };
        full(&text).unwrap();
        refuse(text.replace("sample v1", "sample v2"), &full);
        refuse(text.replace("sample v1", "other v1"), &full);
        refuse(text.replace("count=3", "total=3"), &full);
        refuse(text.replace("-0e0", "NaN"), &full);
        refuse(text.replace("-0e0", "inf"), &full);
        refuse(text.replace("1.5e0 ", ""), &full);
        refuse(text.replace("inner=1", "inner=2"), &full);
        refuse(text.replace("inner=1", "inner=900"), &full);
        refuse(text.replace("end of model\n", "end of model\nmore\n"), &full);
        refuse(text.replace("end of model\n", "end of model"), &full);
        refuse(text.replace('\n', "\r\n"), &full);
        assert_eq!(kind_of("tree\nversion=v4\n"), None);
    }
}
