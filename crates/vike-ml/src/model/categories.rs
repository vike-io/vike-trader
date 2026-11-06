//! The stable string↔integer table for a categorical feature.
//!
//! The data file LightGBM trains on carries NUMBERS, and a CLI-trained model records nothing of
//! what an integer MEANT (`pandas_categorical:` is the PYTHON wrapper's; the CLI writes `null`).
//! So the caller keeps the mapping, in this shape: ids from 0 in declaration order, one label per
//! line, saved beside the model artifact.
//!
//! # Two properties the walker depends on
//!
//! * **Ids are never negative.** A negative category is LightGBM's "missing" and routes right
//!   unconditionally, so a scheme that assigned -1 to anything would silently lose it.
//! * **An UNKNOWN label encodes as `NaN`, not as a fallback id.** NaN routes as missing — the
//!   honest answer for a category the model never saw; an existing id would predict as a different
//!   category, silently.
//!
//! # ⚠ This is a WIRE FORMAT between a training run and a serving binary, and it has no version
//!
//! The map and its model are two files with nothing linking them, and pairing the wrong two is
//! silent and TOTAL — every category means a different thing. The minimum defence:
//! [`CategoryMap::to_text`] writes a caller-supplied NAME as a `#` header line, so a serving path
//! can refuse a mismatch. The name is NOT part of the mapping ([`CategoryMap::from_text`] ignores
//! it), so an unlabelled file still loads. **Write the two files together, name them together, and
//! move them together.**
//!
//! No file I/O: where the file lives is the caller's decision.

use crate::error::MlError;

/// A category vocabulary: label -> id (its position) and back.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CategoryMap {
    labels: Vec<String>,
}

impl CategoryMap {
    /// Build a map, assigning ids `0..n` in the given order.
    ///
    /// Refuses a duplicate label (two ids meaning one thing) and a label containing a newline (the
    /// text form is line-based, so one would silently split into two categories on reload).
    pub fn from_labels(labels: &[&str]) -> Result<Self, MlError> {
        let mut out: Vec<String> = Vec::with_capacity(labels.len());
        for label in labels {
            if label.contains('\n') || label.contains('\r') {
                return Err(MlError::Shape(format!(
                    "category label {label:?} contains a newline; the text form is line-based"
                )));
            }
            if label.starts_with('#') {
                return Err(MlError::Shape(format!(
                    "category label {label:?} starts with `#`, which the text form reads as a \
                     header comment — it would vanish on reload and shift every later id"
                )));
            }
            if label.is_empty() || label.trim() != *label {
                return Err(MlError::Shape(format!(
                    "category label {label:?} is empty or carries surrounding whitespace; \
                     `from_text` trims and drops blank lines, so it would vanish on reload and \
                     shift every later id"
                )));
            }
            if out.iter().any(|l| l == label) {
                return Err(MlError::Shape(format!("duplicate category label {label:?}")));
            }
            out.push((*label).to_string());
        }
        Ok(Self { labels: out })
    }

    /// The id for a label, or `None` if it is not in the vocabulary.
    ///
    /// An O(n) scan — fine for a few hundred labels, and no hashing dep.
    pub fn code(&self, label: &str) -> Option<u32> {
        self.labels.iter().position(|l| l == label).map(|i| i as u32)
    }

    /// The feature value to put in a row: the id as `f64`, or `NaN` for an unknown label.
    ///
    /// `NaN` is what LightGBM's categorical path treats as missing (module doc).
    pub fn encode(&self, label: &str) -> f64 {
        match self.code(label) {
            Some(c) => f64::from(c),
            None => f64::NAN,
        }
    }

    /// The label an id means, or `None` if the id is out of range.
    pub fn label(&self, code: u32) -> Option<&str> {
        self.labels.get(code as usize).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// A header naming the model this map belongs to, then one label per line in id order.
    ///
    /// The ORDER is the mapping — a reordered file is a different map. `for_model` names the model
    /// artifact it is written beside, so the pair can be checked rather than assumed.
    ///
    /// A newline in `for_model` is replaced with a space (this returns `String`, not `Result`): an
    /// unsanitized one would inject a line `from_text` reads back as id 0, shifting every label.
    pub fn to_text(&self, for_model: &str) -> String {
        let name = for_model.replace(['\n', '\r'], " ");
        let mut out = format!("{HEADER_PREFIX}{name}\n");
        for l in &self.labels {
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    /// The model name a text form was written for, or `None` for an unlabelled (or legacy) file.
    pub fn model_name_of(text: &str) -> Option<&str> {
        text.lines().find_map(|l| l.trim().strip_prefix(HEADER_PREFIX)).map(str::trim)
    }

    /// Parse [`CategoryMap::to_text`]'s output. Blank and `#` lines are skipped; everything else is
    /// a label. The header is metadata, never a category — an unlabelled file loads identically.
    pub fn from_text(text: &str) -> Result<Self, MlError> {
        let labels: Vec<&str> =
            text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        Self::from_labels(&labels)
    }
}

/// The one comment line [`CategoryMap::to_text`] writes and [`CategoryMap::model_name_of`] reads.
const HEADER_PREFIX: &str = "# vike-ml category map for: ";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_start_at_zero_and_follow_declaration_order() {
        let m = CategoryMap::from_labels(&["BTC", "ETH", "SOL"]).unwrap();
        assert_eq!(m.code("BTC"), Some(0));
        assert_eq!(m.code("ETH"), Some(1));
        assert_eq!(m.code("SOL"), Some(2));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn an_unknown_label_encodes_as_nan_which_routes_as_a_missing_category() {
        let m = CategoryMap::from_labels(&["BTC"]).unwrap();
        assert_eq!(m.code("XRP"), None);
        assert!(m.encode("XRP").is_nan());
        assert_eq!(m.encode("BTC"), 0.0);
    }

    #[test]
    fn a_duplicate_label_is_refused_because_it_would_make_two_ids_mean_one_thing() {
        assert!(CategoryMap::from_labels(&["BTC", "ETH", "BTC"]).is_err());
    }

    #[test]
    fn a_label_carrying_a_newline_is_refused_because_the_text_form_is_line_based() {
        assert!(CategoryMap::from_labels(&["BT\nC"]).is_err());
    }

    #[test]
    fn the_text_form_round_trips() {
        let m = CategoryMap::from_labels(&["BTC", "ETH", "SOL"]).unwrap();
        assert_eq!(CategoryMap::from_text(&m.to_text("m1")).unwrap(), m);
    }

    #[test]
    fn the_text_form_is_order_bearing_so_a_reordered_file_is_a_different_map() {
        let a = CategoryMap::from_labels(&["BTC", "ETH"]).unwrap();
        let b = CategoryMap::from_labels(&["ETH", "BTC"]).unwrap();
        assert_ne!(a.to_text("m1"), b.to_text("m1"));
        assert_ne!(a.code("BTC"), b.code("BTC"));
    }

    #[test]
    fn the_text_form_records_which_model_it_belongs_to() {
        // The module doc's wire-format defence: the map SAYS what it was built for.
        let m = CategoryMap::from_labels(&["BTC", "ETH"]).unwrap();
        let text = m.to_text("hl_cohort_h4_fold07");
        assert_eq!(CategoryMap::model_name_of(&text), Some("hl_cohort_h4_fold07"));
        assert_ne!(text, m.to_text("hl_cohort_h4_fold08"), "the name is part of the file");
        assert_eq!(CategoryMap::from_text(&text).unwrap(), m, "...and never part of the mapping");
    }

    #[test]
    fn a_map_with_no_header_still_loads_and_reports_no_name() {
        assert_eq!(CategoryMap::model_name_of("BTC\nETH\n"), None);
        assert_eq!(CategoryMap::from_text("BTC\nETH\n").unwrap().len(), 2);
    }

    #[test]
    fn a_label_that_would_read_back_as_a_comment_is_refused() {
        assert!(CategoryMap::from_labels(&["#BTC"]).is_err());
    }

    #[test]
    fn a_blank_label_is_refused_because_from_text_would_drop_it_and_shift_every_later_id() {
        assert!(CategoryMap::from_labels(&["", "ETH"]).is_err());
        assert!(CategoryMap::from_labels(&[" ", "ETH"]).is_err());
    }

    #[test]
    fn a_label_with_surrounding_whitespace_is_refused_because_it_would_round_trip_to_a_different_label()
     {
        assert!(CategoryMap::from_labels(&[" BTC "]).is_err());
    }

    #[test]
    fn a_newline_in_the_model_name_is_sanitized_rather_than_injecting_a_label_line() {
        let m = CategoryMap::from_labels(&["BTC", "ETH"]).unwrap();
        assert_eq!(CategoryMap::from_text(&m.to_text("m1\nBTC")).unwrap(), m);
    }

    #[test]
    fn a_code_maps_back_to_its_label() {
        let m = CategoryMap::from_labels(&["BTC", "ETH"]).unwrap();
        assert_eq!(m.label(1), Some("ETH"));
        assert_eq!(m.label(9), None);
    }
}
