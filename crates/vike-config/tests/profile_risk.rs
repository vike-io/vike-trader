//! The [`vike_config::profile_risk`] gate — **the roster must be `vike_exec::ProfileRisk`, not a
//! copy of it that was right once.**
//!
//! # What this is for
//!
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 2 mirrors a run
//! profile's `[risk]` table into the settings database so `vike-cli config show` can print the live
//! pre-trade ceilings with the daemon down. Phase 1 kept the four settings files' unknown-key
//! refusal by materialising rows back through the patch types, which live in `vike-config`;
//! `[risk]`'s type is `vike_exec::ProfileRisk`, which does not — `vike-cli` links neither
//! `vike-core` nor `vike-exec` and the `light-consumers` CI lane exists to hold it there. So the
//! property is bought with a declared ROSTER instead, and a roster is only worth what the thing
//! holding it equal to the type is worth. That is this file.
//!
//! # Why it reads the SOURCE rather than the type
//!
//! Because it cannot read the type: this crate sits BELOW `vike-exec` and may not link it, and a
//! dev-dependency that pointed upward would be legal (`crates/vike-ops/tests/layer_gate.rs` exempts
//! dev edges) and would still be a manifest edit. Reading
//! `crates/vike-exec/src/risk_profile.rs` is the same technique
//! `crates/vike-config/tests/ceilings_are_distinct.rs` uses on `crates/vike-exec/src/risk.rs` to
//! harvest the deny-lane roster, and it fails the same way for the same reasons.
//!
//! ⚠ **This gate is textual, and there is a BEHAVIOURAL twin that a text gate cannot be**:
//! `crates/vike-tradehub/tests/profile_risk_rows.rs` takes a real profile, mirrors it through the
//! real renderer, reassembles a `[risk]` document from the rows, and parses THAT with the real
//! `vike_exec::ProfileRisk` — in the one crate that links every piece. This file proves the roster
//! names the right keys; that one proves a row round-trips into the type that judges an order.
//!
//! ⚠ **Rows are keyed by repo-relative PATH**, so a file MOVE or RENAME invalidates this gate — the
//! repo-wide trap the root `CLAUDE.md`'s file-move bullet describes. `git grep` the old path before
//! you push; the read below is the half that catches it.

use std::path::{Path, PathBuf};

use vike_config::{
    PROFILE_RISK_KEYS, RiskKeyKind, ceiling_for, missing_keys, profile_risk_key,
    risk_rows_from_profile, unknown_rows,
};

/// The file the roster is held equal to.
const PROFILE_RISK_SOURCE: &str = "crates/vike-exec/src/risk_profile.rs";

/// The live-profile template this gate renders, so the shipped operator document and the renderer
/// cannot disagree about what a `[risk]` table may contain.
const LIVE_PROFILE_TEMPLATE: &str = "docs/ops/run-profile-live.toml";

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the idiom every other
/// source-walking gate in this workspace uses.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Read a repo-relative file this gate claims.
///
/// `None` means **this tree does not carry that file's top-level directory at all** — the public
/// source mirror, which ships no `docs/`. Anything else is a hard failure: a file missing from a
/// directory that IS here is a rename, and re-keying is the fix.
fn read_claimed_file(file: &str) -> Option<String> {
    let root = workspace_root();
    match std::fs::read_to_string(root.join(file)) {
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
                "`{file}` is claimed by this gate and could not be read ({e}), while `{top}/` IS \
                 present — so this is a MOVE, not a mirror. Re-key the constant at the top of this \
                 file."
            )
        }
    }
}

/// The `pub NAME: TYPE` fields of `struct ProfileRisk`, harvested out of its own source.
///
/// Scoped to the struct BLOCK — from the `pub struct ProfileRisk {` line to the first line that is
/// a bare `}` — so `RiskLimits`' identically-named fields in `to_risk_limits` below it are not
/// swept in. Doc comments and attributes between fields are ignored because only a line whose first
/// token is `pub` is read.
fn harvested_fields() -> Vec<(String, String)> {
    let body = read_claimed_file(PROFILE_RISK_SOURCE)
        .expect("`crates/` is present in every tree this gate runs in");
    let mut out = Vec::new();
    let mut inside = false;
    for line in body.lines() {
        let t = line.trim();
        if t == "pub struct ProfileRisk {" {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if t == "}" {
            break;
        }
        let Some(rest) = t.strip_prefix("pub ") else { continue };
        let Some((name, ty)) = rest.split_once(':') else { continue };
        out.push((name.trim().to_string(), ty.trim().trim_end_matches(',').to_string()));
    }
    assert!(
        !out.is_empty(),
        "no `pub` field was harvested from `{PROFILE_RISK_SOURCE}`'s `ProfileRisk` — the struct \
         was renamed, reformatted or moved, and this gate would otherwise pass on an empty set"
    );
    out
}

/// The [`RiskKeyKind`] a harvested Rust type implies. `None` for a type this gate has never seen,
/// which is a STOP: a new scalar shape needs a deliberate decision about what a row may hold, not a
/// guess.
fn kind_of(ty: &str) -> Option<RiskKeyKind> {
    match ty {
        "Option<f64>" | "f64" => Some(RiskKeyKind::Float),
        "Option<usize>" | "usize" | "Option<i64>" | "i64" => Some(RiskKeyKind::Integer),
        "Option<bool>" | "bool" => Some(RiskKeyKind::Boolean),
        _ => None,
    }
}

/// Direction 1 — **every `[risk]` field has a roster row**. A new key on `ProfileRisk` reddens here
/// until somebody declares what a row for it may hold.
#[test]
fn every_profile_risk_field_has_a_roster_row() {
    for (name, ty) in harvested_fields() {
        assert!(
            profile_risk_key(&name).is_some(),
            "`ProfileRisk::{name}: {ty}` has no `PROFILE_RISK_KEYS` row.\n\
             Add one in `crates/vike-config/src/profile_risk.rs`, naming its scalar shape and what \
             it bounds — without it the mirror REFUSES a profile that sets the key, which is a \
             correct refusal and a useless one."
        );
    }
}

/// Direction 2 — **no roster row names a field that is gone**. A stale row is a key the mirror
/// would accept and the boot's own parser would refuse, which is the exact inverse of the property
/// the roster is for.
#[test]
fn no_roster_row_names_a_field_that_no_longer_exists() {
    let fields = harvested_fields();
    for k in PROFILE_RISK_KEYS {
        assert!(
            fields.iter().any(|(name, _)| name == k.name),
            "`PROFILE_RISK_KEYS` carries `{}`, which is no longer a field of `ProfileRisk` in \
             `{PROFILE_RISK_SOURCE}`. Delete the row — a one-line cleanup, never a blocker.",
            k.name
        );
    }
}

/// Direction 3 — **the declared SHAPE agrees with the field's type**. Separate from the two above
/// on purpose: they fail for different reasons and want different fixes.
///
/// ⚠ **This is the ONLY gate on that direction, and the sentence that used to stand here was
/// wrong.** It read: *TOML's `3` is an integer and serde will not take one for an `f64`, so a
/// `max_leverage` declared `Integer` would let the mirror write `3` and the daemon reject the same
/// file.* Serde DOES take it (MEASURED —
/// `crates/vike-tradehub/tests/profile_risk_rows.rs`'s
/// `the_real_parsers_cross_shape_answers_are_pinned`), so that behavioural gate cannot see a
/// Float-declared-Integer row at all: its sample would be `1` and the type would accept it. The
/// real cost of the mis-declaration is the opposite one — `accepts` would then REFUSE
/// `max_leverage = 1.0`, i.e. the mirror rejecting a profile the daemon starts on — and the only
/// thing that catches the declaration itself is this comparison against the field's Rust type.
/// A mutation proof on that exact row is how both facts were established.
#[test]
fn every_roster_row_declares_the_shape_its_field_actually_has() {
    for (name, ty) in harvested_fields() {
        let Some(k) = profile_risk_key(&name) else { continue };
        let expected = kind_of(&ty).unwrap_or_else(|| {
            panic!(
                "`ProfileRisk::{name}` has the type `{ty}`, which this gate has never seen. A new \
                 scalar shape needs a `RiskKeyKind` and a decision about what a row may hold — \
                 extend `kind_of` in this file and `RiskKeyKind` in \
                 `crates/vike-config/src/profile_risk.rs` together."
            )
        });
        assert_eq!(
            k.kind,
            expected,
            "`{name}` is declared `{}` in `PROFILE_RISK_KEYS` and is `{ty}` on `ProfileRisk`",
            k.kind.as_str()
        );
    }
}

/// Direction 4 — **the roster is ordered as the struct is**, so the two read side by side and a
/// reviewer comparing them is comparing lists rather than sets.
#[test]
fn the_roster_is_in_the_structs_own_order() {
    let fields: Vec<String> = harvested_fields().into_iter().map(|(n, _)| n).collect();
    let roster: Vec<&str> = PROFILE_RISK_KEYS.iter().map(|k| k.name).collect();
    assert_eq!(fields, roster, "keep `PROFILE_RISK_KEYS` in `ProfileRisk`'s field order");
}

/// Direction 5 — **every row renders**. A blank `what` is a column that prints an empty cell beside
/// a live ceiling, which is worse than no column.
#[test]
fn every_roster_row_is_legible() {
    for k in PROFILE_RISK_KEYS {
        assert!(!k.name.is_empty());
        assert!(
            k.what.len() > 20,
            "`{}`'s `what` is too short to say what it bounds: {:?}",
            k.name,
            k.what
        );
    }
}

/// The JOIN onto `PRE_TRADE_CEILINGS` rather than a second copy of its flag — and the direction
/// that matters: every run-profile CEILING row must be a roster key, or `config show` would name a
/// ceiling the mirror can never carry a value for.
#[test]
fn every_run_profile_ceiling_is_a_roster_key() {
    for c in vike_config::PRE_TRADE_CEILINGS {
        if c.home != vike_config::CeilingHome::RunProfileRisk {
            continue;
        }
        assert!(
            profile_risk_key(c.name).is_some(),
            "`PRE_TRADE_CEILINGS` carries a run-profile ceiling `{}` with no `PROFILE_RISK_KEYS` \
             row, so `config show` would name it and never be able to print its value",
            c.name
        );
    }
    // ...and the two that REFUSE a live mount are reachable through the join, which is what the
    // `config show` block renders its warning from.
    let refusing: Vec<&str> = PROFILE_RISK_KEYS
        .iter()
        .filter(|k| ceiling_for(k.name).is_some_and(|c| c.refuses_live_mount_when_absent))
        .map(|k| k.name)
        .collect();
    assert!(
        refusing.contains(&"max_notional_per_order") && refusing.contains(&"max_total_exposure"),
        "the two keys a live mount refuses to start without must be reachable from the roster: \
         {refusing:?}"
    );
}

/// **The shipped operator template mirrors**, end to end through the real renderer.
///
/// This is the gate on the template and the roster agreeing: `docs/ops/run-profile-live.toml` is
/// the file an operator copies, so a `[risk]` key it documents and the roster does not carry would
/// make the mirror refuse the very file the tree tells people to write.
#[test]
fn the_shipped_live_profile_template_mirrors_through_the_real_renderer() {
    let Some(body) = read_claimed_file(LIVE_PROFILE_TEMPLATE) else { return };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("run-live.toml");
    std::fs::write(&path, body).unwrap();

    let stored = risk_rows_from_profile(&path).expect("the shipped template must mirror");
    assert_eq!(stored.profile, "run-live.toml");
    assert!(unknown_rows(&stored).is_empty(), "the template may set no key outside the roster");
    // The template is the LIVE one, so it must set both keys a live mount refuses to start
    // without — otherwise it teaches a file that cannot start the daemon it is written for.
    for key in ["max_notional_per_order", "max_total_exposure"] {
        assert!(
            stored.rows.iter().any(|r| r.key == key),
            "`{LIVE_PROFILE_TEMPLATE}` sets no `{key}`, and a live mount refuses to start without \
             it: {:?}",
            stored.rows
        );
    }
    // ...and the complement is computed from the roster, so an operator reading `config show`
    // against this file is told what it does NOT set.
    let unset: Vec<&str> = missing_keys(&stored).iter().map(|k| k.name).collect();
    assert!(
        unset.contains(&"tick_size"),
        "a LIVE template must not set the venue-owned grid fields; a live mount rejects them: \
         {unset:?}"
    );
}
