//! The stable string↔integer table for a categorical feature.
//!
//! # Why this has to exist
//!
//! The data file [`crate::train::TrainData::write_csv`] hands LightGBM carries NUMBERS, so a
//! category was already an integer before the trainer ever saw it, and the saved model has no
//! record of what that integer MEANT — the `pandas_categorical:` trailer some model files carry is
//! written by LightGBM's PYTHON wrapper, and a CLI-trained model ends in `pandas_categorical:null`.
//! Nothing in the model file will give the mapping back.
//!
//! So the caller must keep it, and this is the shape it keeps it in: ids assigned from 0 in
//! declaration order, serialized as one label per line, saved beside the model artifact.
//!
//! # Two properties the walker depends on
//!
//! * **Ids are never negative.** A negative category is LightGBM's "missing" and routes right
//!   unconditionally, so a scheme that assigned -1 to anything would silently lose it.
//! * **An UNKNOWN label encodes as `NaN`, not as a fallback id.** NaN is exactly what a categorical
//!   node treats as missing (right, unconditionally), which is the honest answer for a category the
//!   model never saw. Mapping it onto an existing id would predict as if it were a different
//!   category, and nothing would say so.
//!
//! # ⚠ This is a WIRE FORMAT between a training run and a serving binary, and it has no version
//!
//! The map and the model it belongs to are two separate files with nothing linking them. Pairing
//! the wrong two is silent and TOTAL — every category means a different thing, every prediction is
//! wrong, and no error fires anywhere — which is a worse failure than the model file's own
//! `version=` skew, and unlike that one it has no marker at all.
//!
//! The minimum defence, and what is implemented here: [`CategoryMap::to_text`] takes a
//! caller-supplied NAME and writes it as a `#` header line, so the file at least states what it was
//! built for and a serving path can refuse a mismatch instead of predicting confidently. The name
//! is deliberately NOT part of the mapping — [`CategoryMap::from_text`] ignores it — so an
//! unlabelled legacy file still loads.
//!
//! **Write the two files together, name them together, and move them together.** A model artifact
//! without its category map is not servable, and a category map without its model is not
//! interpretable; there is no third file that could reconcile them after the fact.
//!
//! # No file I/O
//!
//! [`CategoryMap::to_text`] and [`CategoryMap::from_text`] are pure. Where the file lives is the
//! caller's decision, and this crate does not have an opinion about it.

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
    /// An O(n) linear scan, not a hash lookup — fine at the sizes a categorical vocabulary reaches
    /// here (a handful to a few hundred labels), and it keeps this type free of a hashing dep.
    pub fn code(&self, label: &str) -> Option<u32> {
        self.labels.iter().position(|l| l == label).map(|i| i as u32)
    }

    /// The feature value to put in a row: the id as `f64`, or `NaN` for an unknown label.
    ///
    /// `NaN` is not a sentinel invented here — it is what LightGBM's categorical path treats as
    /// missing, so an unseen category routes the way the model was trained to route one.
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
    /// The ORDER is the mapping — a reordered file is a different map, and a model trained against
    /// one is wrong when served with the other. `for_model` is whatever the caller calls the model
    /// artifact it is writing beside; it exists so the pair can be checked rather than assumed (see
    /// this module's wire-format warning).
    ///
    /// A newline in `for_model` is replaced with a space rather than refused — this method returns
    /// `String`, not `Result`, so it sanitizes instead: an unsanitized newline would inject a bare
    /// line `from_text` reads back as category id 0, shifting every real label.
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
        // The map and the model are two files with no other link between them. Pairing the wrong
        // two is silent and total — every category means a different thing — so the map at least
        // SAYS what it was built for, and a serving path can refuse a mismatch.
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
