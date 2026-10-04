//! **Does a policy ceiling actually BITE?** — the row-to-refusal gate, end to end.
//!
//! Everything the settings-unification program built was, until this file, exercised at one end or
//! the other and never across the join:
//!
//! - `vike-config`'s `tests/mirror.rs`/`tests/load.rs` prove a `policy.max_notional_per_order` ROW
//!   LOADS (precedence, clamping, the rejections, and `policy_ignores_the_environment_entirely`) —
//!   but they hand the loaded `Policy` to nothing;
//! - `server.rs`'s own `notional_cap_rejects_oversized_submit_allows_small` proves the server edge
//!   REFUSES an oversized order — but from a `ControlLimits` built by hand in the test;
//! - `vike_model::paths::state_path`'s tests prove the `<project>/settings` walk — but never with a
//!   settings database in it.
//!
//! So no test ever wrote a real ceiling to a real settings database and watched a real order be
//! refused because of it, which is exactly the claim the design rests on. This file is that test. It
//! walks the SAME steps `main.rs`'s `resolve_policy` + `resolve_control_limits` walk, in the same
//! order:
//!
//! ```text
//! <project>/settings/db/vike.db
//!   -> vike_model::paths::state_path::project_settings_dir(cwd)     (the resolver the binary calls)
//!   -> vike_secrets::read_settings_in(home)                  (the BINARY opens the store; a
//!                                                              library never does — see
//!                                                              `vike_config::mirror`'s module doc)
//!   -> vike_config::StoreLayer::of(&store, &mut refusal)     (the same probe `config show` runs)
//!   -> vike_config::load_with_source(home, source, &env, ..) (the real layered loader)
//!   -> server::control::ControlLimitsConfig::from_policy(..) (the pure resolver main.rs feeds)
//!   -> server::control::ControlLimits::vet(&WireCommand, &[], &[]) (the ENFORCING gate; `&[]` =
//!                                                              no engine roster and no named
//!                                                              order, multiplier 1.0 — the
//!                                                              ceiling's own row, not an
//!                                                              instrument's, is what is under test)
//! ```
//!
//! and asserts all four directions the claim needs: the ceiling refuses; an order under it passes;
//! the SAME order passes once the row is gone; and nothing outside the row can raise it.
//!
//! ⚠ **This used to walk `vike_config::load` directly, over a written `policy.toml`.**
//! `docs/decisions/0086` deletes that layer outright: `load`/`load_with_cli` now hand
//! `load_with_source` a `StoreLayer::NotConsulted` unconditionally and open no store of their own —
//! only a composition root's own `load_with_source` call (through `vike_boot::boot` in the shipped
//! binary) ever sees the real rows. So every fixture below plants a settings ROW instead of writing
//! a file, and every resolution goes through the same `read_settings_in` -> `StoreLayer::of` ->
//! `load_with_source` chain the real binary runs, rather than through the pure-default `load`.
//!
//! **Why this crate.** `vike-tradehub`'s control server is the only place in the workspace where a
//! `Policy` ceiling REFUSES anything — `vike-desktop`'s order-entry cap and `vike-cli`'s `trade`/`mcp`
//! guardrail read the same value but only display it (the GUI cap is a local preview; the CLI line
//! literally says "the node will reject"). A test of the ceiling therefore belongs beside the node
//! that does the rejecting.
//!
//! **No process, no socket, no core.** `tests/control_roundtrip.rs` already binds a real node and
//! drives a real `Submit` over the wire; repeating that here would test the transport, not the
//! setting. The value of this file is the JOIN between the loader and the gate, and both halves are
//! pure — so it is a fast unit-speed test with a `TempDir` for the only I/O.
//!
//! ⚠ Every REMOVED variable used below comes from `vike_config::REMOVED_ENV`, never a literal. That
//! is not only tidiness: `crates/vike-ops/tests/settings_registry.rs` harvests env-shaped string
//! literals out of the whole `crates/` tree, `tests/` included, and an invented name would fail CI
//! in an unrelated crate. Reading the table also makes the assertion total — a variable removed
//! later is covered here the day it is added. The four PLATFORM names in
//! `no_per_user_location_can_configure_a_ceiling` are the deliberate exception: they are the OS's
//! names, this workspace declares no row for them under this crate, and the point of naming them is
//! that they configure NOTHING.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_config::{
    CliOverrides, ConfigError, Policy, REMOVED_ENV, Settings, StoreLayer, load, load_with_source,
};
use vike_model::paths::state_path::project_settings_dir;
use vike_secrets::{SettingRow, StoredSettings};
use vike_tradehub::server::control::{ControlLimits, ControlLimitsConfig};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

/// The ceiling every test below plants. Small enough that the "over" case is unmistakable.
const CEILING: f64 = 5.0;

/// One settings row, the shape every fixture below wants: `(section, key, JSON-scalar value)`.
fn stored(specs: &[(&str, &str, &str)]) -> StoredSettings {
    StoredSettings {
        settings: specs
            .iter()
            .map(|(section, key, value)| SettingRow {
                section: (*section).to_string(),
                key: (*key).to_string(),
                value: (*value).to_string(),
            })
            .collect(),
        ..Default::default()
    }
}

/// Lay out a throwaway PROJECT: a `Cargo.toml` (the marker `project_settings_dir` walks up for)
/// plus a real settings DATABASE under `settings/`, planted with the given rows. Returns the nested
/// working directory a process would realistically be started from, to prove the walk actually
/// walks.
fn project(root: &Path, rows: &[(&str, &str, &str)]) -> PathBuf {
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let settings = root.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    if !rows.is_empty() {
        vike_secrets::plant_settings_rows(&settings, &stored(rows))
            .expect("plant the fixture's rows");
    }
    let cwd = root.join("crates").join("some-crate");
    std::fs::create_dir_all(&cwd).unwrap();
    cwd
}

/// **The three steps `main.rs`'s `resolve_policy` takes, in its order**: resolve `<project>/settings`
/// from the working directory, open the settings DATABASE the binary already owns opening (never a
/// library — `vike_config::mirror`'s module doc argues why), then run the real loader over the
/// `StoreLayer` it produces. Returns the `Err` `load_with_source` can still return (only the removed
/// `<project>/vike.toml` layer-0 refusal) rather than unwrapping it, because two tests below need it.
///
/// ⚠ This once passed a third argument — a per-project override directory whose `vike.toml`
/// applied — and every binary in the workspace passed `None` for it, so the mirror was faithful to
/// a BUG: a layer implemented, tested, and named in `vike-cli config show`'s precedence header
/// while nothing read one. Wiring it made the header true; REMOVING it made the shape right,
/// because a fifth settings file above `<project>/settings/` puts "which file won?" back into every
/// investigation, and a ceiling is precisely the value that must have exactly one answer. The
/// parameter is gone from `load` itself, so this mirror cannot drift from the roots again — there
/// is nothing left to pass.
///
/// It changes no assertion below: these cases plant `policy.max_notional_per_order`, which is the
/// only row a ceiling was ever settable from.
fn try_resolve_from(cwd: &Path, env: &HashMap<String, String>) -> Result<Settings, ConfigError> {
    let home = project_settings_dir(cwd);
    let store = home.as_deref().map(vike_secrets::read_settings_in);
    let mut refusal = String::new();
    let source = StoreLayer::of(store.as_ref(), &mut refusal);
    load_with_source(home.as_deref(), source, env, &CliOverrides::default())
}

/// The ordinary case: the load must not `Err` (no `<project>/vike.toml` is planted anywhere in this
/// file), so this unwraps and hands back the policy the real chain resolved.
fn resolve_policy_from(cwd: &Path, env: &HashMap<String, String>) -> Policy {
    try_resolve_from(cwd, env).expect("settings load").policy
}

/// The two steps `main.rs`'s `resolve_control_limits` takes: the loaded ceiling into the pure
/// config resolver, then a fresh per-connection limiter off it.
fn gate(policy: &Policy) -> ControlLimits {
    ControlLimits::new(ControlLimitsConfig::from_policy(policy.max_notional_per_order, None))
}

fn submit(qty: f64, price: f64) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "POLICY-CEILING-E2E".to_string(),
        venue: "polymarket".to_string(),
        symbol: "PROOFTOKEN".to_string(),
        side: 1,
        qty,
        order_type: "limit".to_string(),
        price: Some(price),
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

// ---------------------------------------------------------------------------------------------
// The ceiling bites
// ---------------------------------------------------------------------------------------------

/// **THE test.** A ceiling planted as a `policy.max_notional_per_order` ROW refuses an order that
/// exceeds it, naming the setting — and lets one under it through.
#[test]
fn a_ceiling_in_the_project_settings_row_refuses_an_oversized_order() {
    let root = tempfile::tempdir().unwrap();
    let cwd = project(root.path(), &[("policy", "max_notional_per_order", "5.0")]);

    let policy = resolve_policy_from(&cwd, &HashMap::new());
    assert_eq!(policy.max_notional_per_order, Some(CEILING), "the row did not reach the loader");

    let mut gate = gate(&policy);

    // OVER: 100 x 0.5 = 50, ten times the ceiling.
    let reason =
        gate.vet(&submit(100.0, 0.5), &[], &[]).expect("an over-ceiling order must be refused");
    assert!(reason.contains("50.00"), "the refusal must name the order's notional: {reason}");
    assert!(
        reason.contains("max_notional_per_order 5.00"),
        "the refusal must name the SETTING and its value, not an env var: {reason}"
    );

    // UNDER: 4 x 1.0 = 4.
    assert_eq!(
        gate.vet(&submit(4.0, 1.0), &[], &[]),
        None,
        "an order inside the ceiling must pass"
    );
    // AT the boundary: the cap is `>`, so exactly the ceiling passes.
    assert_eq!(gate.vet(&submit(1.0, CEILING), &[], &[]), None, "the ceiling itself must pass");
}

/// The other direction, and the one that makes the first mean something: with **no row**, the same
/// gate built the same way lets the same order through. If this passed while the test above failed,
/// or vice versa, the demonstration would prove nothing.
#[test]
fn the_same_order_passes_once_the_row_is_gone() {
    let root = tempfile::tempdir().unwrap();
    let cwd = project(root.path(), &[]);

    let policy = resolve_policy_from(&cwd, &HashMap::new());
    assert_eq!(policy, Policy::default(), "no row must be exactly the code defaults");
    assert_eq!(policy.max_notional_per_order, None, "the code default is UNCAPPED");

    assert_eq!(
        gate(&policy).vet(&submit(100.0, 0.5), &[], &[]),
        None,
        "no row ⇒ no cap ⇒ the order passes"
    );
}

/// A ceiling that a typo silently disables is not a ceiling either: the operator writes a key, sees
/// no complaint, and does not have the cap.
///
/// ⚠ **Not a hard load error any more.** `docs/decisions/0086`: `deny_unknown_fields` still refuses
/// the row, but `crate::mirror::apply_rows` MARKS it (`Settings::seal_refusal`) rather than
/// returning `Err` — a hard refusal here would take `config show` and every `secrets` verb down with
/// the box they exist to diagnose. `vike-cli config check`/`vike-tradehub` itself are the surfaces
/// that refuse to START on the mark; this proves the mark exists and names the key.
#[test]
fn a_mistyped_ceiling_key_marks_the_load_instead_of_silently_uncapping() {
    let root = tempfile::tempdir().unwrap();
    let cwd = project(root.path(), &[("policy", "max_notional_per_orderr", "5.0")]);

    let resolved = try_resolve_from(&cwd, &HashMap::new())
        .expect("a row problem MARKS rather than hard-erroring");
    let msg =
        resolved.seal_refusal.as_ref().expect("a typo'd key must not load as 'uncapped'").clone();
    assert!(msg.contains("max_notional_per_orderr"), "…and the offending key: {msg}");
    // …and it really did not silently uncap: the mark is orthogonal to the resolved VALUE, which is
    // why a verb that only discloses (never judges) is safe to keep running on it.
    assert!(resolved.policy.max_notional_per_order.is_none(), "{resolved:?}");
}

/// **Nothing outside `<project>/settings/` configures a ceiling** — the settings twin of
/// `credential_chain_roots.rs`'s `the_home_directory_cannot_supply_credentials`.
///
/// A decoy settings database is planted at every shape a per-user location could take, under a
/// directory the environment names with each of the four platform variables in turn. It holds a
/// ceiling ten million times the project's, so a resolution that consulted one would not merely
/// differ — it would let through an order the project's row refuses. The gate is driven, not just
/// the loader, because the claim is about what gets ORDERED, not about which row parsed.
#[test]
fn no_per_user_location_can_configure_a_ceiling() {
    let root = tempfile::tempdir().unwrap();
    let cwd = project(root.path(), &[("policy", "max_notional_per_order", "5.0")]);

    let decoy_home = root.path().join("home");
    for leaf in [".vike/settings", "settings", "."] {
        let dir = decoy_home.join(leaf);
        std::fs::create_dir_all(&dir).unwrap();
        vike_secrets::plant_settings_rows(
            &dir,
            &stored(&[("policy", "max_notional_per_order", "50000000.0")]),
        )
        .expect("plant the decoy row");
    }

    for name in ["HOME", "USERPROFILE", "XDG_DATA_HOME", "LOCALAPPDATA"] {
        let env = HashMap::from([(name.to_string(), decoy_home.display().to_string())]);
        let policy = resolve_policy_from(&cwd, &env);
        assert_eq!(
            policy.max_notional_per_order,
            Some(CEILING),
            "{name} reached a ceiling outside the project"
        );
        assert!(
            gate(&policy).vet(&submit(100.0, 0.5), &[], &[]).is_some(),
            "{name} raised the ceiling the gate enforces"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// …and cannot be raised
// ---------------------------------------------------------------------------------------------

/// **The sealed-trait guarantee, observed at runtime.** Every removed variable set to a huge value
/// leaves the loaded ceiling — and therefore the gate's verdict — completely unchanged.
///
/// The compile-time half is the real guarantee (`settings.policy.apply_env(&env)` does not resolve:
/// `Policy` implements neither `EnvOverride` nor `CliOverride`, both sealed). This is the runtime
/// net under it, extended past `vike-config`'s own `policy_ignores_the_environment_entirely` to the
/// place it matters — the gate that refuses the order.
#[test]
fn no_environment_variable_can_raise_the_ceiling() {
    let root = tempfile::tempdir().unwrap();
    let cwd = project(root.path(), &[("policy", "max_notional_per_order", "5.0")]);

    // Every REMOVED variable, set as hostilely as possible. The ceiling ones are the point here;
    // the rest ride along because the refusal must name every offender in one pass.
    let hostile: HashMap<String, String> =
        REMOVED_ENV.iter().map(|r| (r.var.to_string(), "1000000000".to_string())).collect();
    assert!(!hostile.is_empty(), "REMOVED_ENV must not be empty — this test would be vacuous");

    let policy = resolve_policy_from(&cwd, &hostile);
    assert_eq!(policy.max_notional_per_order, Some(CEILING), "the ceiling moved");
    assert!(
        gate(&policy).vet(&submit(100.0, 0.5), &[], &[]).is_some(),
        "the gate stopped refusing under a hostile environment"
    );

    // And the belief is not left silently false either: a binary handed this environment REFUSES
    // TO START, naming the row-write command that replaces each variable. That is what `vike-cli`
    // and `vike-tradehub` both call before they load anything.
    let err = vike_config::refuse_removed_env(&hostile)
        .expect_err("a set-but-removed ceiling variable must refuse startup");
    for removed in REMOVED_ENV {
        assert!(err.contains(removed.var), "the refusal must name {}: {err}", removed.var);
        // ⚠ NOT `removed.file` (`"policy.toml"`) — that field is internal data the renderer derives
        // the SECTION word from; the printed remedy is a `vike-cli config set` row write.
        let section = removed.file.trim_end_matches(".toml");
        assert!(err.contains(section), "…and the section that replaces it: {err}");
        // A row with no KEY configured something that no longer exists at all, so there is no line
        // to paste and none to assert — only the section that answers the question instead.
        if let Some(key) = removed.key {
            assert!(err.contains(key), "…and its replacement key: {err}");
        }
    }
}

/// The CLI layer cannot raise it either. `CliOverrides` has no policy field — there is nothing to
/// pass — so this sets **every** field it does have, at once, and asserts the whole `Policy` is
/// byte-identical to the row's.
#[test]
fn no_command_line_flag_can_raise_the_ceiling() {
    let root = tempfile::tempdir().unwrap();
    let cwd = project(root.path(), &[("policy", "max_notional_per_order", "5.0")]);
    let home = project_settings_dir(&cwd);
    let store = home.as_deref().map(vike_secrets::read_settings_in);

    let mut refusal_a = String::new();
    let row_only = load_with_source(
        home.as_deref(),
        StoreLayer::of(store.as_ref(), &mut refusal_a),
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .unwrap()
    .policy;

    let mut refusal_b = String::new();
    let with_cli = load_with_source(
        home.as_deref(),
        StoreLayer::of(store.as_ref(), &mut refusal_b),
        &HashMap::new(),
        &CliOverrides {
            store_root: Some("/cli/store".to_string()),
            log_dir: Some("/cli/logs".to_string()),
            datahub_addr: Some("127.0.0.1:9999".to_string()),
            log_level: Some("trace".to_string()),
            reconcile: Some(true),
        },
    )
    .unwrap()
    .policy;

    assert_eq!(with_cli, row_only, "a CLI layer changed a policy ceiling");
    assert!(
        gate(&with_cli).vet(&submit(100.0, 0.5), &[], &[]).is_some(),
        "the gate stopped refusing"
    );
}

// ---------------------------------------------------------------------------------------------
// The REMOVED rate pair — through the real project layout
// ---------------------------------------------------------------------------------------------

/// ⚠ `a_preference_above_the_ceiling_is_clamped_and_the_clamp_is_reported` stood here. It pinned
/// the model's only policy-binds-preference clamp against the real `<project>/settings` layout —
/// and the thing it was pinning turned out to bound nothing: `preferences.rate_utilization` was
/// read by NO code, so `policy.rate.max_utilization` was a ceiling over a dead value. Both are
/// removed and REFUSED by name.
///
/// This is the same test through the same resolver, asserting what the layout must do now: an
/// operator who followed the old documentation gets the row MARKED, naming the key — not a daemon
/// that trades while reporting a pacing limit it never had. (Not a hard load error: see
/// `a_mistyped_ceiling_key_marks_the_load_instead_of_silently_uncapping`'s doc for why.)
#[test]
fn the_removed_rate_pair_is_marked_through_the_real_project_layout() {
    for (section, key, value, marker) in [
        ("policy", "rate.max_utilization", "0.50", "no longer a"),
        ("preferences", "rate_utilization", "0.90", "NOTHING read it"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let cwd = project(root.path(), &[(section, key, value)]);
        let resolved = try_resolve_from(&cwd, &HashMap::new())
            .expect("a row problem MARKS rather than hard-erroring");
        let msg = resolved.seal_refusal.expect("a removed key must not load silently");
        assert!(msg.contains(key), "names the key: {msg}");
        assert!(msg.contains(marker), "says why it is gone: {msg}");
        assert!(msg.contains("DEFAULT_UTILIZATION"), "says where the number lives now: {msg}");
    }
}

// ---------------------------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------------------------

/// The walk that makes all of the above reachable: the settings directory is found from a NESTED
/// working directory, and a process started where NEITHER project marker exists finds nothing
/// rather than guessing.
///
/// The second half is the operational one, and it is the failure mode the deployment tests below
/// exist to close: a directory the walk cannot resolve is indistinguishable from an absent database —
/// both load as `Policy::default()`, i.e. UNCAPPED, without an error.
#[test]
fn the_settings_directory_is_found_by_walking_up_and_never_invented() {
    let root = tempfile::tempdir().unwrap();
    let cwd = project(root.path(), &[("policy", "max_notional_per_order", "5.0")]);
    assert_eq!(
        project_settings_dir(&cwd),
        Some(root.path().join("settings")),
        "the walk must find the project root from a nested cwd"
    );

    // Neither marker anywhere above (no `Cargo.toml`, no `settings/`) ⇒ `None` ⇒ the code defaults,
    // silently uncapped.
    let outside = tempfile::tempdir().unwrap();
    assert_eq!(project_settings_dir(outside.path()), None, "no project ⇒ no settings directory");
    let policy = load(None, &HashMap::new()).unwrap().policy;
    assert_eq!(policy.max_notional_per_order, None);
    assert_eq!(
        gate(&policy).vet(&submit(100.0, 0.5), &[], &[]),
        None,
        "with no settings there is NO cap"
    );
}

// ---------------------------------------------------------------------------------------------
// The DEPLOYED shape — a binary in a project root, no source tree anywhere above it
// ---------------------------------------------------------------------------------------------

/// Lay out a DEPLOYMENT the way `deploy/vike-tradehub.service` installs one: a `bin/`, a profile
/// and a real settings DATABASE under `settings/` — and deliberately **no `Cargo.toml` anywhere**.
/// Returns the directory the unit would set as `WorkingDirectory`.
fn deployment(root: &Path, rows: &[(&str, &str, &str)]) -> PathBuf {
    let opt_vike = root.join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
    let settings = opt_vike.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    if !rows.is_empty() {
        vike_secrets::plant_settings_rows(&settings, &stored(rows))
            .expect("plant the fixture's rows");
    }
    opt_vike
}

/// **The production bug, end to end.** All three shipped units run `WorkingDirectory=<project>`,
/// where the install recipe puts a BINARY — there is no `Cargo.toml` at or above it. A
/// `Cargo.toml`-only walk therefore resolved `None` on every deployed box, so the daemon loaded no
/// settings database at all and its order ceiling was silently absent: the same oversized order the
/// test at the top of this file proves is REFUSED in a checkout went straight through in
/// production, with nothing in the log to say why.
///
/// The `settings/` directory is the second project marker, and this is the gate on it: the same
/// row, the same loader, the same enforcing `vet` — in the deployed layout.
#[test]
fn a_ceiling_in_a_deployment_refuses_the_same_order_a_checkout_refuses() {
    let root = tempfile::tempdir().unwrap();
    let cwd = deployment(root.path(), &[("policy", "max_notional_per_order", "5.0")]);

    // The precondition that made this fail: there is genuinely no checkout marker above the box.
    assert_eq!(
        vike_model::paths::state_path::workspace_root(&cwd),
        None,
        "precondition: a deployment has no Cargo.toml — otherwise this proves nothing"
    );
    assert_eq!(
        project_settings_dir(&cwd),
        Some(cwd.join("settings")),
        "a deployment must resolve through its OWN settings/ directory"
    );

    let policy = resolve_policy_from(&cwd, &HashMap::new());
    assert_eq!(
        policy.max_notional_per_order,
        Some(CEILING),
        "the deployed row did not reach the loader — the daemon would run UNCAPPED"
    );

    let mut gate = gate(&policy);
    let reason =
        gate.vet(&submit(100.0, 0.5), &[], &[]).expect("an over-ceiling order must be refused");
    assert!(reason.contains("max_notional_per_order 5.00"), "{reason}");
    assert_eq!(
        gate.vet(&submit(4.0, 1.0), &[], &[]),
        None,
        "an order inside the ceiling must still pass"
    );
}

/// …and from a sub-directory of the deployment too — `ExecStart` runs `<project>/bin/...`, and a
/// unit or an operator can start the process one level down.
#[test]
fn a_deployment_resolves_from_below_its_root_as_well() {
    let root = tempfile::tempdir().unwrap();
    let cwd = deployment(root.path(), &[("policy", "max_notional_per_order", "5.0")]);
    let policy = resolve_policy_from(&cwd.join("bin"), &HashMap::new());
    assert_eq!(policy.max_notional_per_order, Some(CEILING));
}

/// **`VIKE_SETTINGS_DIR` beats the walk**, which is what the shipped units set so that resolution
/// does not depend on the working directory at all. Here the process is started somewhere with NO
/// marker whatsoever — the walk cannot answer — and the ceiling still binds.
///
/// The name is the one `vike_model::paths::state_path::SETTINGS_DIR_ENV` declares; it is spelled as a
/// literal here because `crates/vike-ops/tests/settings_registry.rs` harvests env-shaped literals
/// out of `tests/` too, and a read the gate can SEE is the point.
#[test]
fn the_settings_dir_override_binds_the_ceiling_with_no_marker_in_sight() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("anywhere").join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    vike_secrets::plant_settings_rows(
        &settings,
        &stored(&[("policy", "max_notional_per_order", "5.0")]),
    )
    .expect("plant the fixture's row");

    let nowhere = tempfile::tempdir().unwrap();
    assert_eq!(project_settings_dir(nowhere.path()), None, "precondition: the walk finds nothing");

    let env: HashMap<String, String> =
        [("VIKE_SETTINGS_DIR".to_string(), settings.to_string_lossy().into_owned())]
            .into_iter()
            .collect();

    // The two steps `main.rs` takes, with the override threaded exactly as it threads it.
    let home = vike_model::paths::state_path::project_settings_dir_from(
        env.get("VIKE_SETTINGS_DIR").map(String::as_str),
        nowhere.path(),
    );
    assert_eq!(home.as_deref(), Some(settings.as_path()));
    let store = home.as_deref().map(vike_secrets::read_settings_in);
    let mut refusal = String::new();
    let policy = load_with_source(
        home.as_deref(),
        StoreLayer::of(store.as_ref(), &mut refusal),
        &env,
        &CliOverrides::default(),
    )
    .expect("settings load")
    .policy;
    assert_eq!(policy.max_notional_per_order, Some(CEILING));
    assert!(
        gate(&policy).vet(&submit(100.0, 0.5), &[], &[]).is_some(),
        "the override's ceiling must bite"
    );
}

/// A BLANK override is ignored rather than honoured — an empty `Environment=VIKE_SETTINGS_DIR=`
/// line in a unit must not resolve settings to `""` and read them out of the working directory.
#[test]
fn a_blank_settings_dir_override_falls_through_to_the_walk() {
    let root = tempfile::tempdir().unwrap();
    let cwd = deployment(root.path(), &[("policy", "max_notional_per_order", "5.0")]);
    for blank in ["", "   "] {
        let home = vike_model::paths::state_path::project_settings_dir_from(Some(blank), &cwd);
        assert_eq!(home.as_deref(), Some(cwd.join("settings").as_path()));
        let policy = resolve_policy_from(&cwd, &HashMap::new());
        assert_eq!(policy.max_notional_per_order, Some(CEILING));
    }
}
