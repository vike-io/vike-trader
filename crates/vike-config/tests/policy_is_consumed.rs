//! The [`Policy`] consumption gate — every hard ceiling this machine DECLARES must be read by
//! something that enforces it.
//!
//! `Policy` is the settings system's risk tier: the one type with no env layer and no CLI layer,
//! whose whole purpose is that a ceiling written here cannot be quietly widened. That guarantee is
//! worth exactly nothing if the ceiling is never READ — an operator who writes a limit into a
//! `policy` row (`vike-cli config set policy.<key> <value>`), watches it validate (a negative value
//! is rejected, a typo'd key is rejected by name), and gets no complaint, reasonably believes the
//! limit is armed.
//!
//! That is not hypothetical. `max_total_exposure` shipped as a `Policy` field, was validated by
//! `Policy::apply`, was accepted by `PolicyPatch`'s `deny_unknown_fields`, and was **read by
//! nothing** — while `crates/vike-mount/src/policy.rs`'s module doc stated in as many words that it
//! and `max_notional_per_order` "ARE consumed, but at the ORDER surfaces". Only the second half of
//! that sentence was ever true. The field was removed (see `vike_config::policy`'s tombstone in
//! `PolicyPatch`); this gate is what stops the next one.
//!
//! # Why a table of verified claims, and not a comment
//!
//! `vike_mount::MountPolicy::from` already destructures `Policy` exhaustively so a new field cannot
//! be silently dropped — an excellent gate for *"was this field considered?"* and no gate at all for
//! *"is this field consumed?"*. Its answer lives in a `//` comment beside a `_` binding, and a
//! comment that says "consumed at the order surfaces" is exactly as green when it is false.
//!
//! So each field's row here names a FILE and a NEEDLE, and [`every_claimed_consumer_really_reads_it`]
//! opens that file and looks. A row can no longer be wrong in the direction that matters: claiming
//! a ceiling is enforced when it is not turns the gate red, and so does deleting the consumer later.
//!
//! # The four directions
//!
//! 1. [`every_policy_field_has_a_row`] — the row set and the real field set agree. The
//!    EXHAUSTIVE destructure in [`policy_field_names`] is the compile-time half: a new `Policy`
//!    field breaks that line, and the author has to state where it is consumed or admit that it is
//!    not.
//! 2. [`every_claimed_consumer_really_reads_it`] — every [`Consumed::At`] row's file exists and
//!    contains its needle. This is the direction that would have caught `max_total_exposure`, and
//!    it is also a ratchet: deleting the last consumer of a ceiling fails here rather than leaving
//!    a dead field behind.
//! 3. [`a_claimed_consumer_is_outside_the_settings_crate`] — a needle inside `crates/vike-config/`
//!    does not count.
//! 4. [`an_unconsumed_field_is_really_unconsumed`] — every [`Consumed::No`] row is honest in the
//!    other direction; a field admitted as unconsumed that has since gained a real read must be
//!    upgraded to an `At` row rather than keeping its excuse.
//!
//! ## Why direction 3 was added LATER, and what it caught
//!
//! This gate shipped with directions 1, 2 and 4. Its younger twin for the other three settings
//! types (`settings_are_consumed.rs`) shipped with a fourth rule this one lacked — a claimed
//! consumer must live OUTSIDE the declaring crate — for a reason that applies here word for word:
//! `vike-config` parses, validates, clamps and serializes every field, so if that counted, every
//! row could claim `At` and the table would assert nothing.
//!
//! And one row was already through the hole. `Policy::rate` claimed a consumer at
//! `crates/vike-config/src/load.rs`'s `self.policy.rate.max_utilization` — the clamp that bound
//! `Preferences::rate_utilization`. That preference was read by NOTHING (every pacer takes its
//! fraction from the compiled-in `vike_model::rate_limits::DEFAULT_UTILIZATION`), so the ceiling
//! bounded a dead value while this gate reported it enforced: the `max_total_exposure` defect,
//! one field over, hiding behind a self-read. Both halves are tombstones now, and this rule is
//! what stops the next one arriving the same way.
//!
//! ⚠ Directions 2 and 3 are separate tests on purpose, not one loop with two asserts. They fail
//! for different reasons and want different fixes — 2 says "this read is gone, find the real one",
//! 3 says "this is not a read at all" — and a single test would report whichever fired first.
//!
//! # What counts as a read: PRODUCTION code, by the scanner the settings gate proved
//!
//! Directions 2 and 4 read the tree through `crates/vike-config/tests/common/scanner.rs`'s
//! `production_lines` — the one notion of production code this gate now SHARES with
//! `settings_are_consumed.rs`: comment and doc lines dropped, every `#[cfg(test)] mod` (inline,
//! or a `#[cfg(test)] mod name;` sibling FILE) left out, and integration tests, benches and
//! examples left out.
//!
//! ⚠ Until that change this gate was the WEAKER twin while guarding the HARD CEILINGS. Direction 2
//! did a raw `source.contains(needle)`, so a ceiling whose only "read" sat in a comment, a doc or
//! the claimed file's own test module passed as enforced; direction 4 skipped `//` and `*` lines
//! and nothing else. The settings gate had already measured that exact hole on
//! `config.tradehub_addr` (both mutations left it green). These are the proofs this gate never
//! had that its searches can REJECT:
//! [`a_claimed_read_in_a_comment_or_a_test_module_does_not_count`],
//! [`a_claimed_consumer_that_is_a_test_file_does_not_count`],
//! [`an_unconsumed_fields_reader_in_a_comment_or_a_test_module_is_not_a_reader`] and
//! [`every_at_row_goes_red_when_its_read_is_commented_out_or_moved_into_a_test_module`].

use std::collections::BTreeSet;
use std::path::Path;

use vike_config::Policy;
use vike_model::libm_walk::cfg_test_module_files_under;

// Shared with `crates/vike-config/tests/settings_are_consumed.rs`: ONE notion of production code
// for both consumption gates, rather than a weaker copy guarding the stronger claims.
#[path = "common/scanner.rs"]
mod scanner;
#[path = "common/workspace.rs"]
mod workspace;

use scanner::{contains_code_in, is_test_path, production_lines, rel_path, rust_sources};
use workspace::workspace_root;

/// The declaring crate. A read here is the settings machinery reading itself — see the module doc's
/// direction 3, and `settings_are_consumed.rs`, which has enforced the same rule since it shipped.
const SELF_CRATE: &str = "crates/vike-config/";

/// What consumes one [`Policy`] field.
#[derive(Debug)]
enum Consumed {
    /// A real consumer. `file` is repo-root-relative and must EXIST; `needle` must appear in it.
    ///
    /// The needle is the READ ITSELF, not the field name — `settings.policy.market_slippage`
    /// appears in two `tracing` macros that log the resolved policy, and a log is not enforcement.
    At { file: &'static str, needle: &'static str },
    /// Declared and deliberately NOT consumed. `why` must say what enforces the concept instead,
    /// because "nothing does" is the answer this whole gate exists to make impossible to ship
    /// silently.
    No { why: &'static str },
}

/// One row per [`Policy`] field. Keep it in the struct's own declaration order.
const POLICY_CONSUMERS: &[(&str, Consumed)] = &[
    // The mount's leverage authority is `vike_exec::ProfileRisk::max_leverage` -> the
    // `RiskLimits::im_requirement` rescue, which the buying-power lane in `RiskGate::check_inner`
    // actually evaluates. `Policy::max_leverage` is NOT carried into the mount on purpose:
    // its policy default is `1.0`, so carrying it would clamp every deployment with no
    // `policy` rows to 1x — a behaviour change on upgrade, from a row nobody wrote. Reconciling
    // the two authorities is its own phase; see `crates/vike-mount/src/policy.rs`'s module doc.
    (
        "max_leverage",
        Consumed::No {
            why: "leverage is enforced through vike_exec::ProfileRisk::max_leverage -> \
                  RiskLimits::im_requirement (the buying-power lane in RiskGate::check_inner). \
                  This field's 1.0 default would clamp every policy-file-less deployment to 1x, \
                  so the mount deliberately does not carry it — see crates/vike-mount/src/policy.rs",
        },
    ),
    // Phase 5's real wiring: the three order surfaces each read this and cap an order with it.
    // The desktop shell's order-entry preview is the one named here; vike-cli's `lib.rs` and
    // vike-tradehub's `main.rs` read it identically.
    //
    // ⚠ Re-pointed `crates/vike-app` → `crates/vike-desktop` with the GUI shell's rename. Only the
    // PATH moved: the read is the same `resolve_policy` fold it always was, and it survived the
    // desktop's local-core deletion because this ceiling caps the ORDER PREVIEW the shell still
    // draws, not a mount it no longer performs.
    (
        "max_notional_per_order",
        Consumed::At {
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings.policy.max_notional_per_order",
        },
    ),
    // The ACCOUNT-aggregate exposure ceiling — the first `Policy` ceiling to reach the pre-trade
    // GATE itself. Carried by `MountPolicy::from` and folded onto `vike_exec::RiskLimits` at the
    // end of `make_engine_for_account`, where `RiskGate::check_inner`'s `over-account-exposure`
    // lane then evaluates it on every order.
    //
    // ⚠ The needle is the WHOLE fold, `policy.and_then(..)` included, the same strengthening
    // `halt_admit` and `venues` below carry and for the same measured reason. A needle of
    // `narrow_account_exposure` alone stays green under the one mutation that matters here —
    // passing `None`, or passing a hardcoded number — i.e. a ceiling the operator wrote, the field
    // set, and the file's value nowhere in it. Naming the source is what makes this row assert that
    // the OPERATOR's number is the one that arrives.
    //
    // ⚠ The fold is `RiskLimits::narrow_account_exposure` (a `min`), not an assignment, so this
    // needle also pins the shape that makes the ceiling refuse-only structurally.
    (
        "max_account_exposure",
        Consumed::At {
            file: "crates/vike-mount/src/engine/assemble.rs",
            needle: "narrow_account_exposure(policy.and_then(|p| p.max_account_exposure))",
        },
    ),
    // The ceiling on the EQUITY FIGURE the sizing and admission lanes may see — the cap that
    // decouples this deployment's position sizes from a venue wallet a third party can move.
    // Carried by `MountPolicy::from` and folded onto `vike_exec::RiskLimits` at the end of
    // `make_engine_for_account`, where `ExecutionEngine::sizing_equity` then applies it to every
    // strategy context and to the pre-trade gate's margin lane.
    //
    // ⚠ The needle is the WHOLE fold, `policy.and_then(..)` included, for the measured reason the
    // sibling above records: `narrow_sizing_equity` alone stays green under the one mutation that
    // matters — passing `None`, or a hardcoded number — i.e. a ceiling the operator wrote, the
    // field set, and the file's value nowhere in it. Naming the source is what makes this row
    // assert that the OPERATOR's number is the one that arrives. The fold is a `min`, not an
    // assignment, so the needle also pins the shape that makes the ceiling lower-only structurally.
    //
    // ⚠ This row deliberately does NOT prove the half that would hurt most if it broke — that the
    // ceiling reaches the SPENDING consumers and NOT the margin-call sweep. A text needle cannot;
    // `crates/vike-core/tests/wiring/sizing_equity_ceiling.rs` drives both through a real core.
    (
        "max_sizing_equity",
        Consumed::At {
            file: "crates/vike-mount/src/engine/assemble.rs",
            needle: "narrow_sizing_equity(policy.and_then(|p| p.max_sizing_equity))",
        },
    ),
    // Carried by `MountPolicy::from` into `make_engine` and on to the one venue with no native
    // market order (hyperliquid), which prices an emulated market/stop-market inside this band.
    (
        "market_slippage",
        Consumed::At {
            file: "crates/vike-mount/src/policy.rs",
            needle: "market_slippage: *market_slippage",
        },
    ),
    // ⚠ The needle is `.with_halt_admit(halt_admit)`, WITH its argument, and that was a correction
    // rather than a first draft: a needle of `.with_halt_admit(` alone stayed green under a
    // MEASURED mutation that replaced the operator's value with a hardcoded `HaltAdmit::Admit` —
    // the setter called, the policy ignored, the gate satisfied. Naming the variable is what makes
    // this row assert that the operator's value is the one that arrives.
    //
    // Carried by `MountPolicy::from`, through `make_engine_for_account`'s own `halt_admit` local,
    // into the one venue
    // whose bridge can act on it: cTrader, the only adapter holding a position book at its halt
    // boundary. ⚠ Decision 0088 B5 moved the setter call itself out of `crates/vike-mount/src/lib.rs`
    // and into `crates/bridges/ctrader/src/mount.rs`'s `live_mount_for_account` — and since the
    // venue mount contract `CtraderVenueMount::mount` passes `MountRequest::halt_admit` down to it
    // as a plain value — so the needle moved with it. The needle stays the
    // setter call, not the field name and not the `MountPolicy` projection, because carrying a value
    // is not enforcing it (the distinction `max_total_exposure` was removed for).
    (
        "halt_admit",
        Consumed::At {
            file: "crates/bridges/ctrader/src/mount.rs",
            needle: ".with_halt_admit(halt_admit)",
        },
    ),
    // The per-venue arming CEILINGS — stage 3, the fold that made this row an `At`. It was a
    // written `Consumed::No` for the whole of stage 2 ("NOTHING READS IT YET"), which is what made
    // the promotion unmissable rather than something to remember.
    //
    // ⚠ The needle is the WHOLE resolution, `map_or` and fail-safe default included, and that is a
    // deliberate strengthening rather than verbosity. A needle of `p.venue_mode(venue)` alone stays
    // green under the one mutation that matters most here — `map_or(VenueMode::Live, …)`, which
    // makes a caller that threads no policy arm every venue its credentials permit, i.e. the exact
    // defect the ceiling exists to close, restored by one word. Naming `VenueMode::Paper` in the
    // needle is what makes this row assert that the SAFE end is the default. Same correction, and
    // the same reason, as `halt_admit`'s needle carrying its argument.
    //
    // It is deliberately NOT a needle in `crates/vike-config/src/policy.rs`: direction 3 rejects
    // one inside the declaring crate, and it is right to — parsing a field is not consuming it.
    //
    // ⚠ The needle moved from `p.venue_mode(venue)` to `p.venues.account(venue, label)`, and that is
    // a STRENGTHENING of the same kind. The `[accounts]` sub-table lives inside this field, so this
    // is the row that has to prove it binds; `account` resolves the venue ceiling exactly when the
    // label is the default one, so the needle still asserts everything it asserted before AND that
    // the per-account fold is the one the mount reaches. A needle left at `venue_mode` would go
    // green over a mount that had quietly stopped consulting a second account's line.
    (
        "venues",
        Consumed::At {
            file: "crates/vike-mount/src/arming.rs",
            needle: "map_or(vike_config::VenueMode::Paper, |p| p.venues.account(venue, label))",
        },
    ),
    // The dead-man switch — constructed by ONE composition root, `vike-tradehub`'s live mount, as
    // `vike_core::CoreConfig::deadman`. The needle is the CALL SITE inside `live_mount_with`, with
    // its argument: `deadman_config_from_policy` is the pure fold from the two policy keys to the
    // core's config (its own unit tests pin absent → `None`, `0` → `None`, a written 60 s → armed
    // and halting, and the action mapping; for one morning the first of those read "default →
    // 60 s / halt", and `Policy::deadman_timeout_ms`'s doc records the reversal — the needle did
    // not move, because the READ did not), and naming the call beside the `deadman:` field is
    // what asserts the fold's result
    // is what reaches the core — the same strengthening as `halt_admit`'s needle carrying its
    // argument. A needle on the field read alone would stay green if the function were still
    // defined and no longer called.
    //
    // ⚠ Deliberately NOT the `paper_mount` arm, and not `vike-desktop`: neither constructs it, and the
    // field's own doc says why the gate-off paper arm does not — and that the live GATE, not exec
    // arming, is what enters `live_mount_with`. `docs/ops/kill-switches.md` states the residuals.
    (
        "deadman_timeout_ms",
        Consumed::At {
            file: "crates/vike-tradehub/src/tradehub_cli/live_mount.rs",
            needle: "deadman: deadman_config_from_policy(policy)",
        },
    ),
    // The action rides the same construction. Its needle is the READ itself inside that function —
    // the point where the file spelling becomes `vike_core::DeadManAction` — rather than the call
    // site a second time, so this row fails independently if the mapping is dropped and the
    // timeout is wired alone (a switch that trips and does the wrong thing).
    (
        "deadman_action",
        Consumed::At {
            file: "crates/vike-tradehub/src/venue_arming/deadman.rs",
            needle: "policy.deadman_action.to_core()",
        },
    ),
    // The LINK dead-man's grace (M13) — the second switch, constructed by the SAME composition
    // root and by nothing else. The needle is the call site inside `live_mount_with` with its
    // argument, for the same strengthening reason the row above carries one: `link_deadman_config_
    // from_policy` folds this key together with `vike_model::link_deadman_default` and the mounted
    // venue set, and naming the call beside the `link_deadman:` field is what asserts the fold's
    // RESULT reaches the core. A needle on the field read alone would stay green if the function
    // were still defined and no longer called — which is exactly the shape `max_total_exposure`
    // shipped in.
    //
    // ⚠ This key's default is ARMED, so a dropped call site is not "a switch nobody turned on" but
    // "a switch that reported itself on and is not" — a strictly worse failure than the sibling's,
    // and the reason this row exists rather than an admission that the core has a default.
    (
        "link_deadman_grace_ms",
        Consumed::At {
            file: "crates/vike-tradehub/src/tradehub_cli/live_mount.rs",
            needle: "link_deadman: link_deadman_config_from_policy(policy, &link_venues)",
        },
    ),
    // ⚠ `rate` stood here, claiming `crates/vike-config/src/load.rs`'s
    // `self.policy.rate.max_utilization` — the clamp onto `Preferences::rate_utilization`. That was
    // a SELF-read, which direction 3 now rejects, and the preference it clamped was consumed by
    // nothing, so the ceiling bounded a dead value. The field is a refused tombstone
    // (`vike_config::PolicyPatch::rate`) and the row is gone with it.
];

/// The real field set, taken from the type itself.
///
/// ⚠ The destructure is EXHAUSTIVE on purpose — do NOT add `..`. A new `Policy` field must break
/// this line and be given a row in [`POLICY_CONSUMERS`], the same way `vike_model::VENUES` forces
/// a row in every per-venue capability table.
fn policy_field_names() -> Vec<&'static str> {
    let Policy {
        max_leverage: _,
        max_notional_per_order: _,
        max_account_exposure: _,
        max_sizing_equity: _,
        market_slippage: _,
        halt_admit: _,
        deadman_timeout_ms: _,
        deadman_action: _,
        link_deadman_grace_ms: _,
        venues: _,
    } = Policy::default();
    vec![
        "max_leverage",
        "max_notional_per_order",
        "max_account_exposure",
        "max_sizing_equity",
        "market_slippage",
        "halt_admit",
        "deadman_timeout_ms",
        "deadman_action",
        "link_deadman_grace_ms",
        "venues",
    ]
}

#[test]
fn every_policy_field_has_a_row() {
    let mut fields = policy_field_names();
    let mut rows: Vec<&str> = POLICY_CONSUMERS.iter().map(|(f, _)| *f).collect();
    fields.sort_unstable();
    rows.sort_unstable();
    assert_eq!(
        fields, rows,
        "POLICY_CONSUMERS and the real Policy fields disagree.\n\
         A new ceiling needs a row saying WHERE it is consumed (Consumed::At {{ file, needle }}) \
         or an explicit admission that it is not (Consumed::No {{ why }}).\n\
         A removed ceiling needs its row deleted."
    );
}

/// THE direction that catches a declared-but-unenforced ceiling: a row may claim a consumer, and
/// this opens the file and checks.
///
/// ⚠ The claimed file must be PRODUCTION source and the needle must sit in its PRODUCTION lines —
/// [`rejected_claim`] is the whole decision, shared with the proofs below so they exercise the
/// gate's own code rather than a copy of it.
#[test]
fn every_claimed_consumer_really_reads_it() {
    let root = workspace_root();
    let production = production_sources(&root);
    let mut failures = Vec::new();

    for (field, consumed) in POLICY_CONSUMERS {
        let Consumed::At { file, needle } = consumed else {
            continue;
        };
        let path = root.join(file);
        let source = std::fs::read_to_string(&path).ok();
        match rejected_claim(file, needle, source.as_deref(), &production) {
            None => {}
            Some(Rejection::Missing) => failures.push(format!(
                "Policy::{field} claims a consumer in `{file}`, but that file does not exist \
                 (looked in {}). Point the row at the real consumer, or downgrade it to \
                 Consumed::No with a reason.",
                path.display()
            )),
            Some(Rejection::TestFile) => failures.push(format!(
                "Policy::{field} claims a consumer in `{file}`, and that file is not a \
                 PRODUCTION source under `crates/`: it is TEST code — an integration test, bench \
                 or example, or the body of a `#[cfg(test)] mod name;` declared in its parent. No \
                 shipped build compiles it, so the ceiling binds nothing an operator can run. \
                 Point the row at the production read."
            )),
            Some(Rejection::NotInProduction) => failures.push(format!(
                "Policy::{field} is DECLARED but NOT CONSUMED.\n  \
                 The row claims `{file}` reads it as `{needle}`, and that text is not in the \
                 file's PRODUCTION code (a match inside a comment, a doc or a `#[cfg(test)] mod` \
                 does not count — the ceiling would be unread in every shipped build).\n  \
                 A ceiling nothing reads is a limit the operator believes they set and does not \
                 have: a `policy` row validates the value and `deny_unknown_fields` accepts the \
                 key, so nothing tells them otherwise.\n  \
                 Fix it by ENFORCING the field (wire it to a real read and point the needle at \
                 it) or by DELETING it (remove the field, and every doc claiming it is \
                 enforced)."
            )),
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// Why a [`Consumed::At`] claim does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejection {
    /// The claimed file is not there.
    Missing,
    /// The claimed file exists and is test code, so nothing it says is a production read.
    TestFile,
    /// The needle is absent from the claimed file's production lines (it may still sit in a
    /// comment, a doc or a `#[cfg(test)] mod`, which is exactly what does not count).
    NotInProduction,
}

/// THE decision of direction 2 for one claim: `None` when `file` is a production source whose
/// production lines hold `needle`. `source` is the claimed file's text (`None` = unreadable).
fn rejected_claim(
    file: &str,
    needle: &str,
    source: Option<&str>,
    production: &BTreeSet<String>,
) -> Option<Rejection> {
    let Some(source) = source else {
        return Some(Rejection::Missing);
    };
    if !production.contains(file) {
        return Some(Rejection::TestFile);
    }
    (!contains_code_in(source, needle)).then_some(Rejection::NotInProduction)
}

/// Every PRODUCTION `.rs` file under `crates/`, repo-relative: the shared scanner's
/// [`rust_sources`] (which already leaves out each `#[cfg(test)] mod name;` sibling file) minus
/// integration tests, benches and examples ([`is_test_path`]).
fn production_sources(root: &Path) -> BTreeSet<String> {
    rust_sources(&root.join("crates"))
        .iter()
        .map(|p| rel_path(root, p))
        .filter(|rel| !is_test_path(rel))
        .collect()
}

/// A needle inside `crates/vike-config/` is the settings system reading itself, which every field
/// gets for free from `apply`/`serialize`. Without this rule the gate above can be satisfied by
/// pointing at the loader — which is exactly how `Policy::rate` passed while its ceiling bounded a
/// value nothing consumed. Verbatim from `settings_are_consumed.rs`, which had it from day one.
#[test]
fn a_claimed_consumer_is_outside_the_settings_crate() {
    let offenders: Vec<&str> = POLICY_CONSUMERS
        .iter()
        .filter_map(|(field, consumed)| match consumed {
            Consumed::At { file, .. } if file.replace('\\', "/").starts_with(SELF_CRATE) => {
                Some(*field)
            }
            _ => None,
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "these Policy ceilings claim a consumer inside the DECLARING crate, which is not \
         enforcement — parsing, validating, clamping and serializing a field is what vike-config \
         does to EVERY field, so a self-read proves nothing about whether the ceiling binds \
         anything: {offenders:?}"
    );
}

/// The other direction: an admitted-unconsumed field that has quietly gained a real read must be
/// promoted to an `At` row, so the table keeps describing the tree.
///
/// Deliberately narrow — it looks for an anchored `policy.<field>` read (any binding whose name
/// ends in `policy`/`Policy`) in the PRODUCTION view of every source outside this crate
/// ([`policy_field_reads`]), so neither the prose that EXPLAINS why a field is unconsumed nor a
/// test fixture spelling the key reads as the consumption it is describing.
///
/// ⚠ The view is the shared scanner's, the same one `settings_are_consumed.rs`'s direction 4 uses:
/// it used to skip `//` and `*` lines and nothing else, so a `#[cfg(test)] mod` naming the key
/// counted as a reader while a `#[cfg(test)] mod` CLAIMING a read (direction 2) did not have to be
/// production either — two rules on one question.
#[test]
fn an_unconsumed_field_is_really_unconsumed() {
    let root = workspace_root();
    let production = production_sources(&root);
    let mut failures = Vec::new();

    for (field, consumed) in POLICY_CONSUMERS {
        let Consumed::No { why } = consumed else {
            continue;
        };
        // An excuse has to be an ARGUMENT. "TODO" or a blank string is how a field with no
        // consumer and no defence gets waved through, which is the exact shape being gated.
        assert!(
            why.len() > 60 && !why.to_lowercase().contains("todo"),
            "Policy::{field} is marked Consumed::No, but its `why` does not say what enforces \
             the concept instead: {why:?}"
        );
        for rel in &production {
            // The declaring crate's own plumbing (definition, patch, apply, its tests) is not
            // consumption.
            if rel.starts_with(SELF_CRATE) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(root.join(rel)) else { continue };
            // `crates/vike-mount/src/policy.rs`'s module doc names `policy.max_leverage` precisely
            // to explain that it is NOT read there: a comment line is not in the view.
            for (n, line) in policy_field_reads(&source, field) {
                failures.push(format!(
                    "Policy::{field} is marked Consumed::No, but `{rel}:{n}` reads it:\n    \
                     {}\n  Promote it to a Consumed::At row naming that read.",
                    line.trim()
                ));
            }
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// The PRODUCTION lines of `source` (1-indexed, as [`production_lines`] numbers them) that read
/// `policy.<field>` through any binding whose name ends in `policy`/`Policy` — THE search of
/// direction 4, shared with its proof.
fn policy_field_reads<'a>(source: &'a str, field: &str) -> Vec<(usize, &'a str)> {
    let needle = format!("policy.{field}");
    production_lines(source)
        .into_iter()
        .filter(|(_, line)| line.to_lowercase().contains(&needle))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// non-vacuity — the searches above, watched REJECTING
// ---------------------------------------------------------------------------------------------

/// **Direction 2's text rule, proved to reject and to accept.** A ceiling needle that exists ONLY
/// in a comment (line or doc), or ONLY inside a `#[cfg(test)] mod`, must not satisfy a
/// [`Consumed::At`] row; the same needle in production code must. Each source carries the needle in
/// exactly one place, so each verdict is about that place alone.
#[test]
fn a_claimed_read_in_a_comment_or_a_test_module_does_not_count() {
    let file = "crates/vike-mount/src/lib.rs";
    let production: BTreeSet<String> = [file.to_string()].into();
    let needle = ".with_halt_admit(halt_admit)";

    let in_line_comment = "\
fn live(b: B) -> B {
    // b.with_halt_admit(halt_admit)
    b
}
";
    let in_doc_comment = "\
/// Calls `b.with_halt_admit(halt_admit)`.
fn live(b: B) -> B {
    b
}
";
    let in_test_module = "\
fn live(b: B) -> B {
    b
}
#[cfg(test)]
mod tests {
    fn t(b: B) {
        let _ = b.with_halt_admit(halt_admit);
    }
}
";
    let in_production = "\
fn live(b: B, halt_admit: H) -> B {
    b.with_halt_admit(halt_admit)
}
";

    for (case, source) in [
        ("a `//` comment", in_line_comment),
        ("a `///` doc comment", in_doc_comment),
        ("a `#[cfg(test)] mod`", in_test_module),
    ] {
        assert_eq!(
            rejected_claim(file, needle, Some(source), &production),
            Some(Rejection::NotInProduction),
            "a ceiling needle that exists only inside {case} satisfied a Consumed::At row — the \
             ceiling is unread in every shipped build while this gate certifies it enforced:\n\
             {source}"
        );
    }
    assert_eq!(
        rejected_claim(file, needle, Some(in_production), &production),
        None,
        "a needle in production code must count — otherwise direction 2 fails every honest row"
    );
    // …and below a test module the scan RESUMES: a read after one is still a read.
    let after_test_module = format!("{in_test_module}{in_production}");
    assert_eq!(
        rejected_claim(file, needle, Some(after_test_module.as_str()), &production),
        None,
        "production code BELOW a `#[cfg(test)] mod` is still production code"
    );
    assert_eq!(
        rejected_claim(file, needle, None, &production),
        Some(Rejection::Missing),
        "an unreadable claimed file must be reported as missing"
    );
}

/// **Direction 2's FILE rule, over the real tree.** A claim naming a file that is test code must be
/// rejected even when its text would match: an integration test, and the body of a real
/// `#[cfg(test)] mod name;` sibling file — the out-of-line shape, whose text carries no
/// `#[cfg(test)]` at all and so can only be recognised from its parent's declaration.
#[test]
fn a_claimed_consumer_that_is_a_test_file_does_not_count() {
    let root = workspace_root();
    let production = production_sources(&root);
    let source = "\
fn live(b: B, halt_admit: H) -> B {
    b.with_halt_admit(halt_admit)
}
";
    let needle = ".with_halt_admit(halt_admit)";

    // The halt_admit row's own file is production, and the planted text reads there.
    let real = "crates/bridges/ctrader/src/mount.rs";
    assert_eq!(
        rejected_claim(real, needle, Some(source), &production),
        None,
        "`{real}` must be a production source — if it moved, re-point this proof"
    );

    let integration_test = "crates/vike-config/tests/policy_is_consumed.rs";
    assert!(root.join(integration_test).is_file(), "{integration_test} must exist");
    assert_eq!(
        rejected_claim(integration_test, needle, Some(source), &production),
        Some(Rejection::TestFile),
        "an integration test satisfied a Consumed::At row"
    );

    let siblings = cfg_test_module_files_under(&root.join("crates"));
    assert!(
        !siblings.is_empty(),
        "no `#[cfg(test)] mod name;` sibling file found under crates/ — the walk is broken, and \
         this proof would be vacuous"
    );
    for path in &siblings {
        let rel = rel_path(&root, path);
        assert_eq!(
            rejected_claim(&rel, needle, Some(source), &production),
            Some(Rejection::TestFile),
            "`{rel}` is the body of a `#[cfg(test)] mod name;`, yet a claim naming it counted as \
             a production read"
        );
    }
}

/// **Direction 4's search, proved to reject and to accept.** The admitted-unconsumed field's key in
/// a comment or inside a `#[cfg(test)] mod` is not a reader; in production code it is — through a
/// binding of either case, which is what the lowercase comparison is for.
#[test]
fn an_unconsumed_fields_reader_in_a_comment_or_a_test_module_is_not_a_reader() {
    let field = "max_leverage";
    let in_comment = "\
// mount_policy.max_leverage is deliberately not carried
fn m() {}
";
    let in_test_module = "\
fn m() {}
#[cfg(test)]
mod tests {
    fn t(policy: P) {
        let _ = policy.max_leverage;
    }
}
";
    assert_eq!(policy_field_reads(in_comment, field), Vec::new(), "a comment is not a reader");
    assert_eq!(
        policy_field_reads(in_test_module, field),
        Vec::new(),
        "a `#[cfg(test)] mod` naming the key is not a reader — no shipped build compiles it"
    );

    let in_production = "\
fn m(mount_Policy: P) -> f64 {
    mount_Policy.max_leverage
}
";
    assert_eq!(
        policy_field_reads(in_production, field),
        vec![(2usize, "mount_Policy.max_leverage")],
        "a production read must be FOUND — otherwise a Consumed::No row can never be caught \
         gaining a reader"
    );
    let after_test_module = format!("{in_test_module}{in_production}");
    assert_eq!(
        policy_field_reads(&after_test_module, field).len(),
        1,
        "production code BELOW a `#[cfg(test)] mod` is still searched"
    );
}

/// **The mutation proof, over the REAL claimed files.** For every [`Consumed::At`] row that holds
/// today, two edits of a COPY of the claimed file's text must each turn the row red: commenting out
/// every production line carrying the needle, and moving those lines into a `#[cfg(test)] mod`
/// appended to the file. Both left the old raw-`contains` rule green; a row that survived either
/// would mean the needle is ALSO matched somewhere that is not a read.
#[test]
fn every_at_row_goes_red_when_its_read_is_commented_out_or_moved_into_a_test_module() {
    let root = workspace_root();
    let production = production_sources(&root);
    let mut proved = 0usize;

    for (field, consumed) in POLICY_CONSUMERS {
        let Consumed::At { file, needle } = consumed else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(root.join(file)) else { continue };
        // A row that does not hold today is direction 2's failure to report, not this proof's.
        if rejected_claim(file, needle, Some(source.as_str()), &production).is_some() {
            continue;
        }
        let read_lines: BTreeSet<usize> = production_lines(&source)
            .into_iter()
            .filter(|(_, line)| line.contains(needle))
            .map(|(n, _)| n - 1)
            .collect();
        let lines: Vec<&str> = source.lines().collect();

        let mut commented = String::new();
        for (i, l) in lines.iter().enumerate() {
            if read_lines.contains(&i) {
                commented.push_str("// ");
            }
            commented.push_str(l);
            commented.push('\n');
        }
        assert_eq!(
            rejected_claim(file, needle, Some(commented.as_str()), &production),
            Some(Rejection::NotInProduction),
            "Policy::{field}: with every read of `{needle}` in `{file}` commented out, the row \
             still held"
        );

        let mut moved = String::new();
        for (i, l) in lines.iter().enumerate() {
            if !read_lines.contains(&i) {
                moved.push_str(l);
                moved.push('\n');
            }
        }
        moved.push_str("#[cfg(test)]\nmod planted_by_policy_is_consumed {\n");
        for &i in &read_lines {
            moved.push_str(lines[i]);
            moved.push('\n');
        }
        moved.push_str("}\n");
        assert_eq!(
            rejected_claim(file, needle, Some(moved.as_str()), &production),
            Some(Rejection::NotInProduction),
            "Policy::{field}: with every read of `{needle}` in `{file}` moved into a \
             `#[cfg(test)] mod`, the row still held"
        );
        proved += 1;
    }

    assert!(proved > 0, "no Consumed::At row held, so nothing was mutated — this proof is vacuous");
}
