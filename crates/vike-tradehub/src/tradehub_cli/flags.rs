//! Resolved settings flags folded into the venue map (`fold_flags_into_vars`) and the journal map.

use std::collections::HashMap;

use super::process_env;

/// `"1"` / `"0"` — the exact grammar every one of these gates parses, and the one this daemon
/// writes when it folds a resolved flag into a map a library will read.
///
/// `"0"` rather than "leave the key out" for a `false`, deliberately: the two are equivalent to
/// every reader here (all of them compare against `"1"`), and writing the value makes the map an
/// honest record of what the boot decided rather than a record of what it decided to mention.
pub(super) fn flag_wire_value(on: bool) -> String {
    if on { "1" } else { "0" }.to_string()
}

/// How the fold writes ONE key when `vars` ALREADY carries it — the credential store's own
/// `.env` being the only thing that can have put it there.
///
/// ⚠ **This has ONE variant, and it had two until decision 0095's review.** The second one,
/// `CredentialStoreFirst`, left a credential line standing and was reserved for the two Polymarket
/// arming gates, on the argument that [`vike_config::refuse_credential_file_arming`] refuses to
/// START on an arming line in that file. The argument had a hole: that refusal's value grammar is
/// "the text before `#`, trimmed, is exactly `1`", while the gate's reader
/// (`vike_polymarket::poly_exec_enabled`) takes the FIRST TOKEN — so a store row
/// `POLY_EXEC=1 x` armed REAL-MONEY exec over `flags.poly_exec = false` without tripping it, and a
/// `POLY_EXEC=0` row silently vetoed `flags.poly_exec = true`. Both gates are `Resolved` now: the
/// flag is the SOLE source, and no credential row can arm or veto them.
/// `no_credential_store_line_survives_the_fold_for_any_key` holds that for every key.
///
/// The enum stays, single-variant, only because [`vike_config::CONSUMPTION`]'s six needles spell
/// `FoldTier::Resolved)` textually; collapsing the type re-keys those needles and is a change of its
/// own (decision 0095's prose sweep left it, as a code change rather than prose).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FoldTier {
    /// **OVERWRITE.** The resolved flag is written whatever `vars` held, so this key's value in
    /// the map IS `vike_config::Flags`' answer — file, with the environment applied over it.
    ///
    /// Leaving a store line standing would make the credential store a THIRD source that outranks an
    /// exported variable, in no precedence model, invisible to `vike-cli config show` — and for
    /// [`vike_config::flags::ALLOW_WITHDRAW_KEYS_ENV`] and
    /// [`vike_config::flags::PREFLIGHT_SKIP_ENV`] that source would be widening a SAFETY OVERRIDE,
    /// while for the two Polymarket gates it would be arming (or vetoing) real-money exec from a
    /// plaintext credential file.
    Resolved,
}

/// Every key [`fold_flags_into_vars`] writes, with the resolved value and the TIER it is written
/// under — the table both the fold and its tests iterate, so neither can describe a set the other
/// does not have.
pub(super) fn folded_flag_rows(flags: vike_config::Flags) -> [(&'static str, bool, FoldTier); 6] {
    use vike_config::flags as f;
    [
        // Polymarket's two mount gates. Both readers are MAP-ONLY since decision 0095
        // (`vike_polymarket::exec_plane::mount::poly_exec_enabled`,
        // `vike_polymarket::exec_plane::recon_client::poly_reconcile_enabled`): this fold is the only way
        // `flags.poly_exec` / `flags.poly_reconcile` reach them, and it OVERWRITES. ⚠ It used to
        // `or_insert` on the argument that a credential-store spelling "is refused at boot" — but that
        // refusal only catches a value whose text before `#` is exactly `1`, while the readers take the
        // first TOKEN, so `POLY_EXEC=1 x` armed real-money exec over a false flag and `POLY_EXEC=0`
        // vetoed a true one. The flag is the sole source now; the refusal stays as the loud half.
        (f::POLY_EXEC_ENV, flags.poly_exec, FoldTier::Resolved),
        (f::POLY_RECONCILE_ENV, flags.poly_reconcile, FoldTier::Resolved),
        // Hyperliquid's symbology universe — read inside
        // `vike_hyperliquid::instruments::HyperliquidInstruments::load_from_vars`, which had no
        // credential-store tier before that constructor existed.
        (f::HYPERLIQUID_HIP3_ENV, flags.hyperliquid_hip3, FoldTier::Resolved),
        // The PIT `SymbolProperties` recorder's gate. ⚠ This named "`open_from_vars` at the
        // construction site below" as its reader until 2026-09-28, and that call is not made: the
        // daemon has passed `properties_rec: None` since it stopped writing the store (#2093), so
        // nothing in this binary reads the value this row folds. `Resolved` is still the tier the
        // NEXT reader needs: `open_from_env` is a one-line wrapper that calls `open_from_vars` over
        // a snapshot of the PROCESS ENV, so a map handed to the map half has to carry the
        // environment's answer or the swap loses it in both directions.
        (f::RECORD_PROPERTIES_ENV, flags.record_properties, FoldTier::Resolved),
        // ⚠ The two SAFETY OVERRIDES, where `false` is the guarded state. They are folded like any
        // other flag, by the owner's ruling that every setting is editable from the UI including
        // the live gates — and what makes that safe is the tier, not the ruling: what lands here is
        // the RESOLVED flag and nothing else. The withdraw gate reads this map alone (decision 0095
        // retired its variable), so no credential line can arm it past its row; the preflight skip
        // still ORs the process environment in, and an exported `=0` disarms both sides of that OR.
        (f::ALLOW_WITHDRAW_KEYS_ENV, flags.allow_withdraw_keys, FoldTier::Resolved),
        (f::PREFLIGHT_SKIP_ENV, flags.preflight_skip, FoldTier::Resolved),
    ]
}

/// **The resolved [`vike_config::Flags`] that venue adapters read, folded into the map the mount
/// already threads them.** The whole of the wiring for six settings keys, in one place.
///
/// Every flag below gates code inside a venue adapter or a recorder, several frames under any
/// binary, and each one read its variable from the process environment (or from the credentials
/// map) with no way for a `flags` setting to reach it. `vike_config::CONSUMPTION`
/// carried all six as written admissions for exactly that reason. The cure is not a parameter per
/// flag through `vike_mount::NodeConfig` and `vike_mount::make_engine`: those already take a
/// `&HashMap<String, String>` as the one channel a venue fact travels on, so the composition root's
/// job is to FILL that map, and this is where it fills it.
///
/// ⚠ **THE PRECONDITION, and every safety argument downstream rests on it: `flags` must be the
/// `vike_config::load`-resolved value over this process's OWN environment sweep.** That is what
/// makes "the environment wins" true of what lands in the map — `vike_config::load` applies the
/// env layer over the file layer (`crates/vike-config/tests/flag_registry.rs`'s
/// `the_environment_overrides_the_row_for_every_flag` proves it for every flag, registry-wide),
/// and [`resolve_settings`] hands it [`PROCESS_ENV`], the real sweep. A caller that resolved
/// `flags` from some OTHER map would be folding a value with no such property, and the readers
/// downstream would have no way to tell.
///
/// ⚠ **The write OVERWRITES, for every key.** An earlier spelling of this function used `or_insert`
/// for all six, which left a `VIKE_ALLOW_WITHDRAW_KEYS=1` row in the credential store
/// standing and let it be OR-ed in by `vike_mount::arming`'s withdraw gate — a third source,
/// outranking the environment, in no precedence model and invisible to `vike-cli config show`. Two
/// keys kept `or_insert` on purpose for a while (the Polymarket gates, whose credential-store
/// spelling is refused at boot), and that exception was closed because the refusal's value grammar
/// is narrower than the readers' — the whole history is on [`FoldTier`]. The properties are
/// `a_process_env_value_beats_a_file_value_for_every_wired_key` (the keys that keep an environment
/// layer) and `no_credential_store_line_survives_the_fold_for_any_key` (every key, hostile
/// spellings included).
///
/// ⚠ A key whose variable decision 0095 retired (`vike_config::FlagMeta::reads_env` is `false`:
/// the two Polymarket gates, `HYPERLIQUID_HIP3`, `VIKE_ALLOW_WITHDRAW_KEYS`) has NO environment
/// layer, so "the environment wins" above is a statement about the others; for those the settings
/// row is the whole of `flags`' answer, and what the precondition still buys is that no other
/// source stands in for it.
///
/// ⚠ A box configured entirely by `Environment=` lines is byte-identical to before this function
/// existed — which is the constraint `docs/decisions/0054-settings-move-into-one-database.md`
/// states, and the reason the CI box's live reconcile policy in a systemd drop-in keeps working.
///
/// ⚠ The keys the owner DEFERRED (`pm_resolve`, `hl_outcome`, `poly_auto_redeem`,
/// `poly_redeem_halt`, `poly_heartbeat`, `record_chains`) are deliberately absent:
/// their consumers exist but are mounted by no running binary, so folding them in would move a
/// value nothing would read and turn an honest admission into a false claim. (`record_dvol` was a
/// seventh and is no longer a key at all — `vike_config::DEAD_FLAG_KEYS`.)
pub(crate) fn fold_flags_into_vars(flags: vike_config::Flags, vars: &mut HashMap<String, String>) {
    for (name, resolved, FoldTier::Resolved) in folded_flag_rows(flags) {
        vars.insert(name.to_string(), flag_wire_value(resolved));
    }
}

/// **The write-ahead journal's three variables, with `config.journal_dir` folded in** — the map
/// `vike_core::journal_config_from` reads instead of the process environment.
///
/// `VIKE_JOURNAL_DIR` was read in a `vike-core` library several frames below this binary, which is
/// why `config.journal_dir` was a declared key nothing could reach. Resolving it HERE is the
/// settings-registry rule (libraries take configuration as parameters; only binaries read the
/// process environment), and it is resolved ONCE so the paper and live mounts'
/// `CoreConfig::journal` and the journal rung's startup disclosure cannot answer differently about
/// where the WAL is. (The second reader was "the materializer's tail-follow" until the
/// `materialize` feature was deleted on 2026-09-22.)
///
/// ⚠ **`or_insert`, so an `Environment=VIKE_JOURNAL_DIR=…` line beats `config.journal_dir`.** The
/// map starts as the real process env; the setting's value lands only where the variable is absent.
/// `config.journal_snapshot_every` (decision 0111, phase P3) lands the same way as the directory:
/// under `VIKE_JOURNAL_SNAPSHOT_EVERY`, only where that is absent. An ACTIVE run row with a
/// `[sinks].journal` decides alone and this map is not consulted
/// (`crate::profile_rows::journal_config_for`).
pub(super) fn journal_vars(
    journal_dir: Option<&std::path::Path>,
    snapshot_every: Option<u32>,
) -> HashMap<String, String> {
    journal_vars_from(journal_dir, snapshot_every, process_env().clone())
}

/// [`journal_vars`] over a CALLER-SUPPLIED process-env map — the pure half, split out for the same
/// reason [`daemon_recon_env_from`] is: `std::env::set_var` is an `unsafe fn` this workspace
/// forbids, so "the variable was already exported" is a case only an injected base map can drive.
/// `the_process_env_beats_config_journal_dir` is the test.
pub(super) fn journal_vars_from(
    journal_dir: Option<&std::path::Path>,
    snapshot_every: Option<u32>,
    process_env: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut vars = process_env;
    if let Some(dir) = journal_dir {
        // The CONSTANT, never the literal: `vike_config` owns this variable's spelling, and the
        // settings registry resolves a read through the indirection.
        vars.entry(vike_config::config::JOURNAL_DIR_ENV.to_string())
            .or_insert_with(|| dir.display().to_string());
    }
    if let Some(every) = snapshot_every {
        // A literal: `vike_config` owns no constant for this variable (its row has no environment
        // arm there — the reader's own read is the environment layer). `vike-core`'s
        // `journal_config_from` parses it exactly as it parses the variable.
        vars.entry("VIKE_JOURNAL_SNAPSHOT_EVERY".to_string()).or_insert_with(|| every.to_string());
    }
    vars
}
