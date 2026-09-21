//! The [`vike_config::ceilings`] gate — **two ceilings that share a name must be provably two
//! ceilings**, every site either of them claims must be real CODE, and the relationship between
//! them must be held by something that reads the tree rather than this table.
//!
//! # What this is for
//!
//! MEASURED on the live box: `settings/policy.toml` and the run profile `VIKE_RUN_PROFILE` names
//! both carried `max_notional_per_order`, kept in step by a `# mirrors settings/policy.toml`
//! comment. They do not constrain the same act — the policy key guards three EDGE surfaces and is
//! dropped by `vike_mount::MountPolicy::from` before it can reach `vike_exec::RiskLimits`, while
//! the profile key is the one that judges every order the core admits. The relationship was written
//! in a TOML comment and nowhere else, and a comment is exactly as green when it is false.
//!
//! `vike_config::ceilings::PRE_TRADE_CEILINGS` replaced the comment with a table. This file is what
//! makes the table a mechanism rather than a longer comment.
//!
//! # The two tiers, and why the second exists
//!
//! **Tier one compares the table to the tree.** Every declaration and every enforcement site opens
//! and contains what the row says ([`every_claimed_site_exists_and_contains_its_needle`]); a shared
//! name names different acts at disjoint sites ([`a_shared_name_names_two_different_acts`]); each
//! home's operator file names the other's ([`a_shared_name_is_disclosed_at_both_homes`]); an
//! unenforced row says what does the work instead
//! ([`an_unenforced_ceiling_says_what_enforces_the_concept_instead`]); every rendered column
//! carries something to render ([`every_row_is_legible`]).
//!
//! **Tier two is the correction this file was sent back for.** Tier one proves the TABLE is
//! self-consistent; it does not gate the RELATIONSHIP. Fold `Policy::max_notional_per_order` into
//! `vike_exec::RiskLimits` and leave this file alone and every tier-one test still passes — the
//! rows did not move, so nothing compared to rows can notice. Three gates therefore read
//! PRODUCTION code:
//!
//! - [`every_enforcement_needle_is_matched_by_code`] — a needle satisfied only by `//` commentary
//!   is not evidence of enforcement. This one is not hypothetical: the `max_account_exposure` row
//!   claimed the bare quoted `over-account-exposure` in `crates/vike-exec/src/risk.rs`, which
//!   occurs three times there and is a COMMENT every time (the lane builds its reason with
//!   `format!`, so the closing quote never follows the token). Deleting the account lane outright
//!   would have left that row green — this table's own stated defect, inside this table's own fix.
//! - [`every_pre_trade_deny_lane_is_named_or_exempt`] — the deny reasons `vike_exec::RiskGate` can
//!   return are a ROSTER, harvested from `crates/vike-exec/src/risk.rs`. Each must be claimed by a
//!   row or carry a written reason for why it is not an operator-written size ceiling. A NEW
//!   pre-trade ceiling lane reddens until somebody classifies it, which is the `vike_model::VENUES`
//!   playbook aimed at a second roster.
//! - [`the_live_mount_refusal_is_owned_by_exactly_the_rows_that_claim_it`] — the keys
//!   `crates/vike-mount/src/arming.rs`'s `require_live_risk_budget` refuses a live mount over must
//!   be exactly the rows flagged `refuses_live_mount_when_absent`, and no NAME may carry that flag
//!   from two homes. That is the one axis on which the two `max_notional_per_order`s differ in CODE
//!   rather than in prose, so it is the one a gate can hold.
//!
//! The fourth member of tier two lives where it can be behavioural rather than textual:
//! `crates/vike-mount/src/policy.rs`'s
//! `a_policy_per_order_ceiling_reaches_nothing_the_mount_carries` projects two `Policy` values
//! differing ONLY in that key and requires the `MountPolicy`s to be equal. That is the gate on
//! "`MountPolicy::from` starts forwarding the field", and it needs no needle at all.
//!
//! ⚠ Directions are separate tests on purpose, never one loop with several asserts — they fail for
//! different reasons and want different fixes, and a single test reports whichever fired first.
//! Same rule `crates/vike-config/tests/policy_is_consumed.rs` states for its own four. Their number
//! is deliberately not written here; every count this workspace has put in prose has rotted.
//!
//! # What this gate deliberately does NOT do
//!
//! It does not compare the two numbers, because there are no numbers here — the table is static
//! data and a run profile is not loaded by this crate. And it does not require them to agree: they
//! judge different acts, so agreement is not the correct relationship. What an operator needs is to
//! know that both exist and which one judges what.
//!
//! ⚠ **The declared blind spot**: [`deny_lanes`] resolves STRING LITERALS. A deny whose reason is
//! computed (`RiskGate`'s impact veto passes an `ImpactDeny` through) names no lane this file can
//! read, so those sites are COUNTED instead — [`DYNAMIC_DENY_SITES`] is a ratchet, and a new
//! computed deny fails it rather than slipping past the roster unnoticed.
//!
//! ⚠ **The mirror**: the run profile's operator file lives under `docs/`, which
//! `scripts/publish_mirror.sh` never publishes — `crates/vike-ops/tests/publish_mirror_gate.rs`
//! pins that `docs` is absent from ALLOW and asserts the produced tree has none. These are RUNTIME
//! reads, not `include_str!`, so the mirror still COMPILES; but a hard failure there would ship a
//! PERMANENTLY RED test in the public tree. [`read_claimed_file`] therefore skips — loudly, and
//! only when the file's whole TOP-LEVEL DIRECTORY is absent, which is the mirror and nothing else.
//! A file missing while its directory is present is still a hard failure, because that is a rename
//! and renames are what these path-keyed rows exist to catch. Same shape as `vike-core`'s latency
//! pin, which reads `.github/workflows/ci.yml` at run time and skips where there is no workflow.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_config::ceilings::{Ceiling, PRE_TRADE_CEILINGS, Site, ceilings_named, shared_names};

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the idiom every other
/// source-walking gate in this workspace uses.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every site a row claims, declaration included.
fn sites(c: &'static Ceiling) -> impl Iterator<Item = &'static Site> {
    std::iter::once(&c.declared_at).chain(c.enforced_at.iter())
}

/// Read a file a row (or this gate's own machinery) claims.
///
/// `None` means **this tree does not carry that file's top-level directory at all** — the public
/// source mirror, which ships no `docs/`. Anything else is a hard failure: a file missing from a
/// directory that IS here is a rename, and re-keying the row is the fix.
fn read_claimed_file(file: &str) -> Option<String> {
    let root = workspace_root();
    let path = root.join(file);
    match std::fs::read_to_string(&path) {
        Ok(body) => Some(body),
        Err(e) => {
            let top = file.split('/').next().unwrap_or(file);
            if !root.join(top).exists() {
                eprintln!(
                    "SKIPPED for `{file}`: this tree carries no `{top}/` at all, which is the \
                     public source mirror — `scripts/publish_mirror.sh` withholds that directory \
                     deliberately. In the private tree it is always present and this never skips."
                );
                return None;
            }
            panic!(
                "`{file}` is claimed by a `PRE_TRADE_CEILINGS` row and could not be read ({e}), \
                 while `{top}/` IS present — so this is a MOVE, not a mirror.\n\
                 Re-key the row: these rows are keyed by repo-relative path, and a rename \
                 invalidates them silently until this test runs."
            )
        }
    }
}

/// Does `needle` occur somewhere in `body` that a Rust compiler would look at?
///
/// A line is CODE for this purpose when the needle appears before any `//` on it. That is a
/// deliberately conservative reading — it will not credit a needle that only ever occurs after a
/// `//`, which is exactly the case this exists to refuse. It cannot see `/* */`, which this
/// workspace does not use.
fn occurs_outside_a_comment(body: &str, needle: &str) -> bool {
    body.lines().any(|line| line.match_indices(needle).any(|(at, _)| !line[..at].contains("//")))
}

/// Direction 1 — the claim is checkable, so check it.
#[test]
fn every_claimed_site_exists_and_contains_its_needle() {
    for c in PRE_TRADE_CEILINGS {
        for s in sites(c) {
            let Some(body) = read_claimed_file(s.file) else { continue };
            assert!(
                body.contains(s.needle),
                "{} ({}): `{}` no longer contains `{}`.\n\
                 Either the site moved — find the real one and re-point the row — or this ceiling \
                 lost its last enforcement, which is the `Policy::max_total_exposure` defect \
                 arriving again and must be fixed in the code, not by deleting the row.",
                c.name,
                c.home.label(),
                s.file,
                s.needle,
            );
        }
    }
}

/// Tier two — **a needle a comment can satisfy is not evidence of enforcement.**
///
/// Direction 1 asks whether the text is in the file. That is not the question: a `//` comment
/// ABOUT a lane sits in the same file as the lane, survives the lane's deletion, and keeps the row
/// green. This is the exact failure `vike_config::consumed` exists for (a field name appears in log
/// lines and doc comments, and neither is enforcement), and this table shipped one — the
/// `max_account_exposure` row's needle matched three comments and never the deny, which builds its
/// reason with `format!`.
///
/// Applied to DECLARATIONS as well as enforcement sites: a `pub` field that exists only in prose
/// is no more real than a lane that does.
#[test]
fn every_enforcement_needle_is_matched_by_code() {
    for c in PRE_TRADE_CEILINGS {
        for s in sites(c) {
            let Some(body) = read_claimed_file(s.file) else { continue };
            assert!(
                occurs_outside_a_comment(&body, s.needle),
                "{} ({}): `{}` contains `{}` ONLY inside `//` commentary.\n\
                 A comment about a ceiling is exactly as green when the ceiling is gone — which is \
                 the whole reason this table replaced a comment. Point the needle at the thing \
                 ITSELF (the deny reason string as the code builds it, the `fn` line, the field \
                 declaration), not at a sentence describing it.",
                c.name,
                c.home.label(),
                s.file,
                s.needle,
            );
        }
    }
}

/// Direction 2 — **the gate this table exists for.**
///
/// Two rows sharing a name must judge different acts at disjoint sites. The disjointness is on the
/// `(file, needle)` PAIR rather than on the file alone, because one file legitimately holds several
/// lanes (`crates/vike-exec/src/risk.rs` carries the per-order, per-symbol and account lanes side
/// by side) — while the merge this test is here to refuse would land the policy row on the profile
/// row's exact lane.
#[test]
fn a_shared_name_names_two_different_acts() {
    let shared = shared_names();
    assert!(
        !shared.is_empty(),
        "no ceiling name is carried by two homes — if that became true by a key being REMOVED, \
         delete this expectation deliberately; it is here because the collision is the finding"
    );
    for name in shared {
        let rows: Vec<&Ceiling> = ceilings_named(name).collect();

        // The acts must be distinct prose. Identical `guards` on two rows means the table is
        // asserting they ARE one ceiling, in which case they should be one key.
        let acts: BTreeSet<&str> = rows.iter().map(|c| c.guards).collect();
        assert_eq!(
            acts.len(),
            rows.len(),
            "`{name}` is carried by {} homes that claim the same act. Two ceilings with one name \
             and one act are a duplication to resolve, not a table row.",
            rows.len(),
        );

        // …and no enforcement site may be shared between them. THIS is the merge alarm.
        for (i, a) in rows.iter().enumerate() {
            for b in &rows[i + 1..] {
                for sa in a.enforced_at {
                    for sb in b.enforced_at {
                        assert!(
                            !(sa.file == sb.file && sa.needle == sb.needle),
                            "`{name}`: the `{}` row and the `{}` row both claim `{}` / `{}`.\n\
                             Two same-named ceilings enforcing at one site are ONE gate wearing two \
                             keys — and merging two different gates because they share a name is \
                             how a ceiling disappears. If the merge was deliberate, collapse the \
                             rows and say so; if it was not, the fold is the bug.",
                            a.home.label(),
                            b.home.label(),
                            sa.file,
                            sa.needle,
                        );
                    }
                }
            }
        }
    }
}

/// Direction 3 — the ⚠ at each key is gated, so it cannot quietly stop saying a twin exists.
///
/// An operator copying `settings/policy.example.toml` and an operator copying
/// `docs/ops/run-profile-live.toml` never see each other's file. Each must therefore learn from
/// their own that the key they are typing has a same-named sibling somewhere else, and be told
/// where. Checked by content, not by a comment claiming it was done.
///
/// ⚠ The run-profile half reads a `docs/` path, so it SKIPS in the public mirror and only there —
/// see this file's module doc.
#[test]
fn a_shared_name_is_disclosed_at_both_homes() {
    for name in shared_names() {
        let rows: Vec<&Ceiling> = ceilings_named(name).collect();
        for a in &rows {
            let Some(doc) = read_claimed_file(a.home.operator_doc()) else { continue };
            assert!(
                doc.contains(name),
                "{}: the operator file for the `{}` home never mentions the key",
                name,
                a.home.label(),
            );
            for b in rows.iter().filter(|b| b.home != a.home) {
                assert!(
                    doc.contains(b.home.operator_doc()),
                    "`{name}` is written in two files, and `{}` never names the other one \
                     (`{}`).\nAn operator editing this file must be told from THIS file that a \
                     same-named ceiling exists elsewhere and judges a different act — that is the \
                     comment this table replaced, and it has to stay checked.",
                    a.home.operator_doc(),
                    b.home.operator_doc(),
                );
            }
        }
    }
}

/// Direction 4 — a ceiling that enforces nothing has to say what does.
#[test]
fn an_unenforced_ceiling_says_what_enforces_the_concept_instead() {
    for c in PRE_TRADE_CEILINGS.iter().filter(|c| !c.is_enforced()) {
        assert!(
            c.absent_means.len() > 80,
            "{} ({}) claims no enforcement site. That is legal, but the row must say what enforces \
             the concept instead — a ceiling reported as set and bound to nothing is the defect \
             `Policy::max_total_exposure` was deleted for.",
            c.name,
            c.home.label(),
        );
    }
}

/// Direction 5 — every column a disclosure surface renders carries something to render.
#[test]
fn every_row_is_legible() {
    for c in PRE_TRADE_CEILINGS {
        assert!(!c.name.is_empty(), "a row has no key name");
        assert!(
            !c.guards.trim().is_empty(),
            "{}: `guards` is the column that distinguishes two \
             rows sharing a name; it cannot be empty",
            c.name
        );
        assert!(
            !c.absent_means.trim().is_empty(),
            "{} ({}): what an unset value means is the difference between `uncapped` and `the \
             daemon refuses to start live`, and this table has BOTH answers in it",
            c.name,
            c.home.label(),
        );
        for s in sites(c) {
            assert!(!s.what.trim().is_empty(), "{}: a site with no description", c.name);
            assert!(
                s.file.starts_with("crates/")
                    || s.file.starts_with("docs/")
                    || s.file.starts_with("settings/"),
                "{}: `{}` is not a repo-root-relative path — a shorthand path is relative to an \
                 unstated base and cannot be checked",
                c.name,
                s.file,
            );
        }
    }
}

/// The one row `vike-cli config show` cannot print a VALUE for must SAY so, rather than being
/// absent from the payload — which is how `max_total_exposure` came to be alive at 500 on a live
/// box and covered by no disclosure at all.
#[test]
fn a_ceiling_whose_value_config_show_cannot_resolve_is_still_in_the_table() {
    let unresolved: Vec<&Ceiling> =
        PRE_TRADE_CEILINGS.iter().filter(|c| !c.home.value_shown_by_config_show()).collect();
    assert!(
        !unresolved.is_empty(),
        "the run-profile home vanished from the table; if a `config show` that resolves run \
         profiles has landed, delete this test deliberately"
    );
    assert!(
        unresolved.iter().any(|c| c.name == "max_total_exposure"),
        "`max_total_exposure` is enforced, mandatory for a live mount, and resolved by no settings \
         file this command reads. It must have a row or the disclosure gap is back."
    );
}

// ─── Tier two: the roster ──────────────────────────────────────────────────────────────────────

/// The pre-trade gate, read at run time so this gate sees the real lanes rather than a list of
/// them. `vike-config` links neither `vike-exec` nor `vike-mount` (and must not — it sits below
/// both), so a SOURCE read is the only seam available here, and it is the same one every meta-gate
/// in this workspace uses.
const RISK_GATE_FILE: &str = "crates/vike-exec/src/risk.rs";

/// `vike_mount::require_live_risk_budget` — the only function in the workspace that refuses a mount
/// because a ceiling is absent.
const ARMING_FILE: &str = "crates/vike-mount/src/arming.rs";

/// Deny reasons `RiskGate` can return that are NOT an operator-written ceiling on order SIZE or
/// EXPOSURE, each with the reason it is not one. A lane leaves this list the day an operator can
/// write a number that arms it from `policy.toml` or a run profile's `[risk]` table — at which
/// point it needs a `PRE_TRADE_CEILINGS` row instead.
///
/// ⚠ This is an EXEMPTION table, not a denylist: a lane that is in neither this list nor the table
/// fails the roster test. Being wrong here costs a row that says why; being silent costs the whole
/// point of the table.
const NOT_AN_OPERATOR_SIZE_CEILING: &[(&str, &str)] = &[
    ("invalid-side", "a validity check on the request itself; no number arms it"),
    ("not-a-combo", "a validity check on a combo request; no number arms it"),
    ("non-positive-size", "a validity check on the rounded quantity; no number arms it"),
    (
        "halted",
        "the HALT sentinel — a kill switch, armed by a FILE's existence rather than by a ceiling. \
         `policy.halt_admit` tunes how much evidence it demands, which is a mode, not a size cap.",
    ),
    (
        "reduce-only",
        "a flag on the order, not a ceiling: the request itself asked to reduce and would not",
    ),
    (
        "reduce-only-overshoot",
        "armed by a run profile's `block_reduce_only_overshoot`, a BOOLEAN — it refuses an order \
         larger than the position it claims to reduce, so the bound is the position, not a number \
         an operator writes",
    ),
    (
        "below-min-qty",
        "a FLOOR, not a ceiling — it refuses orders that are too SMALL, and its value is the \
         venue's own lot grid on a live mount",
    ),
    (
        "below-min-notional",
        "a FLOOR for the same reason as `below-min-qty`, from the same venue-fetched grid",
    ),
    (
        "price-collar",
        "a PRICE band (the fat-finger guard), not a size ceiling — and not operator-writable from \
         either file today: `ProfileRisk::to_risk_limits` sets its `price_collar` to `None` on \
         purpose, with the reason written at that line",
    ),
    (
        "rate-limited",
        "a THROUGHPUT ceiling (`max_orders_per_window`/`window_ms`), not a size one. The same \
         distinction the root `CLAUDE.md` draws for `VIKE_TRADEHUB_CONTROL_RATE`, which stayed an \
         environment variable precisely because raising it cannot place a larger order.",
    ),
    (
        "leg",
        "not a lane: the combo path prefixes a LEG's own verdict (`leg <symbol>: below-min-qty`), \
         so the ceiling that refused is whichever lane the leg hit, already classified above",
    ),
];

/// Deny sites whose reason this gate cannot read because the code COMPUTES it. A ratchet: it may
/// shrink, never grow without somebody saying why.
///
/// One today — `RiskGate`'s pre-trade impact veto passes an `ImpactDeny`'s own string through, and
/// those knobs (`max_slippage_bps`/`require_fillable`) are not operator-writable from either file
/// (`ProfileRisk::to_risk_limits` leaves both off, with the reason at that line). If this number
/// grows, a lane has been added that the roster below cannot see, and the fix is to give it a
/// literal reason or a row — not to bump the constant.
const DYNAMIC_DENY_SITES: usize = 1;

/// Harvest every deny reason `RiskGate` can return, out of the real source.
///
/// One entry per `Self::deny(` call site: the STATEMENT is taken up to its `;`, the first string
/// literal in it is found, and the reason's leading token is read off it — so a `format!` whose
/// text begins `over-account-exposure (projected …` yields `over-account-exposure`, and a bare
/// `halted` yields itself. A site whose statement holds no literal is COUNTED as dynamic instead
/// (see [`DYNAMIC_DENY_SITES`]).
///
/// Only the production half of the file is scanned — everything before its `#[cfg(test)]` — so a
/// test that constructs a verdict cannot invent a lane.
fn deny_lanes(src: &str) -> (BTreeSet<String>, usize) {
    const CALL: &str = "Self::deny(";
    let prod = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let mut lanes = BTreeSet::new();
    let mut dynamic = 0usize;
    let mut rest = prod;
    while let Some(at) = rest.find(CALL) {
        rest = &rest[at + CALL.len()..];
        let stmt = rest.split(';').next().unwrap_or("");
        match stmt.find('"') {
            Some(q) => {
                let tail = &stmt[q + 1..];
                let end = tail.find(['"', '(', '{', ' ']).unwrap_or(tail.len());
                lanes.insert(tail[..end].to_string());
            }
            None => dynamic += 1,
        }
    }
    (lanes, dynamic)
}

/// Tier two — **the deny lanes are a roster, and this table must cover it.**
///
/// The table's tier-one gates all compare rows to rows, so a NEW pre-trade ceiling — a fresh
/// `Self::deny` armed by a fresh `[risk]` key — lands with every one of them green and the
/// disclosure surface silent about it. This is the `vike_model::VENUES` playbook aimed at a second
/// roster: the set is DERIVED from the code, and a member is either a covered row or an exempt row
/// with a written reason.
#[test]
fn every_pre_trade_deny_lane_is_named_or_exempt() {
    let Some(src) = read_claimed_file(RISK_GATE_FILE) else { return };
    let (lanes, dynamic) = deny_lanes(&src);
    assert!(
        lanes.len() > 5,
        "harvested only {lanes:?} from `{RISK_GATE_FILE}` — the extractor has stopped matching the \
         real call sites, which would make this whole test vacuously green. Re-read `RiskGate`'s \
         `deny` call shape before touching anything else."
    );
    assert_eq!(
        dynamic, DYNAMIC_DENY_SITES,
        "`{RISK_GATE_FILE}` now has {dynamic} deny site(s) whose reason this gate cannot read \
         (it was {DYNAMIC_DENY_SITES}). A computed reason names no lane, so the roster below cannot \
         see it. Give the new site a literal reason, or — if it is genuinely a pass-through of an \
         already-classified verdict — raise this constant WITH the reason written on it."
    );

    let exempt: BTreeSet<&str> = NOT_AN_OPERATOR_SIZE_CEILING.iter().map(|(l, _)| *l).collect();
    for (lane, why) in NOT_AN_OPERATOR_SIZE_CEILING {
        assert!(!why.trim().is_empty(), "{lane}: an exemption with no reason is not an exemption");
        assert!(
            lanes.contains(*lane),
            "`{lane}` is exempted here but `{RISK_GATE_FILE}` no longer denies on it. Delete the \
             row — a stale exemption is how a lane that comes BACK slips past this roster."
        );
    }

    for lane in &lanes {
        // ⚠ The match requires the OPENING QUOTE, not the bare token, and it is a LATENT guard
        // rather than a live one: MEASURED on this table, no needle contains any harvested lane
        // token as a bare substring, so `contains(lane)` and `contains("\"{lane}")` classify the
        // same set today. It is still the right shape, because the two kinds of needle in
        // this table are not alike. A deny-lane needle always spells the string literal, so
        // demanding its opening quote costs nothing; a DECLARATION needle is an identifier
        // (`pub max_leverage`, `fn guardrail_caps`) which can acquire a lane's token by pure
        // coincidence as either roster grows — and a declaration is not evidence that a lane is
        // enforced. Demanding the quote is what keeps an identifier from ever claiming one.
        // (An earlier version of this comment justified the rule with `leg` being read out of
        // `pub max_leverage`. That example is simply false — `max_leverage` contains no `leg` —
        // and it is left named here rather than deleted, because a rule whose stated reason does
        // not hold is the exact defect this file exists to refuse.)
        let quoted = format!("\"{lane}");
        let claimed: Vec<&Ceiling> = PRE_TRADE_CEILINGS
            .iter()
            .filter(|c| c.enforced_at.iter().any(|s| s.needle.contains(&quoted)))
            .collect();
        let is_exempt = exempt.contains(lane.as_str());
        assert!(
            !(claimed.is_empty() && !is_exempt),
            "`RiskGate` can refuse an order with `{lane}` and NOTHING classifies it.\n\
             It is a pre-trade refusal an operator will meet, so it is one of two things:\n  \
             (a) an operator-written ceiling — give it a `PRE_TRADE_CEILINGS` row naming the file \
             it is written in, the act it judges and what an unset value means; or\n  \
             (b) not one — add it to `NOT_AN_OPERATOR_SIZE_CEILING` with the reason.\n\
             Leaving it unclassified is how `max_total_exposure` came to be enforced, mandatory \
             for a live mount, and in no payload `config show` printed."
        );
        assert!(
            !(is_exempt && !claimed.is_empty()),
            "`{lane}` is BOTH exempted as not-an-operator-ceiling and claimed by {} — one of the \
             two is wrong, and while they disagree neither is evidence.",
            claimed
                .iter()
                .map(|c| format!("{} ({})", c.name, c.home.label()))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
}

/// Every key `require_live_risk_budget` refuses a live mount over, harvested from its own source.
///
/// `missing.push` is that function's private idiom, and the scan stops at the file's `#[cfg(test)]`
/// so a test fixture cannot invent a key.
fn live_mount_required_keys(src: &str) -> BTreeSet<String> {
    const PUSH: &str = "missing.push(\"";
    let prod = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let mut out = BTreeSet::new();
    let mut rest = prod;
    while let Some(at) = rest.find(PUSH) {
        rest = &rest[at + PUSH.len()..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
        }
    }
    out
}

/// Tier two — **the live-mount refusal has exactly one owner, and the table names it.**
///
/// This is the one axis on which the two `max_notional_per_order`s differ in CODE rather than in
/// prose: absence of the run profile's key REFUSES a live mount pre-connect
/// (`MountError::MissingRiskBudget`, the daemon exits failure); absence of `policy.toml`'s key
/// changes nothing anywhere. `crates/vike-mount/src/policy.rs`'s module doc calls moving that
/// refusal "a decision, not plumbing" — so it must not be movable without a red test, and the
/// second assertion is what makes it one: no NAME may carry the flag from two homes, because two
/// ceilings that both refuse the mount are no longer distinguishable by the only structural
/// difference between them.
#[test]
fn the_live_mount_refusal_is_owned_by_exactly_the_rows_that_claim_it() {
    let Some(src) = read_claimed_file(ARMING_FILE) else { return };
    let from_code = live_mount_required_keys(&src);
    assert!(
        !from_code.is_empty(),
        "no `missing.push` literal found in `{ARMING_FILE}` — either `require_live_risk_budget` \
         stopped refusing anything (a LIVE-TRADING gate disappearing, which is the finding, not a \
         reason to relax this) or its idiom changed and this extractor must follow it"
    );

    let claimed: BTreeSet<String> = PRE_TRADE_CEILINGS
        .iter()
        .filter(|c| c.refuses_live_mount_when_absent)
        .map(|c| c.name.to_string())
        .collect();
    assert_eq!(
        from_code, claimed,
        "`require_live_risk_budget` refuses a live mount over {from_code:?} while \
         `PRE_TRADE_CEILINGS` flags {claimed:?}.\n\
         If a key JOINED the refusal, a live box that used to start now exits FAILURE when it is \
         absent — say so on its row (`refuses_live_mount_when_absent`), because `absent_means` is \
         what `vike-cli config show` prints and an operator reading `uncapped` there would be \
         reading the opposite of the truth. If a key LEFT it, absence silently became `uncapped` \
         on a live mount, which is the direction that trades."
    );

    for name in shared_names() {
        let owners: Vec<&Ceiling> =
            ceilings_named(name).filter(|c| c.refuses_live_mount_when_absent).collect();
        assert!(
            owners.len() <= 1,
            "`{name}` is flagged as refusing the live mount from {} homes ({}).\n\
             The two same-named ceilings are then indistinguishable on the ONE axis where they \
             differ in code rather than in prose. If the refusal genuinely moved, it moved — and \
             moving it is the decision `crates/vike-mount/src/policy.rs` declines to make silently.",
            owners.len(),
            owners.iter().map(|c| c.home.label()).collect::<Vec<_>>().join(", "),
        );
    }
}
