//! Resolved settings flags folded into the venue map (`fold_flags_into_vars`).

use std::collections::HashMap;

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
/// arming gates, on the argument that a boot refusal stopped an arming line in that store. The
/// argument had a hole: that refusal's value grammar was "the text before `#`, trimmed, is exactly
/// `1`", while the gate's reader (`vike_polymarket::poly_exec_enabled`) takes the FIRST TOKEN — so
/// a store row `POLY_EXEC=1 x` armed REAL-MONEY exec over `flags.poly_exec = false` without
/// tripping it, and a `POLY_EXEC=0` row silently vetoed `flags.poly_exec = true`. Both gates are
/// `Resolved` now: the flag is the SOLE source, and no credential row can arm or veto them — which
/// is why that boot refusal was deleted (2026-10-10): a row it refused is read by nothing.
/// `no_credential_store_line_survives_the_fold_for_any_key` holds that for every key.
///
/// The enum stays, single-variant, only because [`vike_config::CONSUMPTION`]'s six needles spell
/// `FoldTier::Resolved)` textually; collapsing the type re-keys those needles and is a change of its
/// own (decision 0095's prose sweep left it, as a code change rather than prose).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FoldTier {
    /// **OVERWRITE.** The resolved flag is written whatever `vars` held, so this key's value in
    /// the map IS `vike_config::Flags`' answer — the settings row, else its default.
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
        // refusal only caught a value whose text before `#` is exactly `1`, while the readers take the
        // first TOKEN, so `POLY_EXEC=1 x` armed real-money exec over a false flag and `POLY_EXEC=0`
        // vetoed a true one. The flag is the sole source now, so a store row of either name is read
        // by nothing.
        (f::POLY_EXEC_ENV, flags.poly_exec, FoldTier::Resolved),
        (f::POLY_RECONCILE_ENV, flags.poly_reconcile, FoldTier::Resolved),
        // Hyperliquid's symbology universe — read inside
        // `vike_hyperliquid::instruments::HyperliquidInstruments::load_from_vars`, which had no
        // credential-store tier before that constructor existed.
        (f::HYPERLIQUID_HIP3_ENV, flags.hyperliquid_hip3, FoldTier::Resolved),
        // The PIT `SymbolProperties` recorder's gate. ⚠ This named "`open_from_vars` at the
        // construction site below" as its reader until 2026-09-28, and that call is not made: the
        // daemon has passed `properties_rec: None` since it stopped writing the store (#2093), so
        // nothing in this binary reads the value this row folds. Decision 0111 then deleted the
        // recorder's map half too: `vike_data::PropertiesRecorder::open` takes the flag as its
        // `enabled` PARAMETER, so a NEXT root hands it `flags.record_properties` directly.
        (f::RECORD_PROPERTIES_ENV, flags.record_properties, FoldTier::Resolved),
        // ⚠ The two SAFETY OVERRIDES, where `false` is the guarded state. They are folded like any
        // other flag, by the owner's ruling that every setting is editable from the UI including
        // the live gates — and what makes that safe is the tier, not the ruling: what lands here is
        // the RESOLVED flag and nothing else. The withdraw gate reads this map alone (decision 0095
        // retired its variable), so no credential line can arm it past its row; the startup preflight
        // reads this map alone too (decision 0111 retired `VIKE_PREFLIGHT_SKIP`).
        (f::ALLOW_WITHDRAW_KEYS_ENV, flags.allow_withdraw_keys, FoldTier::Resolved),
        (f::PREFLIGHT_SKIP_ENV, flags.preflight_skip, FoldTier::Resolved),
    ]
}

/// **The resolved [`vike_config::Flags`] that venue adapters read, folded into the map the mount
/// already threads them.** The whole of the wiring for six settings keys, in one place.
///
/// Every flag below gates code inside a venue adapter or a recorder, several frames under any
/// binary, with no way for a `flags` setting to reach it except through a map. The cure is not a
/// parameter per flag through `vike_mount::NodeConfig` and `vike_mount::make_engine`: those already
/// take a `&HashMap<String, String>` as the one channel a venue fact travels on, so the composition
/// root's job is to FILL that map, and this is where it fills it.
///
/// ⚠ **THE PRECONDITION: `flags` is the value this process's ONE boot resolved** — its settings
/// rows, else each flag's default. The variable of each folded name refuses startup (decision 0095
/// for four, decision 0111 for `VIKE_RECORD_PROPERTIES` and `VIKE_PREFLIGHT_SKIP`), so no other
/// source can stand in for the row.
///
/// ⚠ **The write OVERWRITES, for every key.** An earlier spelling of this function used `or_insert`
/// for all six, which left a `VIKE_ALLOW_WITHDRAW_KEYS=1` row in the credential store standing and
/// let it be OR-ed in by `vike_mount::arming`'s withdraw gate — a third source in no precedence
/// model, invisible to `vike-cli config show`. Two keys kept `or_insert` on purpose for a while (the
/// Polymarket gates, whose credential-store spelling a boot refusal then caught), and that exception
/// was closed because the refusal's value grammar was narrower than the readers' — the whole
/// history is on [`FoldTier`]. `no_credential_store_line_survives_the_fold_for_any_key` is the property, hostile
/// spellings included.
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
