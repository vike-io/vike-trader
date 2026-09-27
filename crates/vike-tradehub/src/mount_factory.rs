//! RUNTIME strategy-mount resolution (split-plane B5) — the daemon's
//! [`vike_core::CoreConfig::strategy_factory`], built on the SAME `DaemonProfile` machinery a
//! `[strategy]` table faces at load, so a wire [`MountSpec`] meets the profile vocabulary's OWN
//! refusals (unknown/simulator-only name, unread or mistyped params keys, name-XOR-rhai, the
//! script-path rules of docs/decisions/0024-rhai-strategies-live.md) rather than a parallel,
//! drift-prone re-implementation.
//!
//! The reuse is LITERAL: [`profile_for`] renders the spec as a one-mount profile document and runs
//! it through [`DaemonProfile::from_toml_str`] — the one public entry that validates — then
//! [`resolve_spec`] resolves through [`DaemonProfile::resolve_strategy`], the same call `main`
//! makes for the spawn-time mount. Two deliberate narrowings for the RUNTIME path:
//!
//! - **The A-S maker names ([`AS_MAKER_NAMES`]) are refused.** The maker is built from a profile's
//!   own top-level maker fields (`qty`/`tick_size`/the venue-selected price domain …), which a
//!   mount request does not carry — resolving one here would mount a maker on GUESSED grid knobs,
//!   the exact silently-wrong-configuration class `DaemonProfile::validate_strategy` exists to
//!   refuse. Mount the maker through the daemon profile instead.
//! - **A `[strategy]` selection is REQUIRED** (`name` XOR `rhai`): the absent-table "default to
//!   the A-S maker" arm is a spawn-time convenience, not a runtime one, for the same reason.
//!
//! Validation runs TWICE by design: once at the server edge ([`validate_spec`] from
//! `server::lower_command`, so a remote peer gets a `Response::Error` carrying the real message)
//! and again inside the factory on the core's fold thread — the edge is UX, the factory is the
//! authority (an in-process caller skips the edge entirely).

use vike_core::LiveBroker;
use vike_exec::MountSpec;

use crate::config::{AS_MAKER_NAMES, DaemonProfile};

/// Validate a runtime mount spec with the daemon's profile refusals, resolving nothing. Pure over
/// the spec (no filesystem read: a `rhai` PATH's existence/compile is checked at resolve, exactly
/// like a profile load vs. its resolve — see `StrategyCfg::rhai`'s doc for that split).
pub fn validate_spec(spec: &MountSpec) -> Result<(), String> {
    profile_for(spec).map(|_| ())
}

/// Resolve a runtime mount spec into the strategy object to mount — validate ([`profile_for`]),
/// then the SAME [`DaemonProfile::resolve_strategy`] the spawn-time mount goes through (registry
/// and user-registry names at [`vike_core::LiveBroker`]; `rhai` scripts compiled with the audit
/// line + sha256 that resolve logs).
pub fn resolve_spec(
    spec: &MountSpec,
) -> Result<Box<dyn vike_model::Strategy<LiveBroker> + Send>, String> {
    let profile = profile_for(spec)?;
    // The maker-config argument is DEAD on every path that can reach it: `resolve_strategy`'s
    // maker arm fires only for AS_MAKER_NAMES / an absent `[strategy]` table, both refused in
    // `profile_for`. The registry/script arms read `[strategy]` alone.
    let cfg =
        vike_run::MakerMountConfig::crypto(spec.venue.clone(), spec.symbol.clone(), 0.01, 1.0);
    profile.resolve_strategy(&cfg)
}

/// The daemon's [`vike_core::StrategyFactory`]: [`resolve_spec`] boxed. Runs on the core's fold
/// thread inside the `Command::MountStrategy` arm (an occasional operator verb — the
/// `CoreConfig::strategy_factory` doc carries the latency argument).
pub fn strategy_factory() -> vike_core::StrategyFactory {
    Box::new(|spec: &MountSpec| resolve_spec(spec))
}

/// What [`resurrect_runtime_mounts`] did: how many recorded mounts were re-sent into the core's
/// command lane, and how many were skipped at the validation edge. Counts, not verdicts — a SENT
/// record can still refuse INSIDE the core (duplicate id, venue-less engine), where the refusal
/// is a recent-events note exactly as it would be for a wire mount.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResurrectOutcome {
    /// Records re-sent as `Command::MountStrategy` (the core decides the rest).
    pub sent: usize,
    /// Records skipped at the edge — [`validate_spec`] refused (logged per record).
    pub skipped: usize,
}

/// Resurrect RUNTIME strategy mounts from the topology sidecar (split-plane B5, residual
/// closed): read `vike_core::mount_topology`'s records under `state_dir` and re-send each as an
/// ordinary [`vike_exec::Command::MountStrategy`] through `send` — the SAME command a wire mount
/// lowers into, so every record meets the same resolution and the same refusals it met the first
/// time. The split mirrors the wire path exactly: each record is edge-validated with
/// [`validate_spec`] first (the check `server::lower_command` applies to a remote peer's spec —
/// a record that no longer validates is SKIPPED with a `warn!` naming the mount and the reason),
/// and a record that passes the edge still faces the core arm's own authority checks (duplicate
/// id, engine-less venue, the factory's resolve), whose refusals surface as recent-events notes.
/// NOTHING here can fail a boot: a corrupt topology file reads as empty inside
/// `vike_core::mount_topology::read` (loudly), an absent one silently, and this function returns
/// counts, never an error.
///
/// ORDERING CONTRACT (the call-site rule, stated here because both daemon arms must obey it):
/// call this AFTER the core spawned and BEFORE the feeds arm. The ingest lane is FIFO
/// ([`vike_core::CoreHandle::send_command`] is the lossless lane), so every resurrected mount
/// folds ahead of the first market message — and the mount arm loads the mount's
/// `<mount_id>.json` strategy-state sidecar as part of mounting, so a resurrected strategy has
/// its saved state before any tick can reach it.
pub fn resurrect_runtime_mounts(
    state_dir: &std::path::Path,
    mut send: impl FnMut(vike_exec::Command),
) -> ResurrectOutcome {
    let mut outcome = ResurrectOutcome::default();
    for record in vike_core::mount_topology::read(state_dir) {
        let id = record.mount_id();
        match validate_spec(&record.spec) {
            Err(reason) => {
                tracing::warn!(
                    target: "vike_tradehub::mount_factory",
                    mount_id = %id,
                    %reason,
                    "skipping a recorded runtime mount that no longer validates — the daemon \
                     boots without it; fix the spec (or unmount intent) and re-mount by hand"
                );
                outcome.skipped += 1;
            }
            Ok(()) => {
                tracing::info!(
                    target: "vike_tradehub::mount_factory",
                    mount_id = %id,
                    venue = %record.spec.venue,
                    symbol = %record.spec.symbol,
                    "resurrecting runtime strategy mount from the topology sidecar"
                );
                send(vike_exec::Command::MountStrategy(Box::new(record.spec)));
                outcome.sent += 1;
            }
        }
    }
    outcome
}

/// Render the spec as a one-mount `DaemonProfile` document and run the profile's FULL validation —
/// the one reuse point everything above shares.
fn profile_for(spec: &MountSpec) -> Result<DaemonProfile, String> {
    // The runtime-path narrowings (module doc): a selection is required, and the A-S maker names
    // are refused with the actionable alternative.
    if let Some(name) = spec.name.as_deref()
        && AS_MAKER_NAMES.contains(&name)
    {
        return Err(format!(
            "strategy {name:?} cannot be runtime-mounted: the A-S maker is built from a \
                 daemon profile's own top-level maker fields (`qty` / `tick_size` / the \
                 venue-selected price domain), which a mount request does not carry — mounting it \
                 here would run the maker on guessed grid knobs. Mount it through the daemon's \
                 profile instead."
        ));
    }
    if spec.name.is_none() && spec.rhai.is_none() {
        return Err("a runtime mount must say WHAT to mount: set `name` (a registry strategy) or \
                    `rhai` (a script path on the node)"
            .to_string());
    }
    // `[strategy.params]`: the wire carries JSON; the profile vocabulary is TOML. `toml::Value`
    // deserializes from the JSON value directly; what JSON can say that TOML cannot (a `null`) is
    // a refusal, never a silent drop.
    let params: toml::Value = serde_json::from_value(spec.params.clone())
        .map_err(|e| format!("`params` does not translate to a TOML table: {e}"))?;
    let mut strategy = toml::value::Table::new();
    if let Some(n) = &spec.name {
        strategy.insert("name".to_string(), toml::Value::String(n.clone()));
    }
    if let Some(r) = &spec.rhai {
        strategy.insert("rhai".to_string(), toml::Value::String(r.clone()));
    }
    strategy.insert("params".to_string(), params);
    let mut root = toml::value::Table::new();
    root.insert("venue".to_string(), toml::Value::String(spec.venue.clone()));
    root.insert("symbol".to_string(), toml::Value::String(spec.symbol.clone()));
    root.insert("interval".to_string(), toml::Value::String(spec.interval.clone()));
    root.insert("strategy".to_string(), toml::Value::Table(strategy));
    let text = toml::to_string(&toml::Value::Table(root))
        .map_err(|e| format!("render mount spec as a profile document: {e}"))?;
    DaemonProfile::from_toml_str(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: Option<&str>, rhai: Option<&str>, params: serde_json::Value) -> MountSpec {
        MountSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            account: None,
            controller_id: Some("rt-a".into()),
            name: name.map(str::to_string),
            rhai: rhai.map(str::to_string),
            params,
        }
    }

    /// A live-capable registry strategy with well-formed params validates AND resolves.
    #[test]
    fn a_registry_strategy_resolves() {
        let s = spec(Some("buy_hold"), None, serde_json::json!({}));
        validate_spec(&s).expect("buy_hold is live-capable with default params");
        resolve_spec(&s).expect("resolves to a boxed strategy");
    }

    /// The profile vocabulary's own refusals reach the wire caller VERBATIM — an unknown name and
    /// an unread params key each refuse with `DaemonProfile::validate`'s message, not a re-spelled
    /// one.
    #[test]
    fn profile_refusals_surface_verbatim() {
        let unknown = validate_spec(&spec(Some("no_such_strategy"), None, serde_json::json!({})))
            .expect_err("unknown name refuses");
        assert!(unknown.contains("unknown strategy"), "{unknown}");

        let unread = validate_spec(&spec(Some("buy_hold"), None, serde_json::json!({"qtyy": 0.5})))
            .expect_err("an unread params key refuses");
        assert!(unread.contains("does not read these"), "{unread}");
    }

    /// The runtime-path narrowings: the A-S maker names refuse with the actionable alternative,
    /// and a spec naming neither `name` nor `rhai` refuses (no default-maker arm at runtime).
    #[test]
    fn runtime_narrowings_refuse() {
        for name in AS_MAKER_NAMES {
            let e = validate_spec(&spec(Some(name), None, serde_json::json!({})))
                .expect_err("A-S maker names cannot runtime-mount");
            assert!(e.contains("cannot be runtime-mounted"), "{e}");
        }
        let e = validate_spec(&spec(None, None, serde_json::json!({})))
            .expect_err("a selection is required");
        assert!(e.contains("must say WHAT to mount"), "{e}");
    }

    /// Both-set refuses through the profile's own name-XOR-rhai rule, and a `null` params refuses
    /// at the JSON→TOML boundary rather than silently dropping.
    #[test]
    fn malformed_specs_refuse() {
        let both = validate_spec(&spec(Some("buy_hold"), Some("x.rhai"), serde_json::json!({})))
            .expect_err("name XOR rhai");
        assert!(both.contains("not both"), "{both}");

        let null = validate_spec(&spec(Some("buy_hold"), None, serde_json::Value::Null))
            .expect_err("null params refuse");
        assert!(null.contains("TOML"), "{null}");
    }
}
