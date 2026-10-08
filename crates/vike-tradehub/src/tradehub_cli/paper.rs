//! The PAPER arm's phases before its mount: venues, wire seeds, `data_only` refusal, banner.

use std::process::ExitCode;

use crate::ResolvedMount;
use vike_mount::MakerMountConfig;

use super::mount_rows::{WireMountSeed, mounts_wire_params};

/// The DISTINCT venues across the mount set, in mount order — see the comment at [`run`]'s call
/// for why this vec is a log line and nothing else.
pub(super) fn distinct_mount_venues(resolved: &[ResolvedMount]) -> Vec<String> {
    let mut mount_venues: Vec<String> = Vec::new();
    for m in resolved {
        if !mount_venues.contains(&m.cfg.venue) {
            mount_venues.push(m.cfg.venue.clone());
        }
    }
    mount_venues
}

/// One [`WireMountSeed`] per resolved mount — the half of each `StrategyStatus` wire row that
/// exists BEFORE the mount runs. [`ready_banner`] completes them into rows once the arming record
/// does.
pub(super) fn wire_mount_seeds_for(
    resolved: &[ResolvedMount],
    multi: bool,
    strategy_name: &str,
    strategy_params: &str,
) -> Vec<WireMountSeed> {
    resolved
        .iter()
        .map(|m| WireMountSeed {
            // A `[[mounts]]` daemon publishes one SELF-ADDRESSED row per mount (the `params`
            // string opens with the mount's own venue/symbol/interval — the OLD route to a
            // mount's key, kept for the human-readable line now that the row struct carries the
            // structured fields; see `mounts_wire_params`); a single-mount daemon's one row
            // carries the identity block's own two strings — byte-identical to the row
            // `server.rs`'s `Request::StrategyStatus` arm used to DERIVE from the identity when
            // this daemon published no rows at all. Only that row's `live` changes, and it
            // changes from a lie to a fact.
            strategy: if multi {
                m.row.strategy_name().to_string()
            } else {
                strategy_name.to_string()
            },
            params: if multi { mounts_wire_params(m) } else { strategy_params.to_string() },
            venue: m.cfg.venue.clone(),
            asset_class: m.row.asset_class.clone(),
        })
        .collect()
}

/// **Paper-mount phase — refuse `data_only` on a PAPER daemon.** An `Err` carries the exit code
/// [`run`] returns.
pub(super) fn refuse_data_only_on_paper(resolved: &[ResolvedMount]) -> Result<(), ExitCode> {
    // The data-only declaration is a LIVE-mount fact (it tells `live_mount` which venue's
    // credentials to withhold from exec) — the paper daemon mounts no venue feed and exec is
    // already paper, so a declared key here would configure NOTHING while reading as real,
    // the declared-but-unread failure the settings rule exists for. Refuse, like the removed
    // env variables do, rather than warn: an operator who wrote it believes it decides
    // something about THIS run.
    if resolved.iter().any(|m| m.row.data_only_effective()) {
        let msg = "the profile declares `data_only = true` but the live gate is OFF — the \
                   PAPER daemon mounts no venue feed and exec is already paper, so the key \
                   configures nothing here. Drop it, or arm the live gate (`vike-cli config set \
                   flags.tradehub_live true`, or VIKE_TRADEHUB_LIVE=1) for the data-only live \
                   mount";
        tracing::error!("{msg}");
        eprintln!("vike-tradehub: {msg}");
        return Err(ExitCode::FAILURE);
    }
    Ok(())
}

/// **Paper-mount phase — say what is about to be mounted and which sentinel it watches.** The
/// startup line (venue, token, interval, strategy and its echoed params) and the HALT-sentinel
/// advisory, in that order.
pub(super) fn announce_paper_mount(
    mount_venues: &[String],
    cfg: &MakerMountConfig,
    resolved: &[ResolvedMount],
    strategy_name: &str,
    strategy_params: &str,
    profile: &crate::config::DaemonProfile,
) {
    // ⚠ NO `qty` AND NO `resolution_ts` FIELD HERE — their absence IS the fix, not an omission.
    //
    // Both are A-S MAKER knobs read off `MakerMountConfig`, and this daemon no longer mounts
    // only that maker. On a `[strategy] name = "grid"` mount NOTHING reads either one, so
    // `qty = cfg.qty` printed the A-S default beside `strategy=grid` while the grid's real size
    // sat in `strategy_params`. MEASURED: a grid profile with `[strategy.params] size = 2.0`
    // logged `… qty=20.0 strategy=grid strategy_params=… size=2 …`. `qty` is the field an
    // operator scans for ORDER SIZE, so that line made a positive claim about a knob that
    // configures nothing — the same class as echoing the raw `[strategy.params]` table, which
    // `DaemonProfile::effective_params`' own doc records this daemon having done and undone.
    //
    // Nothing is lost where the claim IS true: on a maker mount `effective_params` reports
    // `qty=…` and `resolution_ts=…` INSIDE `strategy_params` (its A-S branch), so both still
    // print — from the one place that knows which strategy was mounted, instead of from a second
    // copy that cannot. That is also why the cure is deletion rather than a relabel: a second
    // field would be a second authority for the same number, and the two can then disagree.
    // `crates/vike-tradehub/tests/daemon/any_strategy_mount.rs`'s
    // `the_mount_line_states_order_size_only_where_it_is_true` pins both directions.
    tracing::info!(
        venue = %mount_venues.join("+"),
        token = %cfg.token_id,
        interval = %cfg.interval,
        mounts = resolved.len(),
        strategy = %strategy_name,
        strategy_params = %strategy_params,
        summary_ms = profile.daemon.summary_ms,
        "mounting the PAPER strategy (headless daemon; no live feed — `vike-cli config set \
         flags.tradehub_live true`, or VIKE_TRADEHUB_LIVE=1, for the live build_node core)"
    );
    // ⚠ SAY OUT LOUD WHICH SENTINEL THIS MOUNT IS WATCHING.
    //
    // This line said the OPPOSITE until the paper mount was armed, and the reversal is the
    // point of the comment. It used to read "the HALT sentinel reaches NOTHING on a PAPER
    // mount", argued from a grep that returned nothing for `halt` anywhere under
    // `crates/vike-paper/src/`, and every word of it was true when it was written. It became
    // false in the commit that armed `crates/vike-mount/src/run/paper.rs`'s `paper_client_for` — and a
    // startup advisory that survives the fix it describes is WORSE than no advisory, because an
    // operator reads a `warn!` in today's journal as a statement about today's binary. So this
    // is the fix and the tombstone in one.
    //
    // What was always wrong, and is what this line is actually for, is the SILENCE. A PAPER
    // mount builds no `ExecActor`, so nothing calls `halt::halt_path_from_env()` at spawn and
    // nothing emits the arming report `docs/ops/kill-switches.md` section C tells an operator to
    // grep for. An absent report is ALSO what an older binary and a not-yet-mounted venue look
    // like, so the three were indistinguishable at exactly the moment the difference matters —
    // the shape `crates/vike-bridge-core/src/halt.rs` exists to refuse: a kill switch that is
    // not armed looks exactly like one that is, right up until it is needed.
    //
    // It NAMES THE PATH now, and that reversal is deliberate too. The old comment refused to
    // print one on the grounds that "a printed path reads as a working target rather than an
    // inert one" — correct while the mount was inert, and exactly backwards now: the path IS
    // the working target, it is the file an operator has to `touch`, and the resolution is a
    // precedence (`<project>/settings/state/HALT`, else `<exe_dir>/HALT`; a `VIKE_HALT_FILE`
    // override outranked both until decision 0099 retired it) that they cannot compute from the
    // unit file alone. `halt_path_from_env` is the same
    // memoized resolver the mount itself uses (`vike_bridge_core::halt`), so this cannot print a
    // different answer from the one being enforced, and it adds NO new `env::var` call site —
    // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` is untouched.
    //
    // `warn!`, not `info!`: an operator scanning for what stops trading must find this without
    // knowing to look for it.
    //
    // Bound to a NAMED const so `crates/vike-ops/tests/docs/kill_switch_gate.rs` can key its row on
    // an identifier instead of on this prose: the wording will be improved, and a gate keyed on
    // the message text would either break on every reword or (worse) be quietly loosened until
    // deleting the line stopped reddening it. Deleting `PAPER_MOUNT_HALT_ADVISORY` is what must
    // fail the gate.
    // ⚠ The tail used to read "This mount emits no ExecActor arming report … so this line is the
    // report", and that inference was FALSE — the second false claim this one advisory has
    // carried. No `ExecActor` is built here, true; but the report is emitted by
    // `halt_path_from_env`'s own one-per-process resolution, and the `halt_sentinel` field below
    // IS a call to it. So the `NOT ARMABLE` error reaches a paper node too, which is the half an
    // operator rehearsing the switch is told to check for.
    // ⚠ That last sentence follows from the CALL GRAPH, not from a measurement — and the draft
    // that replaced the false claim above carried a false citation of its own, "MEASURED
    // read-only on the CI box's PAPER tradehub". the CI box's journal does not say that: it holds exactly
    // one `vike_bridge_core::halt` line, from the LIVE bybit mount's pid, while eleven distinct
    // pids logged a paper mount and none emitted one. Their silence is not counter-evidence
    // (they predate this advisory) — it is why the argument may not be a journal grep. The third
    // false claim on one advisory would have been a citation, which is the kind a reader trusts
    // most.
    const PAPER_MOUNT_HALT_ADVISORY: &str = "HALT kill switch is ARMED on this PAPER mount: `touch` the sentinel below and every \
         order that OPENS risk is refused (a reduce_only submit still passes, so you can always \
         flatten). This mount builds no venue adapter, so nothing else here would name the file \
         — the halt_sentinel field is that name, and resolving it emits the usual \
         vike_bridge_core::halt arming report beside this line \
         (docs/ops/kill-switches.md section C).";
    tracing::warn!(
        halt_sentinel = %vike_bridge_core::halt::halt_path_from_env().display(),
        "{PAPER_MOUNT_HALT_ADVISORY}"
    );
}
