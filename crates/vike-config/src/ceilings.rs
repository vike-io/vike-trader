//! [`PRE_TRADE_CEILINGS`] — **every pre-trade ceiling an operator of this workspace can write, the
//! FILE it lives in, and the ACT it judges** — in one table, so that two ceilings sharing a name
//! cannot be read as one.
//!
//! # The defect this replaces
//!
//! MEASURED on the live box: two files carried a ceiling of the same name, kept in step by a
//! comment.
//!
//! ```text
//! settings/policy.toml    max_notional_per_order = 100
//! settings/run-live.toml  [risk] max_notional_per_order = 100.0   # mirrors settings/policy.toml
//! ```
//!
//! They do **not** constrain the same act, and nothing in the tree said so where an operator looks.
//! `vike_mount::MountPolicy::from` drops the `policy.toml` value on the floor — deliberately, with
//! a written reason beside a `_` binding — so it never reaches `vike_exec::RiskLimits` and never
//! judges a strategy-emitted order; it guards three EDGE surfaces instead. The run profile's
//! `[risk]` twin is the only one of the two that reaches `vike_exec::RiskGate`'s
//! `over-max-notional` lane, i.e. every order the core admits from any origin. Every order the
//! policy key touches is ALSO judged by the profile key; the reverse is false.
//!
//! So the two numbers can disagree, and when they do the pick is **silent**: `min(policy, profile)`
//! on the operator-typed lane, `profile ALONE` on the strategy lane, with nothing comparing them at
//! load, at mount or at submit. Lowering the file `vike-cli config show` and the GUI surface
//! tightens what a human may TYPE and changes nothing about what a mounted strategy may SIZE.
//!
//! **This table is not a fix for that — it is the disclosure of it.** Merging the two would be the
//! wrong move and is refused on purpose: they are different gates, and merging two different gates
//! because they share a name is how a ceiling disappears. `crates/vike-mount/src/policy.rs`'s
//! module doc already ruled on the tempting version of that merge (folding
//! `Policy::max_notional_per_order` into `RiskLimits` MOVES `require_live_risk_budget`'s
//! pre-connect refusal, whose operator-facing text names the profile's `[risk]` table) and called
//! it "a decision, not plumbing".
//!
//! # …and the second half, which is stranger
//!
//! `Policy::max_total_exposure` shipped declared-but-unread and was **DELETED for it** — it is the
//! worked example the whole [`crate::consumed`] gate exists because of, and
//! [`crate::policy::PolicyPatch::max_total_exposure`] is now a tombstone that hard-errors the load.
//! The same key is alive, read, mandatory for a live mount and threaded into the pre-trade gate in
//! a run profile's `[risk]` table — the mirror-image defect of the one the field was deleted for:
//! not "shown and dead" but "live and invisible". No `config show` payload covered it, because that
//! command reads the four settings TOMLs and a run profile is not one of them. It has a row here,
//! and `vike-cli config show` renders this table, which is what closes that half.
//!
//! # Why a table and not a comment
//!
//! Exactly the argument [`crate::consumed`] makes one tier down: a `//` comment claiming a ceiling
//! is enforced somewhere is precisely as green when it is false, and this workspace has already
//! shipped that failure twice (`Policy::max_total_exposure`, `Policy::rate.max_utilization`). Each
//! row here names a repo-relative FILE and a NEEDLE for its declaration and for every site that
//! enforces it, and `crates/vike-config/tests/ceilings_are_distinct.rs` opens those files and
//! looks. The relationship itself is DERIVED rather than written: [`shared_names`] computes which
//! names more than one home carries, and the gate then requires those rows to name different acts
//! and **disjoint** enforcement sites. Fold the policy key into `RiskLimits` and both rows point at
//! `crates/vike-exec/src/risk.rs` — the gate reddens, and the author has to state the merge instead
//! of performing it by accident.
//!
//! # …and why a table is not enough either
//!
//! A table describing a relationship is still prose with a test on its spelling: every gate in the
//! paragraph above compares ROWS to ROWS, so an author who folds
//! [`crate::Policy::max_notional_per_order`] into `vike_exec::RiskLimits` and leaves this file
//! alone passes all of them. The commission was to make the RELATIONSHIP itself machine-checked,
//! so three gates read PRODUCTION code rather than this table:
//!
//! - **The drop is behavioural.** `crates/vike-mount/src/policy.rs`'s
//!   `a_policy_per_order_ceiling_reaches_nothing_the_mount_carries` projects two `Policy` values
//!   that differ ONLY in `max_notional_per_order` and requires the resulting `MountPolicy`s to be
//!   EQUAL — and equal to the no-file default. Carrying the field means adding somewhere to carry
//!   it, and the moment that exists the two projections differ. No comment, no needle, no spelling:
//!   the value is either observable on the mount's side of the seam or it is not.
//! - **The deny lanes are a ROSTER, and this table must cover it.**
//!   `ceilings_are_distinct.rs`'s `every_pre_trade_deny_lane_is_named_or_exempt` harvests every
//!   reason `vike_exec::RiskGate` can refuse an order with out of `crates/vike-exec/src/risk.rs`
//!   and requires each to be claimed by a row here or listed with a written reason for why it is
//!   not a size ceiling an operator writes. A NEW pre-trade ceiling lane therefore reddens until
//!   somebody classifies it — the `vike_model::VENUES` playbook, pointed at a second roster.
//! - **The mount refusal has ONE owner.** [`Ceiling::refuses_live_mount_when_absent`] is compared
//!   against the keys `crates/vike-mount/src/arming.rs`'s `require_live_risk_budget` actually
//!   pushes, and no NAME may carry the flag from two homes. That is the one axis on which the two
//!   `max_notional_per_order`s differ in code rather than in prose, so it is the one that can be
//!   held: absence of the profile key stops the box starting live, absence of the policy key
//!   changes nothing at all.
//!
//! ⚠ **Rows are keyed by repo-relative PATH**, so a file MOVE or RENAME invalidates them — the
//! repo-wide trap the root `CLAUDE.md`'s file-move bullet describes, of which this is one more
//! instance. `git grep` the old path before you push; the gate's existence check is the half that
//! catches it.
//!
//! # What this table is NOT
//!
//! It carries no VALUES and resolves no file. A run profile is reached through `--profile` or
//! `VIKE_RUN_PROFILE`, and `vike-cli` deliberately links neither `vike-core` nor `vike-exec` (it
//! takes `vike-ops` with `default-features = false` precisely to stay out of that closure — the
//! `light-consumers` CI lane exists to hold it there), so the disclosure surface can name a run
//! profile's ceilings, their scopes and their absence semantics, and cannot print their numbers.
//! Saying which file holds them, and that this command did not read it, is the honest shape.

/// Which operator-owned file a ceiling is written in.
///
/// The split is the whole point of the table: [`Self::PolicyFile`] is the box's own risk tier (no
/// env layer, no CLI layer — both traits are sealed, so a ceiling here cannot be widened from a
/// shell export), while [`Self::RunProfileRisk`] travels with the RUN you point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CeilingHome {
    /// `<project>/settings/policy.toml`, loaded into [`crate::Policy`].
    PolicyFile,
    /// The `[risk]` table of the run profile `--profile` / `VIKE_RUN_PROFILE` names, loaded into
    /// `vike_exec::ProfileRisk`.
    RunProfileRisk,
}

impl CeilingHome {
    /// How the file is spelled to an operator — what `vike-cli config show` prints.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            CeilingHome::PolicyFile => "policy.toml",
            CeilingHome::RunProfileRisk => "run profile [risk]",
        }
    }

    /// The operator-facing file that documents this home's keys, repo-root-relative.
    ///
    /// Gated: `ceilings_are_distinct.rs` opens it, and for a shared name requires it to name the
    /// OTHER home's file — so the warning at the key cannot quietly stop saying that a twin exists.
    #[must_use]
    pub fn operator_doc(self) -> &'static str {
        match self {
            CeilingHome::PolicyFile => "settings/policy.example.toml",
            CeilingHome::RunProfileRisk => "docs/ops/run-profile-live.toml",
        }
    }

    /// Whether `vike-cli config show`'s settings-file table resolves this home's VALUES.
    ///
    /// `false` for [`Self::RunProfileRisk`] and that is the disclosure gap this table names rather
    /// than hides — see the module doc's last section for why the command does not link the crate
    /// that would parse it.
    #[must_use]
    pub fn value_shown_by_config_show(self) -> bool {
        matches!(self, CeilingHome::PolicyFile)
    }
}

/// One source site a row claims, verified by opening the file and looking for the needle.
///
/// The needle is the thing ITSELF — the deny reason string, the resolver call — never the bare key
/// name, for the reason [`crate::consumed`] states: a field name appears in log lines and doc
/// comments, and neither is enforcement.
#[derive(Debug, Clone, Copy)]
pub struct Site {
    /// Repo-root-relative path. Must exist.
    pub file: &'static str,
    /// Text that must appear in that file.
    pub needle: &'static str,
    /// What this site DOES, in one clause — rendered beside the ceiling.
    pub what: &'static str,
}

/// One pre-trade ceiling: where it is written, what it judges, where that judgement happens.
#[derive(Debug, Clone, Copy)]
pub struct Ceiling {
    /// The key exactly as an operator types it. **Not unique** — that is the finding this table
    /// exists for; [`shared_names`] derives which names collide.
    pub name: &'static str,
    /// The file the key is written in.
    pub home: CeilingHome,
    /// Where the field is declared in code.
    pub declared_at: Site,
    /// **The ACT this ceiling judges** — the column that distinguishes two rows sharing a name.
    pub guards: &'static str,
    /// Every site that actually refuses on this ceiling. EMPTY means nothing enforces it from this
    /// home, in which case [`Self::absent_means`] must say what enforces the concept instead.
    pub enforced_at: &'static [Site],
    /// What an unset value means here. Never "probably uncapped" — the two answers in this table
    /// are genuinely different, and one of them REFUSES a mount.
    pub absent_means: &'static str,
    /// **Whether a LIVE mount REFUSES TO START when this ceiling is absent** — the one axis on
    /// which two same-named ceilings differ in the CODE rather than in prose, and therefore the one
    /// a gate can hold.
    ///
    /// `crates/vike-mount/src/arming.rs`'s `require_live_risk_budget` is the only function in the
    /// workspace that refuses a mount for a missing ceiling, and it keys on the `vike_exec::
    /// RiskLimits` a run profile built. `ceilings_are_distinct.rs` reads the keys it pushes onto
    /// `missing` and requires them to be exactly the rows here carrying `true` — and requires no
    /// NAME to carry it from two homes. So moving that refusal onto `policy.toml`'s twin (the
    /// "decision, not plumbing" `crates/vike-mount/src/policy.rs`'s module doc declines to make
    /// silently) reddens this table instead of landing quietly, and the two spellings cannot stop
    /// being distinguishable here without somebody saying so.
    pub refuses_live_mount_when_absent: bool,
}

impl Ceiling {
    /// Whether anything refuses on this ceiling from this home.
    #[must_use]
    pub fn is_enforced(&self) -> bool {
        !self.enforced_at.is_empty()
    }
}

/// **THE TABLE.** One row per (key, home) pair. Ordered by key, then by home, so the two rows of a
/// shared name are adjacent in every rendering.
///
/// A new operator-writable pre-trade ceiling belongs here the day it lands, in either file.
pub const PRE_TRADE_CEILINGS: &[Ceiling] = &[
    Ceiling {
        name: "max_account_exposure",
        home: CeilingHome::PolicyFile,
        declared_at: Site {
            file: "crates/vike-config/src/policy.rs",
            needle: "pub max_account_exposure",
            what: "declared on `Policy`",
        },
        guards: "one engine's whole projected gross open notional — every symbol of one \
                 (venue, account) plus every live unfilled order. THE account-aggregate axis, and \
                 the one `max_total_exposure`'s name reads as but is not.",
        enforced_at: &[
            Site {
                file: "crates/vike-mount/src/policy.rs",
                needle: "max_account_exposure,",
                what: "CARRIED by `MountPolicy::from` onto `RiskLimits::max_account_exposure`",
            },
            Site {
                file: "crates/vike-exec/src/risk.rs",
                // ⚠ The needle carries `(projected` for a reason, and it is THIS table's own
                // defect caught by its own rule. `"over-account-exposure"` — the bare quoted
                // reason — occurs three times in that file and every one of them is a `//`
                // COMMENT about the lane; the lane itself builds its reason with `format!`, so
                // the closing quote never follows the token. Delete the account lane outright and
                // the row would have stayed green on three comments, which is precisely the
                // "a comment is exactly as green when it is false" failure this table replaced.
                // `every_enforcement_needle_is_matched_by_CODE` is the general cure; this is the
                // needle the cure found.
                needle: "\"over-account-exposure (projected",
                what: "denied by `RiskGate::check_inner`'s account lane",
            },
        ],
        absent_means: "NO LIMIT on this axis — the gate runs no comparison and the producer folds \
                       no book, byte-identical to before the field existed. Deliberately NOT part \
                       of `require_live_risk_budget`'s refusal, so arriving at it cannot stop an \
                       existing deployment from starting.",
        refuses_live_mount_when_absent: false,
    },
    Ceiling {
        name: "max_leverage",
        home: CeilingHome::PolicyFile,
        declared_at: Site {
            file: "crates/vike-config/src/policy.rs",
            needle: "pub max_leverage",
            what: "declared on `Policy`",
        },
        guards: "NOTHING on this box. The key validates, reports as set, and binds no gate — \
                 `MountPolicy::from` deliberately does not carry it.",
        enforced_at: &[],
        absent_means: "Identical to setting it: the concept is enforced from the OTHER home in \
                       this table, through `vike_exec::ProfileRisk::max_leverage` -> \
                       `RiskLimits::im_requirement` -> the buying-power lane. Carrying this field \
                       would clamp every deployment with no `policy.toml` to 1x from a file \
                       nobody wrote — `crates/vike-mount/src/policy.rs`'s module doc argues it.",
        refuses_live_mount_when_absent: false,
    },
    Ceiling {
        name: "max_leverage",
        home: CeilingHome::RunProfileRisk,
        declared_at: Site {
            file: "crates/vike-exec/src/risk_profile.rs",
            needle: "pub max_leverage",
            what: "declared on `ProfileRisk`",
        },
        guards: "the pre-trade BUYING-POWER check for every order this engine admits, by \
                 converting to an initial-margin fraction (`im = 1.0 / max_leverage`) at the \
                 config edge.",
        enforced_at: &[
            Site {
                file: "crates/vike-exec/src/risk_profile.rs",
                needle: "fn im_requirement",
                what: "converted at the config edge onto `RiskLimits::im_requirement`",
            },
            Site {
                file: "crates/vike-exec/src/risk.rs",
                needle: "\"insufficient-margin\"",
                what: "denied by `RiskGate::check_inner`'s buying-power lane",
            },
        ],
        absent_means: "the buying-power lane stays off here; a LIVE mount then applies its own \
                       conservative 1x rescue.",
        refuses_live_mount_when_absent: false,
    },
    Ceiling {
        name: "max_notional_per_order",
        home: CeilingHome::PolicyFile,
        declared_at: Site {
            file: "crates/vike-config/src/policy.rs",
            needle: "pub max_notional_per_order",
            what: "declared on `Policy`",
        },
        guards: "an order a HUMAN types at an EDGE — the desktop's client-side order preview, the \
                 daemon's remote-control socket, and an advisory CLI line. It reaches no \
                 `RiskLimits` and judges NO strategy-emitted order: `MountPolicy::from` drops it \
                 with a written reason.",
        enforced_at: &[
            Site {
                file: "crates/vike-desktop/src/main.rs",
                needle: "OrderLimits::with_max_notional",
                what: "caps the desktop's client-side order preview; the write is dropped before \
                       the wire",
            },
            Site {
                file: "crates/vike-tradehub/src/server.rs",
                needle: "fn notional_reason",
                what: "REFUSES a sized remote control command (`Submit` with a price, `Modify` \
                       with both) before it reaches the core",
            },
            Site {
                file: "crates/vike-cli/src/cmd/verbs.rs",
                needle: "fn guardrail_caps",
                what: "ADVISORY only — prints the over-limit line and sends the order anyway; \
                       nothing branches on the verdict",
            },
        ],
        absent_means: "NO LIMIT at any of the three edges, and no compiled default: the preview's \
                       cap is `f64::INFINITY`, `ControlLimitsConfig::default()` is `None`, and the \
                       advisory prints no cap. Resolved ONCE at startup on the daemon, so a change \
                       lands only on restart.",
        refuses_live_mount_when_absent: false,
    },
    Ceiling {
        name: "max_notional_per_order",
        home: CeilingHome::RunProfileRisk,
        declared_at: Site {
            file: "crates/vike-exec/src/risk_profile.rs",
            needle: "pub max_notional_per_order",
            what: "declared on `ProfileRisk`",
        },
        guards: "EVERY order this engine admits, whatever its origin — strategy-emitted, \
                 control-socket-originated, reconcile-originated. This is the only one of the two \
                 same-named ceilings that reaches the pre-trade gate.",
        enforced_at: &[Site {
            file: "crates/vike-exec/src/risk.rs",
            needle: "\"over-max-notional\"",
            what: "denied by `RiskGate::check_inner`'s per-order notional lane",
        }],
        absent_means: "TWO different answers. On a venue `vike_mount::would_mount_live` reports \
                       live-intent it REFUSES THE MOUNT pre-connect \
                       (`MountError::MissingRiskBudget`) and the daemon exits FAILURE — absent \
                       does not mean uncapped, it means the box does not start live. On every \
                       other mount (paper, backtest) it is simply no ceiling.",
        refuses_live_mount_when_absent: true,
    },
    Ceiling {
        name: "max_total_exposure",
        home: CeilingHome::RunProfileRisk,
        declared_at: Site {
            file: "crates/vike-exec/src/risk_profile.rs",
            needle: "pub max_total_exposure",
            what: "declared on `ProfileRisk`",
        },
        guards: "ONE SYMBOL at ONE VENUE — the projected open notional of the order's own symbol \
                 at this engine's venue, NOT the account, NOT cross-symbol, NOT cross-venue, \
                 despite the name. An operator who sized it for a book of N symbols has an N-times \
                 weaker cap than they wrote; the ACCOUNT axis is `max_account_exposure`, above.",
        enforced_at: &[Site {
            file: "crates/vike-exec/src/risk.rs",
            needle: "\"over-max-exposure\"",
            what: "denied by `RiskGate::check_inner`'s per-symbol exposure lane",
        }],
        absent_means: "Same split as its sibling above, and this is the load-bearing one: on a \
                       live-intent venue it is one of exactly TWO keys \
                       `require_live_risk_budget` demands, so absence REFUSES the mount before any \
                       venue session exists. A compiled default was deliberately not chosen. On a \
                       paper or backtest mount, absence is no ceiling.",
        refuses_live_mount_when_absent: true,
    },
];

/// Every row carrying `name`, in table order.
pub fn ceilings_named(name: &str) -> impl Iterator<Item = &'static Ceiling> + '_ {
    PRE_TRADE_CEILINGS.iter().filter(move |c| c.name == name)
}

/// **The relationship, DERIVED** — every ceiling name that more than one home carries.
///
/// Computed from [`PRE_TRADE_CEILINGS`] rather than written down, so it cannot disagree with the
/// table; `crates/vike-config/tests/ceilings_are_distinct.rs` is what holds each returned name to
/// naming different acts at disjoint enforcement sites, and `vike-cli config show` renders the
/// warning from this call. Returned in table order, deduplicated.
#[must_use]
pub fn shared_names() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for c in PRE_TRADE_CEILINGS {
        if out.contains(&c.name) {
            continue;
        }
        if PRE_TRADE_CEILINGS.iter().filter(|o| o.name == c.name).count() > 1 {
            out.push(c.name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derivation answers from the TABLE, not from a written list — add a second home for a
    /// name and it appears here with no edit.
    #[test]
    fn a_name_carried_by_two_homes_is_derived_as_shared() {
        let shared = shared_names();
        assert!(shared.contains(&"max_notional_per_order"), "{shared:?}");
        assert!(shared.contains(&"max_leverage"), "{shared:?}");
        assert!(
            !shared.contains(&"max_total_exposure"),
            "policy.toml cannot carry this name today — `PolicyPatch::max_total_exposure` is a \
             tombstone that hard-errors the load, so exactly one home declares it: {shared:?}"
        );
        assert!(!shared.contains(&"max_account_exposure"), "{shared:?}");
    }

    /// Every shared name really does carry two DIFFERENT homes — the property that makes the word
    /// "shared" mean what the renderer says it means.
    #[test]
    fn a_shared_name_spans_two_homes() {
        for name in shared_names() {
            let mut homes: Vec<CeilingHome> = ceilings_named(name).map(|c| c.home).collect();
            homes.sort_unstable();
            homes.dedup();
            assert!(homes.len() > 1, "{name} repeats within one home rather than spanning two");
        }
    }

    /// Rows are ordered so a rendering puts the two halves of a shared name side by side. Without
    /// this the disclosure is two lines a screen apart, which is how the defect read on the box.
    #[test]
    fn rows_are_grouped_by_name_and_ordered_by_home() {
        let mut seen: Vec<&str> = Vec::new();
        for w in PRE_TRADE_CEILINGS.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            if a.name == b.name {
                assert!(a.home < b.home, "{}: keep a name's homes in enum order", a.name);
            } else {
                assert!(!seen.contains(&b.name), "{} is split across the table", b.name);
                seen.push(a.name);
                assert!(
                    a.name < b.name,
                    "{} then {}: keep the table sorted by key",
                    a.name,
                    b.name
                );
            }
        }
    }
}
