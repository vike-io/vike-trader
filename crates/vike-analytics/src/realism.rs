//! The REALISM STAMP: the cost model a run actually ran under, recorded on the run rather than
//! inferred from the profile afterwards.
//!
//! # The defect this exists to close
//!
//! `[engine] fee_rate` and `[engine] slippage` are both `#[serde(default)]` and both default to
//! `0.0`, so a profile that simply does not mention them describes a FRICTIONLESS market — every
//! fill at the touch, no commission, no impact. That is a legitimate configuration (it is the right
//! one for isolating a signal from its costs) and it is also what an unfinished profile looks like,
//! and the two produced identical output: a report of eight scalars with nothing on it that says
//! which market the numbers came from. Two runs are comparable only if they ran under the same cost
//! model, and nothing in a run record said what the cost model WAS.
//!
//! Prior art says the same: MetaTrader 5 writes `Model` and `ExecutionMode` into its tester report,
//! and NinjaTrader records the fill resolution. Neither leaves the question to the reader.
//!
//! # RESOLVED values, not the file's text
//!
//! The stamp records what the run resolved, which for the defaulted keys is the interesting half —
//! a key absent from the file still reaches the engine as a number, and that number is what priced
//! the fills. So the producer enumerates the resolved configuration and writes every value,
//! including the ones nobody typed. `vike_backtest::harness::report::realism_stamp` is the one
//! producer, because only a holder of the (feature-gated) `BacktestProfile` knows what resolved.
//!
//! # Why a MAP and not a mirror of `EngineCfg`
//!
//! A typed twin of `EngineCfg` would be a second declaration of the same 30-odd fields, and this
//! workspace has watched every hand copy of a field roster rot — `EngineCfg` grew sixteen fields
//! inside one branch while a doc comment upstream still called its own eight-item list "the whole
//! `[engine]` surface". A `BTreeMap<String, String>` keyed on the TOML path an operator would edit
//! cannot fall behind a struct it does not name, sorts deterministically on the wire, and makes the
//! question this stamp exists for — "did these two runs pay the same costs" — a set comparison
//! ([`RealismStamp::divergence`]) instead of a field-by-field audit somebody has to remember to
//! extend.
//!
//! It costs the type safety, and that is the accepted trade: a stamp is read, diffed and printed,
//! never computed with.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The version of the stamp document. Bumped when the KEY SET changes meaning — a reader comparing
/// two stamps across a bump is comparing two vocabularies, and the schema is what lets it say so
/// rather than reporting every renamed key as a divergence.
pub const REALISM_STAMP_SCHEMA: u32 = 1;

/// One key on which two runs' cost models disagree — the unit [`RealismStamp::divergence`] answers
/// in, because "the stamps differ" is not actionable and "`engine.fee_rate` was 0 there and 0.0004
/// here" is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealismDivergence {
    /// The TOML path, exactly as an operator would edit it (`engine.slippage`).
    pub key: String,
    /// The value the stamp being asked about carried, or `None` when it carried the key not at all
    /// — a real and different answer from a value, because an absent key means the run predates
    /// that knob rather than having set it to zero.
    pub mine: Option<String>,
    /// The value the other stamp carried, on the same terms.
    pub theirs: Option<String>,
}

/// The cost model one run ran under.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealismStamp {
    /// [`REALISM_STAMP_SCHEMA`] at write time; `0` in a document written before the field existed.
    #[serde(default)]
    pub schema: u32,
    /// `TOML path -> resolved value`, sorted. Every key the producer resolved, including the ones
    /// the profile never mentioned — see the module doc.
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    /// `Some(reason)` when the run charged NOTHING for trading — no commission, no slippage, no
    /// impact model. The reason is carried rather than recomputed because it is the sentence an
    /// operator needs and because a reader of an older document can then report the verdict without
    /// knowing which keys the producer of the day consulted.
    ///
    /// ⚠ This is a MEASUREMENT, not a warning that something is wrong. A frictionless run is the
    /// correct configuration for isolating a signal from its costs; what it must not be is
    /// indistinguishable from a costed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frictionless: Option<String>,
}

impl RealismStamp {
    /// Build a stamp from an already-resolved `(key, value)` sweep plus the frictionless verdict.
    ///
    /// The producer passes strings because the values are heterogeneous (floats, bools, counts,
    /// optional names, a symbol map) and the stamp's job is to be READ and DIFFED, never computed
    /// with — see the module doc on why this is not a typed mirror of `EngineCfg`.
    pub fn new(
        values: impl IntoIterator<Item = (String, String)>,
        frictionless: Option<String>,
    ) -> Self {
        RealismStamp {
            schema: REALISM_STAMP_SCHEMA,
            values: values.into_iter().collect(),
            frictionless,
        }
    }

    /// The resolved value of one TOML path, or `None` when this stamp does not carry that key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Every key on which the two stamps disagree, sorted, INCLUDING keys only one of them carries.
    ///
    /// ⚠ **A one-sided key is a divergence, not a skip**, and that is the load-bearing decision
    /// here. The tempting shape is to compare the intersection, which reads as "these two runs
    /// agree" whenever the newer run grew a knob the older one never had — precisely the case where
    /// the answer matters, because the new knob is the thing that changed what the fills cost.
    pub fn divergence(&self, other: &RealismStamp) -> Vec<RealismDivergence> {
        let mut keys: Vec<&String> = self.values.keys().chain(other.values.keys()).collect();
        keys.sort();
        keys.dedup();
        keys.into_iter()
            .filter_map(|k| {
                let mine = self.values.get(k);
                let theirs = other.values.get(k);
                if mine == theirs {
                    return None;
                }
                Some(RealismDivergence {
                    key: k.clone(),
                    mine: mine.cloned(),
                    theirs: theirs.cloned(),
                })
            })
            .collect()
    }

    /// The one line a human table can afford: the fee and slippage an operator asked about first,
    /// plus the frictionless verdict when it applies.
    ///
    /// Keys absent from the stamp render as `-` rather than as `0`, because a stamp written before a
    /// knob existed said nothing about it and printing a zero would be inventing an answer.
    pub fn digest(&self) -> String {
        let fee = self.get("engine.fee_rate").unwrap_or("-");
        let slip = self.get("engine.slippage").unwrap_or("-");
        let schedule = self.get("engine.fee.kind").unwrap_or("-");
        let impact = self.get("engine.impact.model").unwrap_or("-");
        match &self.frictionless {
            Some(why) => format!("FRICTIONLESS — {why}"),
            None => {
                format!("fee_rate={fee} slippage={slip} fee.kind={schedule} impact.model={impact}")
            }
        }
    }
}

impl fmt::Display for RealismStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self.values.keys().map(String::len).max().unwrap_or(0);
        if let Some(why) = &self.frictionless {
            writeln!(f, "FRICTIONLESS RUN — {why}")?;
            writeln!(
                f,
                "  this run's P&L is an upper bound: it is what the strategy would have earned had \
                 trading been free"
            )?;
        }
        for (k, v) in &self.values {
            writeln!(f, "  {k:width$}  {v}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(pairs: &[(&str, &str)], frictionless: Option<&str>) -> RealismStamp {
        RealismStamp::new(
            pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())),
            frictionless.map(str::to_string),
        )
    }

    /// The whole reason a stamp is written: two runs whose fee differs are NOT comparable, and the
    /// divergence names the key an operator would edit.
    #[test]
    fn a_differing_fee_is_reported_by_its_toml_path() {
        let a = stamp(&[("engine.fee_rate", "0.0004"), ("engine.slippage", "0")], None);
        let b = stamp(&[("engine.fee_rate", "0"), ("engine.slippage", "0")], Some("no fee"));

        let d = a.divergence(&b);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].key, "engine.fee_rate");
        assert_eq!(d[0].mine.as_deref(), Some("0.0004"));
        assert_eq!(d[0].theirs.as_deref(), Some("0"));
    }

    /// ⚠ The decision this test exists to hold: a key only ONE side carries is a divergence. The
    /// intersection-only spelling would report "no difference" for exactly the case where a new
    /// knob changed what the fills cost.
    #[test]
    fn a_key_only_one_side_carries_is_a_divergence_not_a_skip() {
        let old = stamp(&[("engine.fee_rate", "0")], None);
        let new = stamp(&[("engine.fee_rate", "0"), ("engine.impact.model", "almgren")], None);

        let d = old.divergence(&new);
        assert_eq!(d.len(), 1, "the new knob must be reported: {d:?}");
        assert_eq!(d[0].key, "engine.impact.model");
        assert_eq!(d[0].mine, None, "an absent key is None, never a fabricated zero");
        assert_eq!(d[0].theirs.as_deref(), Some("almgren"));
    }

    /// Identical cost models diverge on nothing — the answer that makes a diff worth running.
    #[test]
    fn identical_stamps_diverge_on_nothing() {
        let a = stamp(&[("engine.fee_rate", "0.0004"), ("engine.slippage", "0.0001")], None);
        assert!(a.divergence(&a).is_empty());
    }

    /// A stamp is a document on somebody's disk: it must read back out of its own bytes, and an
    /// absent `frictionless` must read as `None` rather than failing the parse.
    #[test]
    fn a_serialized_stamp_reads_back_into_the_same_values() {
        let written = stamp(&[("engine.fee_rate", "0.0004")], None);
        let json = serde_json::to_string(&written).unwrap();
        assert!(!json.contains("frictionless"), "a costed run writes no verdict key: {json}");

        let back: RealismStamp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, written);
        assert_eq!(back.schema, REALISM_STAMP_SCHEMA);
    }

    /// The frictionless verdict survives the round trip, because it is the one line a reader of an
    /// old document needs and cannot re-derive without knowing that producer's key set.
    #[test]
    fn the_frictionless_verdict_survives_the_round_trip() {
        let written = stamp(&[("engine.fee_rate", "0")], Some("no fee_rate, no slippage"));
        let back: RealismStamp =
            serde_json::from_str(&serde_json::to_string(&written).unwrap()).unwrap();
        assert_eq!(back.frictionless.as_deref(), Some("no fee_rate, no slippage"));
        assert!(back.digest().starts_with("FRICTIONLESS"), "{}", back.digest());
    }

    /// An absent key digests as `-`, never as `0`: a stamp written before a knob existed said
    /// nothing about it, and a zero would be an invented answer.
    #[test]
    fn an_absent_key_digests_as_a_dash_rather_than_a_zero() {
        let d = stamp(&[("engine.fee_rate", "0.0004")], None).digest();
        assert!(d.contains("fee_rate=0.0004"), "{d}");
        assert!(d.contains("slippage=-"), "{d}");
    }

    /// An EMPTY stamp is the state of a producer that wrote none, and it must render and diff
    /// without panicking — the `max().unwrap_or(0)` width path.
    #[test]
    fn an_empty_stamp_renders_and_diffs_without_panicking() {
        let empty = RealismStamp::default();
        assert_eq!(empty.to_string(), "");
        assert!(empty.divergence(&empty).is_empty());
        let other = stamp(&[("engine.fee_rate", "0")], None);
        assert_eq!(empty.divergence(&other).len(), 1);
    }
}
