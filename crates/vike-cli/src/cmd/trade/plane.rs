//! **The `trade` PLANE's group layer.** Every verb lives in a group and the group is REQUIRED —
//! there is no bare `vike-cli trade <verb>` — mirroring `crate::cmd::data`'s own group split.
//!
//! ⚠ **THE ONE EXEMPTION, and the rule that licenses it.** `status`, `halt` and `resume` stay at
//! the top level with no group, and that is not a compatibility carve-out: it is the
//! risk-direction law showing up in the grammar. **A verb that takes NO BOOK takes no group**, and
//! all three are node-wide and risk-REDUCING. `halt` under pressure is one word.
//!
//! An unbuilt group is NAMED in the roster and refused BY NAME, never reported as unknown: an
//! operator who types it has read the design, and telling them it is wrong would be false.

use crate::exit::CliError;

/// The plane's groups: name, and the question the group answers.
pub(crate) const GROUPS: &[(&str, &str)] = &[
    ("account", "WHO - which books exist, what is in them, which are LIVE"),
    ("order", "the working set, and the intent to change it"),
    ("position", "what is held"),
    ("strategy", "what trades by itself"),
    ("watch", "the live stream"),
];

/// A group that is designed and not built. ⚠ ONE sentence, spelled once: a per-call-site rewording
/// is how two refusals for one fact come to disagree.
pub(crate) fn unbuilt_group_message(group: &str) -> String {
    format!(
        "`trade {group}` is designed but not built yet - this phase ships `order`, `position` and \
         `strategy`. See docs/superpowers/specs/2026-09-21-trade-cli-surface-design.md and its \
         phase table."
    )
}

/// REPL words that have a one-shot spelling now. ⚠ Refused BY NAME with the replacement: a
/// deprecation that fails without naming its replacement costs a support round trip.
pub(crate) const RETIRED_FLAT_SPELLINGS: &[(&str, &str)] = &[
    ("submit", "trade order submit"),
    ("buy", "trade order submit <book> <symbol> buy"),
    ("sell", "trade order submit <book> <symbol> sell"),
    ("cancel", "trade order cancel"),
    ("modify", "trade order modify"),
    ("mass-cancel", "trade order mass-cancel"),
    ("orders", "trade order ls"),
    ("flatten", "trade position flatten"),
    ("market-exit", "trade position close-all"),
    ("panic", "trade position close-all"),
    ("positions", "trade position ls"),
    ("pos", "trade position ls"),
    ("mount", "trade strategy mount"),
    ("unmount", "trade strategy unmount"),
];

/// A built group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Order,
    Position,
    Strategy,
}

/// Claim the leading token as a group. ⚠ The roster is what every refusal is derived from.
pub(crate) fn claim_group(first: &str) -> Result<Group, CliError> {
    match first {
        "order" => Ok(Group::Order),
        "position" => Ok(Group::Position),
        "strategy" => Ok(Group::Strategy),
        "account" | "watch" => Err(CliError::usage(unbuilt_group_message(first))),
        other => {
            if let Some((_, now)) = RETIRED_FLAT_SPELLINGS.iter().find(|(was, _)| *was == other) {
                return Err(CliError::usage(format!(
                    "`trade {other}` is a REPL word, not a command. One-shot it is `{now}`. Every \
                     trade verb lives in a GROUP ({}) - the plane got too wide to be flat.",
                    group_roster()
                )));
            }
            Err(CliError::usage(format!("unknown `trade` group '{other}' ({})", group_roster())))
        }
    }
}

/// The roster as one string, DERIVED from [`GROUPS`] rather than re-typed. The hand-written copy
/// this shape replaced in `crate::cmd::data` omitted a subcommand that had shipped months earlier.
fn group_roster() -> String {
    GROUPS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn a_built_group_is_claimed() {
        assert_matches!(claim_group("order"), Ok(Group::Order));
        assert_matches!(claim_group("position"), Ok(Group::Position));
        assert_matches!(claim_group("strategy"), Ok(Group::Strategy));
    }

    #[test]
    fn an_unbuilt_group_is_refused_by_name_and_says_it_is_designed() {
        for group in ["account", "watch"] {
            let e = claim_group(group).expect_err("an unbuilt group is refused");
            assert_eq!(
                e.msg,
                unbuilt_group_message(group),
                "it may not re-word the shared sentence"
            );
            assert!(e.msg.contains("designed"), "the refusal must say it is coming: {}", e.msg);
        }
    }

    #[test]
    fn every_group_in_the_roster_is_either_claimed_or_refused_by_name() {
        // The roster is the authority: a group nobody classified would fall into the unknown arm
        // and read as a typo.
        for (name, _) in GROUPS {
            let answer = claim_group(name);
            let refused_by_name =
                answer.as_ref().err().is_some_and(|e| e.msg == unbuilt_group_message(name));
            assert!(answer.is_ok() || refused_by_name, "group '{name}' reads as unknown");
        }
    }

    #[test]
    fn a_repl_word_is_refused_with_its_one_shot_spelling() {
        let e = claim_group("submit").expect_err("a pre-group spelling is refused");
        assert!(e.msg.contains("trade order submit"), "name the replacement: {}", e.msg);
    }

    #[test]
    fn an_unknown_group_names_the_roster() {
        let e = claim_group("nonsense").expect_err("an unknown group is a usage error");
        for (name, _) in GROUPS {
            assert!(e.msg.contains(name), "the refusal must name '{name}': {}", e.msg);
        }
    }

    #[test]
    fn no_retired_spelling_is_also_a_group() {
        for (was, _) in RETIRED_FLAT_SPELLINGS {
            assert!(
                !GROUPS.iter().any(|(g, _)| g == was),
                "'{was}' is both a group and a retired spelling - one of the two is wrong"
            );
        }
    }
}
