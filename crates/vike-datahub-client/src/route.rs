//! `route` — WHERE a reader gets history from, decided once for everybody who asks.
//!
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` gave the hist store ONE reader — the
//! datahub — and everything else asks over the wire. That leaves every other reader with the same
//! two-armed question (the files here, or the datahub there?), and the same answer: a flag on the
//! line opts out, nothing else does.
//!
//! # ⚠ Why the decision lives in the CLIENT crate
//!
//! It was `crates/vike-backtest/src/backtest_cli.rs`'s until 2026-09-23, because the compute daemon
//! and the three `cheap_np` bins were the only callers and they are all that crate's. `vike-report`
//! is the fourth reader 0084 still owes, and it cannot name `vike-backtest`: both declare layer 45
//! and `crates/vike-ops/tests/layer_gate.rs` refuses a same-rank edge. So the choice was a second
//! spelling or a shared home BELOW both — and `CLAUDE.md` states the rule outright: *when two sides
//! must not disagree, the cure is a shared crate BELOW both, not a shared crate containing both*.
//!
//! The arithmetic picked the home rather than taste. A crate that resolves this question must name
//! whatever implements the wire arm, which is this crate at layer 30, so its floor is 35 — and the
//! tiers there are named `user-host` (35) and `venue` (40), neither of which describes a routing
//! decision. `docs/decisions/0085-the-rank-follows-the-declaration-not-the-role.md` ruled that a
//! correctly-named crate does not move into a wrongly-named tier, which leaves this crate: it is
//! below both readers already, and it owns half the body outright ([`crate::RemoteHistStore`] and
//! [`vike_node_proto::auth::node_keys_from_vars`]).
//!
//! # ⚠ The feature is why that costs the other consumers nothing
//!
//! The LOCAL arm opens a `DataFusionHist`, so this module forwards `vike-data/hist-datafusion` —
//! and nine crates take this one, including `vike-studio`, whose `studio-standalone` lane
//! (`scripts/ci_feature_suite.sh`) asserts STRUCTURALLY that a default Studio normal-dep tree
//! carries no datafusion crate at all. So the module sits behind `hist-route`, off by default: the
//! consumers that route enable it, and the ones that only dial the datahub are byte-identical to
//! before it existed. `vike-backtest` reaches it through `datafusion-store`, which is the feature
//! its `backtest_cli` module was already gated on, so that crate gained no configuration from the
//! move; `vike-report` will reach it through `hist`, which is the feature its `--store` path is
//! already gated on, and will gain none either.
//!
//! ⚠ **The move landed on its own, ahead of the reader that motivated it**, in this tree's own
//! two-step idiom: this file is byte-identical in behaviour to the code it left `vike-backtest`,
//! so a red CI here is a red about the MOVE and about nothing else. The tearsheet's routing is a
//! separate change against it.
//!
//! # ⚠ This module's tests do not live here, and a test ADDED here would not run
//!
//! The six that cover the decision stayed in `crates/vike-backtest/src/backtest_cli.rs`'s
//! `history_route_tests`, where the `backtest-datafusion-store` lane
//! (`scripts/ci_feature_suite.sh`) executes them on every PR. Moving them down with the code would
//! have SILENTLY turned all six off: the roster lane builds this crate with DEFAULT features, so it
//! compiles none of this file, and the lanes that do enable `hist-route` reach it as a DEPENDENCY,
//! where cargo compiles no `#[cfg(test)]` module at all. There is no
//! `-p vike-datahub-client --features hist-route` lane today, so this file has no configuration in
//! which its own unit tests would execute — a `#[cfg(test)] mod` added below would read green by
//! never running, which is the failure this paragraph exists to prevent. Add the lane first, or put
//! the test beside the caller that proves the behaviour.

use std::path::Path;

use vike_data::{DataFusionHist, HistStore};

/// WHERE a reader gets history from — the decision alone, so it can be tested without binding a
/// socket and without opening a store.
///
/// ⚠ **The default is the WIRE** (`docs/decisions/0084-only-the-datahub-touches-the-store.md`), and
/// only `--store` ON THE LINE opts out. The environment deliberately does NOT choose: the deployed
/// unit sets `VIKE_HIST_STORE`, so leaving that in charge would keep the live daemon local and the
/// change would prove nothing where it matters. A flag on the line is a decision somebody made for
/// one invocation; an inherited variable is one nobody remembers making.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRoute {
    /// `--store DIR` was given: open the files here.
    Local,
    /// The default: ask the datahub at this address.
    Wire(String),
}

impl HistoryRoute {
    /// What to PRINT for this route — a path only when there is a local one to name.
    ///
    /// ⚠ On the wire arm it names the ADDRESS and never a directory. The resolved root is the
    /// SERVER's, so a path printed client-side is a guess about another box's filesystem —
    /// `crates/vike-cli/src/cmd/data.rs`'s remote arm withholds it for the same reason, and the
    /// disclosure in `crates/vike-backtest/src/backtest_cli.rs`'s `run_serve` says the same words
    /// on its wire arm.
    ///
    /// ⚠ **It is for a caller with ONE disclosure site covering both arms** — the `cheap_np` bins,
    /// which print the route once into a JSON field and once onto a human line. `run_serve`
    /// deliberately does NOT call it: its arms carry different prose (the local one adds why it is
    /// local), so a shared renderer would have to take a path the wire arm has no honest value
    /// for. This is not a second spelling of the ROUTE — `history_route` below is still the one
    /// decision — only of how a route reads out loud.
    pub fn label(&self, local_root: &Path) -> String {
        match self {
            Self::Local => local_root.display().to_string(),
            Self::Wire(hub) => format!("the datahub at {hub}"),
        }
    }
}

/// `VIKE_DATAHUB_ADDR` — where the bins dial when no `--store` opted them out.
///
/// ⚠ **This crate spells its own constant instead of importing `vike_config`'s, and that is not
/// duplication.** `vike_ops::scan` resolves constants CRATE-WIDE, so a crate that imports the name
/// from another crate goes INVISIBLE to the settings-registry sweep and its read then passes
/// `every_read_variable_is_declared` by BLINDNESS rather than by declaration. The price of the
/// second spelling is the equality assertion that pays for it —
/// `datahub_addr_env_matches_the_config_crate` below, the shape
/// `crates/vike-app-core/src/backend_registry.rs`'s
/// `observe_and_control_key_names_match_the_client` set.
pub const DATAHUB_ADDR_ENV: &str = "VIKE_DATAHUB_ADDR";

/// The address rung available to a BARE BIN — the environment, and nothing above it.
///
/// ⚠ It is deliberately SHORTER than the compute daemon's ladder, which starts at
/// `settings.config.datahub_addr`. A `cheap_np` bin loads no settings file at all, so that rung is
/// not one it can reach, and inventing a settings load for it would give two answers to "where is
/// my datahub" on one box. Both feed the SAME [`history_route`], which owns the blank-value filter
/// and [`vike_config::DEFAULT_DATAHUB_ADDR`]: one decision, two sources.
pub fn datahub_addr_for_bin(vars: &std::collections::HashMap<String, String>) -> Option<&str> {
    vars.get(DATAHUB_ADDR_ENV).map(String::as_str)
}

/// Resolve [`HistoryRoute`] from the flag and the configured address.
///
/// A BLANK configured address is treated as absent rather than dialled — the same rule
/// `vike_config` applies everywhere a string names a peer, so an empty settings line cannot produce
/// a connect to `""`.
pub fn history_route(local_store: Option<&Path>, datahub_addr: Option<&str>) -> HistoryRoute {
    if local_store.is_some() {
        return HistoryRoute::Local;
    }
    HistoryRoute::Wire(
        datahub_addr
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(vike_config::DEFAULT_DATAHUB_ADDR)
            .to_string(),
    )
}

/// Open the history the route names — the BINS' half of `0084`, and the one call that turns a
/// [`HistoryRoute`] into something with `scan_*` on it.
///
/// ⚠ **Node keys come from the credential STORE, not the environment**, through the same
/// `resolve_node_keys` + `is_datahub_node_key` pair that
/// `crates/vike-backtest/src/backtest_cli.rs`'s `run_serve` uses. An env-only read was the
/// tempting shape for a bare research bin and it is the wrong one HERE: both boxes migrated to the
/// settings database on 2026-09-14, so the datahub pair is a row rather than a shell export, and
/// an env read would resolve `None`, connect unauthenticated, and be refused by the keyed server
/// with a message about auth rather than about where the keys were looked for.
///
/// ⚠ **An UNREADABLE store WARNS and continues rather than refusing.** That is the opposite of
/// the disposition of `crates/vike-backtest/src/backtest_cli.rs`'s `run_serve`, and the difference
/// is real: a server decides from that read whether it may bind at all, while a bin has no
/// listener and no such decision — and against a KEY-LESS
/// dev datahub an unreadable store is harmless, so refusing would block a working configuration.
/// The warning names the cause, which is the half that would otherwise be lost: without it the
/// operator sees only the datahub's own refusal and cannot tell a permissions bug from a missing
/// key. An ABSENT store is the ordinary unconfigured state and says nothing, exactly as everywhere
/// else in this workspace.
///
/// ⚠ **The wire arm makes every `scan_*` a network read**, and these callers scan a whole series
/// per entry with `TsRange::all()`. That is the cost `0084` buys the single-reader property with;
/// `--store DIR` is the documented way back to local files when the data daemon is down or the
/// transfer is not worth it.
pub fn open_routed_history(
    route: &HistoryRoute,
    local_root: &Path,
    vars: &std::collections::HashMap<String, String>,
) -> Result<Box<dyn HistStore + Send + Sync>, String> {
    match route {
        HistoryRoute::Local => match DataFusionHist::open(local_root) {
            Ok(h) => Ok(Box::new(h)),
            Err(e) => Err(format!("open hist store at {}: {e}", local_root.display())),
        },
        HistoryRoute::Wire(hub) => {
            let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
            let keys = match vike_secrets::resolve_node_keys(
                settings_override,
                vike_model::credential_keys::is_datahub_node_key,
            ) {
                Ok((resolved, _source)) => {
                    vike_node_proto::auth::node_keys_from_vars(&resolved.secrets.into_map())
                }
                Err(e) => {
                    eprintln!(
                        "credential store PRESENT but UNREADABLE ({e}) — any configured \
                         datahub node keys were NOT loaded, so this run will connect to {hub} \
                         UNAUTHENTICATED. Against a keyed datahub that connect is refused, and \
                         the cause is that store rather than the keys: fix its permissions \
                         and re-run"
                    );
                    None
                }
            };
            // `with_keys` whenever a pair resolved, `new` only when none did — `with_keys` degrades
            // to a plain unauthenticated connect against a KEY-LESS server, so one spelling works
            // against the keyed production datahub and a bare dev one. `run_serve` chooses the same
            // way, for the reason stated there.
            Ok(match keys {
                Some(k) => Box::new(crate::RemoteHistStore::with_keys(hub, k)),
                None => Box::new(crate::RemoteHistStore::new(hub)),
            })
        }
    }
}
