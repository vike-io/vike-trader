//! The [`Policy`] consumption gate — every hard ceiling this machine DECLARES must be read by
//! something that enforces it.
//!
//! `Policy` is the settings system's risk tier: the one type with no env layer and no CLI layer,
//! whose whole purpose is that a ceiling written here cannot be quietly widened. That guarantee is
//! worth exactly nothing if the ceiling is never READ — an operator who writes a limit into
//! `<project>/settings/policy.toml`, watches it validate (a negative value is rejected, a typo'd key is
//! rejected by name), and gets no complaint, reasonably believes the limit is armed.
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

use std::path::{Path, PathBuf};

use vike_config::Policy;

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the `load_workspace_dotenv`
/// idiom used by every other source-walking gate in this workspace.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

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
    // `policy.toml` to 1x — a behaviour change on upgrade, from a file nobody wrote. Reconciling
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
    // vike-app's order-entry preview is the one named here; vike-cli's `lib.rs` and
    // vike-tradehub's `main.rs` read it identically.
    (
        "max_notional_per_order",
        Consumed::At {
            file: "crates/vike-app/src/main.rs",
            needle: "settings.policy.max_notional_per_order",
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
    // Carried by `MountPolicy::from` and APPLIED at the one venue arm where it can do anything:
    // cTrader, the only adapter holding a position book at its halt boundary. The needle is the
    // setter call at that arm — not the field name, and not the `MountPolicy` projection, because
    // carrying a value is not enforcing it (the distinction `max_total_exposure` was removed for).
    (
        "halt_admit",
        Consumed::At {
            file: "crates/vike-mount/src/lib.rs",
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
    // ⚠ Deliberately NOT the `paper_mount` arm, and not `vike-app`: neither constructs it, and the
    // field's own doc says why the gate-off paper arm does not — and that the live GATE, not exec
    // arming, is what enters `live_mount_with`. `docs/ops/kill-switches.md` states the residuals.
    (
        "deadman_timeout_ms",
        Consumed::At {
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
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
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "policy.deadman_action.to_core()",
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
        market_slippage: _,
        halt_admit: _,
        deadman_timeout_ms: _,
        deadman_action: _,
        venues: _,
    } = Policy::default();
    vec![
        "max_leverage",
        "max_notional_per_order",
        "market_slippage",
        "halt_admit",
        "deadman_timeout_ms",
        "deadman_action",
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
#[test]
fn every_claimed_consumer_really_reads_it() {
    let root = workspace_root();
    let mut failures = Vec::new();

    for (field, consumed) in POLICY_CONSUMERS {
        let Consumed::At { file, needle } = consumed else {
            continue;
        };
        let path = root.join(file);
        let Ok(source) = std::fs::read_to_string(&path) else {
            failures.push(format!(
                "Policy::{field} claims a consumer in `{file}`, but that file does not exist \
                 (looked in {}). Point the row at the real consumer, or downgrade it to \
                 Consumed::No with a reason.",
                path.display()
            ));
            continue;
        };
        if !source.contains(needle) {
            failures.push(format!(
                "Policy::{field} is DECLARED but NOT CONSUMED.\n  \
                 The row claims `{file}` reads it as `{needle}`, and that text is not in the \
                 file.\n  \
                 A ceiling nothing reads is a limit the operator believes they set and does not \
                 have: `policy.toml` validates the value and `deny_unknown_fields` accepts the \
                 key, so nothing tells them otherwise.\n  \
                 Fix it by ENFORCING the field (wire it to a real read and point the needle at \
                 it) or by DELETING it (remove the field, and every doc claiming it is \
                 enforced)."
            ));
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
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
/// ends in `policy`/`Policy`), skipping comment lines so the prose that EXPLAINS why a field is
/// unconsumed does not read as the consumption it is describing.
#[test]
fn an_unconsumed_field_is_really_unconsumed() {
    let root = workspace_root();
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
        let needle = format!("policy.{field}");
        for path in rust_sources(&root.join("crates")) {
            // The declaring crate's own plumbing (definition, patch, apply, its tests) is not
            // consumption.
            let rel =
                path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if rel.starts_with("crates/vike-config/") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else { continue };
            for (n, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                // Skip doc/line comments — `crates/vike-mount/src/policy.rs`'s module doc names
                // `policy.max_leverage` precisely to explain that it is NOT read there.
                if trimmed.starts_with("//") || trimmed.starts_with("*") {
                    continue;
                }
                if trimmed.to_lowercase().contains(&needle) {
                    failures.push(format!(
                        "Policy::{field} is marked Consumed::No, but `{rel}:{}` reads it:\n    \
                         {}\n  Promote it to a Consumed::At row naming that read.",
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// Every `.rs` file under `dir`, minus the vendored tree this workspace does not own.
fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                // `target/` is build output; the ibkr vendor tree is third-party.
                if name == "target" || name == "vendor" {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out
}
