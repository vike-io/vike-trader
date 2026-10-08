//! Unit tests for the registry's own invariants (row keys, scope, naming, defaults).

use super::*;

/// The registry is keyed on `(name, krate)`, not `name`: a variable read by several crates
/// gets one row per crate, because each carries its own default and its own evidence.
/// A duplicated PAIR means two rows disagree about the same read site.
#[test]
fn registry_rows_are_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for s in all_settings() {
        assert!(
            seen.insert((s.name, s.krate)),
            "duplicate registry row for {} in {}",
            s.name,
            s.krate
        );
    }
}

/// `Scope` must agree with the name in both directions: a `VIKE_`-prefixed name is always
/// `Scope::Vike`, and — the half the earlier version of this test could not catch — a row
/// claiming `Scope::Vike` must actually carry that prefix.
#[test]
fn scope_matches_the_name_prefix() {
    for s in all_settings() {
        let prefixed = s.name.starts_with("VIKE_");
        assert_eq!(
            prefixed,
            s.scope == Scope::Vike,
            "{} has VIKE_ prefix = {prefixed} but declares {:?}",
            s.name,
            s.scope
        );
    }
}

/// `Naming::Konst` names the Rust constant the value reaches `env::var` through; an
/// empty identifier would make the Task-5 cross-check vacuous.
#[test]
fn konst_naming_carries_an_identifier() {
    for s in all_settings() {
        if let Naming::Konst(ident) = s.naming {
            assert!(!ident.is_empty(), "{} declares Konst with an empty ident", s.name);
        }
    }
}

/// Layer and Naming must agree on every row: an `Injected` row reads a caller-supplied map,
/// and a `Library` direct-read row never does — `layer_for` only ever classifies a
/// non-bin/test/build file as `Library` when the read was a direct `env::var` (its
/// `injected` branch returns `Injected` instead), so `Library` + `MapLookup` can never occur
/// honestly. Checks EVERY row — a `.find()` spot-check would stop catching mistakes as soon
/// as the table grows.
///
/// `Binary` is deliberately NOT constrained here (unlike an earlier version of this test):
/// a `main.rs`/`src/bin/*.rs` can legitimately read its own locally-loaded credentials map
/// (`load_workspace_dotenv().get("NAME")`) instead of `env::var` — e.g.
/// `vike-backfill/src/bin/tardis_backfill.rs`'s `TARDIS_API_KEY` — which is Naming::MapLookup
/// at Layer::Binary, a real and correct shape, not a STEP-2 violation.
#[test]
fn layer_and_naming_agree_on_every_row() {
    for s in all_settings() {
        match s.layer {
            Layer::Injected => assert_eq!(
                s.naming,
                Naming::MapLookup,
                "{} is Layer::Injected so it must be read via a map lookup",
                s.name
            ),
            Layer::Library => assert_ne!(
                s.naming,
                Naming::MapLookup,
                "{} is Layer::Library (a direct read) but declares Naming::MapLookup",
                s.name
            ),
            Layer::Binary | Layer::TestOnly | Layer::BuildScript => {}
        }
    }
}

/// A row's `default` is that CRATE's fallback, so two crates reading one variable may
/// legitimately disagree. Guard the invariant that actually matters: a default is either
/// empty (unset means "off") or non-blank — never whitespace, which would silently read as
/// a real value in the operator table.
#[test]
fn defaults_are_empty_or_meaningful() {
    for s in all_settings() {
        assert!(
            s.default.is_empty() || !s.default.trim().is_empty(),
            "{} in {} declares a whitespace-only default",
            s.name,
            s.krate
        );
    }
}
