//! Plan the folding of stray PER-SYMBOL tick series into their family's GROUPED series.
//!
//! The pure half of the `migrate_to_group` bin: given the store's series list, decide **per kind**
//! whether a fold is unambiguous, and which symbols would move where. The bin does the I/O
//! ([`vike_data::DataFusionHist::migrate_series_to_group`]); everything decided here is arithmetic
//! over names, so the refusal rules are unit-testable.
//!
//! ## Why a refusal rule exists at all
//!
//! The store cannot know which family a stray token belonged to — that knowledge lived in the
//! recorder's in-memory membership map and is not persisted. So a fold is only performed when the
//! answer needs no guess: the venue has **exactly one** grouped series for that kind. With several,
//! picking one would write rows into the wrong family, which no later pass can untangle (the rows
//! carry their symbol, but nothing records which group they came from). That case is reported as
//! [`KindPlan::Ambiguous`] and left for an operator to resolve with an explicit target.

use std::collections::BTreeMap;

/// The tick kinds that have a grouped form. `bar` has no `append_bars_grouped`, so it never folds.
pub const GROUPABLE_KINDS: [&str; 4] = ["quote", "trade", "book", "depth"];

/// What to do with one kind's stray per-symbol series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindPlan {
    /// Exactly one target: fold these symbols into `group`.
    Fold { group: String, symbols: Vec<String> },
    /// Strays exist but the venue has no grouped series of this kind to fold them into.
    NoTarget { symbols: Vec<String> },
    /// Strays exist and several groups could be the target — refuse rather than guess.
    Ambiguous { groups: Vec<String>, symbols: Vec<String> },
}

impl KindPlan {
    /// The symbols this plan concerns, whichever variant it is.
    pub fn symbols(&self) -> &[String] {
        match self {
            KindPlan::Fold { symbols, .. }
            | KindPlan::NoTarget { symbols }
            | KindPlan::Ambiguous { symbols, .. } => symbols,
        }
    }
}

/// Decide the fold for every groupable kind of `venue`, keyed by kind.
///
/// `forced_group` overrides the target for EVERY kind (the operator's answer to an
/// [`KindPlan::Ambiguous`] report) — including kinds that had no group at all, which is what lets a
/// fold into a brand-new group name work.
///
/// Kinds with no strays are absent from the result: there is nothing to say about them.
pub fn plan_regroup(
    series: &[(String, String, String, Option<String>)],
    venue: &str,
    forced_group: Option<&str>,
) -> BTreeMap<String, KindPlan> {
    let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut strays: BTreeMap<&str, Vec<String>> = BTreeMap::new();

    for (kind, v, symbol, group) in series {
        if v != venue {
            continue;
        }
        let Some(kind) = GROUPABLE_KINDS.iter().find(|k| *k == kind) else { continue };
        match group {
            Some(g) => groups.entry(kind).or_default().push(g.clone()),
            None => strays.entry(kind).or_default().push(symbol.clone()),
        }
    }

    let mut out = BTreeMap::new();
    for (kind, mut symbols) in strays {
        symbols.sort();
        symbols.dedup();
        let mut present = groups.remove(kind).unwrap_or_default();
        present.sort();
        present.dedup();

        let plan = match (forced_group, present.len()) {
            (Some(g), _) => KindPlan::Fold { group: g.to_string(), symbols },
            (None, 1) => KindPlan::Fold { group: present.remove(0), symbols },
            (None, 0) => KindPlan::NoTarget { symbols },
            (None, _) => KindPlan::Ambiguous { groups: present, symbols },
        };
        out.insert(kind.to_string(), plan);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(
        kind: &str,
        venue: &str,
        symbol: &str,
        group: Option<&str>,
    ) -> (String, String, String, Option<String>) {
        (kind.into(), venue.into(), symbol.into(), group.map(Into::into))
    }

    #[test]
    fn one_group_per_kind_folds() {
        let series = vec![
            s("book", "polymarket", "aaa", None),
            s("book", "polymarket", "bbb", None),
            s("book", "polymarket", "btc-updown-5m", Some("btc-updown-5m")),
        ];
        let plan = plan_regroup(&series, "polymarket", None);
        assert_eq!(
            plan["book"],
            KindPlan::Fold {
                group: "btc-updown-5m".into(),
                symbols: vec!["aaa".into(), "bbb".into()],
            }
        );
    }

    #[test]
    fn no_group_is_not_a_fold() {
        let series = vec![s("trade", "binance", "BTCUSDT", None)];
        let plan = plan_regroup(&series, "binance", None);
        assert_eq!(plan["trade"], KindPlan::NoTarget { symbols: vec!["BTCUSDT".into()] });
    }

    /// The refusal this module exists for: two candidate families, no way to tell which a stray
    /// token belonged to, so nothing moves.
    #[test]
    fn several_groups_refuse_rather_than_guess() {
        let series = vec![
            s("book", "polymarket", "stray", None),
            s("book", "polymarket", "btc-updown-5m", Some("btc-updown-5m")),
            s("book", "polymarket", "eth-updown-5m", Some("eth-updown-5m")),
        ];
        let plan = plan_regroup(&series, "polymarket", None);
        assert_eq!(
            plan["book"],
            KindPlan::Ambiguous {
                groups: vec!["btc-updown-5m".into(), "eth-updown-5m".into()],
                symbols: vec!["stray".into()],
            }
        );
    }

    #[test]
    fn forced_group_overrides_every_verdict() {
        let series = vec![
            s("book", "polymarket", "stray", None),
            s("book", "polymarket", "a", Some("a")),
            s("book", "polymarket", "b", Some("b")),
            s("trade", "polymarket", "lonely", None), // would be NoTarget
        ];
        let plan = plan_regroup(&series, "polymarket", Some("chosen"));
        for kind in ["book", "trade"] {
            match &plan[kind] {
                KindPlan::Fold { group, .. } => assert_eq!(group, "chosen"),
                other => panic!("{kind}: expected a forced fold, got {other:?}"),
            }
        }
    }

    /// Kinds without a grouped form, other venues, and already-grouped-only kinds all say nothing.
    #[test]
    fn only_groupable_kinds_of_this_venue_appear() {
        let series = vec![
            s("bar", "polymarket", "aaa", None),   // bar has no grouped form
            s("book", "binance", "BTCUSDT", None), // other venue
            s("quote", "polymarket", "g", Some("g")), // grouped already, no strays
        ];
        assert!(plan_regroup(&series, "polymarket", None).is_empty());
    }
}
