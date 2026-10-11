//! The `--json` printer: ONE document carrying the header, both halves, ceilings and venue rows.

use vike_config::Description;
use vike_config::show::FileRow;

use super::resolve::{Resolved, UnknownKeys};
use super::{RunProfileRows, StoreStatus};

// ---------------------------------------------------------------------------------------------
// Printers
// ---------------------------------------------------------------------------------------------

/// The tooling view: ONE object, so the two halves and the header do not have to be re-derived by
/// whoever parses it.
///
/// `secret` rides along on every row because `value` is otherwise ambiguous to a machine: a tool
/// cannot tell a redaction marker from a knob whose literal value is the string `<set>`. The human
/// tables do not need it — there, `<set>` in a `_API_KEY` row reads as exactly what it is.
pub(super) fn settings_json(
    d: &Description,
    secrets: &StoreStatus,
    files: &[FileRow],
    envs: &[Resolved],
    unknown: &UnknownKeys,
    profile_risk: &RunProfileRows,
    venue: &[crate::cmd::config::venue::VenueRow],
) -> serde_json::Value {
    serde_json::json!({
        "settings_dir": d.settings_dir.as_ref().map(|p| p.display().to_string()),
        // THE STORE THAT ANSWERED. `kind` — the same word `vike-cli secrets list --json` prints —
        // says which kind of store it is outright rather than smuggling it into an extension.
        "secrets": {
            "path": secrets.path.as_ref().map(|p| p.display().to_string()),
            "kind": secrets.kind(),
            "present": secrets.present,
            "keys": secrets.keys,
        },
        "warnings": d.settings.warnings,
        "settings": files.iter().map(|r| serde_json::json!({
            "key": r.key,
            "value": r.value,
            "origin": r.origin_kind,
            "origin_detail": r.origin,
            "default": r.default,
            "adjusted": r.adjusted,
            "secret": r.secret,
            // `false` means the value is recorded and applied by NOTHING. A tool reading this
            // should treat an `origin != "default"` row with `consumed: false` as a
            // misconfiguration, not as a setting in force.
            "consumed": r.consumed,
            // WHICH binary reads it, short name. `null` with `consumed: true` means a LIBRARY
            // reads it, so it applies wherever that library is linked — NOT "unknown". An agent
            // deciding whether a setting does anything on THIS box wants this field, not
            // `consumed`: three GUI-only keys reported `consumed: true` on headless daemons.
            "read_by": r.read_by,
            "why_unread": r.why_unread,
            // The machine half of the same correction: a consumer that only reads `why_unread`
            // gets the paragraph that was misleading on its own.
            "unread_verdict": r.unread_verdict,
        })).collect::<Vec<_>>(),
        "env": envs.iter().map(|r| serde_json::json!({
            "name": r.name,
            "value": r.value,
            "source": r.source.as_str(),
            "reads": r.reads.as_str(),
            "store_may_not_reach_reader": r.store_may_not_reach_reader(),
            "default": r.default,
            "krate": r.krate,
            "secret": r.secret,
        })).collect::<Vec<_>>(),
        // THE PRE-TRADE CEILINGS, from `vike_config::ceilings::PRE_TRADE_CEILINGS`. Unfiltered and always
        // present: a machine asking "what can refuse this order" must not have its answer depend
        // on the human filter, and `value_shown` is the field that says whether `settings` above
        // carries the number (`false` = it lives in the active run profile's `[risk]` table, which
        // the `profile_risk` block below reads — see `print_ceilings`). A `false` row is NOT an
        // unset ceiling: `max_total_exposure` is mandatory for a live mount.
        "ceilings": vike_config::ceilings::PRE_TRADE_CEILINGS.iter().map(|c| serde_json::json!({
            "name": c.name,
            "home": c.home.label(),
            "operator_doc": c.home.operator_doc(),
            "value_shown": c.home.value_shown_by_config_show(),
            "guards": c.guards,
            "enforced": c.is_enforced(),
            "enforced_at": c.enforced_at.iter().map(|s| serde_json::json!({
                "file": s.file,
                "what": s.what,
            })).collect::<Vec<_>>(),
            "absent_means": c.absent_means,
            // DERIVED, never typed: true when another home carries this same key and the two judge
            // different acts. The `# mirrors settings/policy.toml` comment this replaced.
            "shares_its_name": vike_config::shared_names().contains(&c.name),
            // ⚠ `false` here does NOT mean "absent is uncapped" — read `absent_means`, which is
            // the prose column that distinguishes the two answers. `true` means a LIVE mount
            // REFUSES TO START without this key, and it is gated against the source of the only
            // function that performs that refusal, so a machine can rely on it: see
            // `vike_config::Ceiling::refuses_live_mount_when_absent`.
            "refuses_live_mount_when_absent": c.refuses_live_mount_when_absent,
        })).collect::<Vec<_>>(),
        // THE STORED RUN-PROFILE `[risk]` ROWS, and the VALUES the `ceilings` array above still
        // cannot carry from the file. Unfiltered, like `ceilings`, and for the same reason.
        //
        // ⚠ `enforced` and `read_by` were the literal `false` and `null` on every row, on the
        // ground that NOTHING on the mount path read them. **That stopped being true when the run
        // profile's body moved onto the profile plane and the daemon learned to read it**, so they
        // are now computed per profile: `active` names the body the daemon builds its ceilings
        // from, and only THAT profile's rows are enforced. A machine that treated every row as a
        // ceiling in force would be wrong about the inactive ones; one that treated none as in
        // force would be wrong about the active one, which is the more dangerous half.
        // `state` is how a consumer tells "stored and empty" from "never stored".
        "profile_risk": {
            "state": match profile_risk.source {
                vike_secrets::ProfileRiskSource::Rows(_) => "rows",
                vike_secrets::ProfileRiskSource::NoDatabase { .. } => "no-database",
                vike_secrets::ProfileRiskSource::TableAbsent { .. } => "table-absent",
            },
            // Which stored `run` body this box READS, or `null` when none is active (decision 0111:
            // no file stands in for it, so the daemon then runs with no run profile at all).
            "active": profile_risk.active,
            "enforced": profile_risk.active.is_some(),
            "read_by": profile_risk.active.as_ref().map(|_| "vike-tradehub (the active run row)"),
            "profiles": profile_risk.source.profiles().unwrap_or_default().iter().map(|p| serde_json::json!({
                "profile": p.profile,
                // Per profile, so a consumer never has to join two fields to answer the one
                // question that matters about a pre-trade ceiling.
                "active": profile_risk.active.as_deref() == Some(p.profile.as_str()),
                "rows": p.rows.iter().map(|r| serde_json::json!({
                    "key": r.key,
                    "value": r.value,
                    // `null` for a row whose key is in no `[risk]` schema — see
                    // `vike_config::unknown_rows`, and note that no verb in this tree can write one.
                    "shape": vike_config::risk_key_kind(&r.key).map(|k| k.as_str()),
                    "known": vike_model::ProfileRisk::keys().contains(&r.key.as_str()),
                })).collect::<Vec<_>>(),
                // The keys this profile does NOT set, so a machine need not diff against the
                // roster itself. Two of them REFUSE a live mount when absent — the `ceilings`
                // array above is the authority for which.
                "unset": vike_config::missing_keys(p),
            })).collect::<Vec<_>>(),
        },
        // THE `venue_setting` ROWS (decision 0095): every declared field with its value and origin,
        // and every stored row nothing reads as `origin: "undeclared"` (no field declares it) or
        // `"misscoped"` (declared, stored at a scope the field does not use). A secret is `<set>`.
        "venue_settings": venue.iter().map(|r| serde_json::json!({
            "key": r.key,
            "value": r.value,
            "origin": r.origin,
            "secret": r.secret,
            "doc": r.doc,
        })).collect::<Vec<_>>(),
        // The env table's COMPLEMENT: store keys no registry row covers. `named` carries only the
        // keys `unknown_store_keys` cleared as non-credential-shaped; the rest are a count, so a
        // machine reading this document cannot enumerate the credential store either.
        "unknown_env_keys": {
            "named": unknown.named,
            "credential_shaped": unknown.credential_shaped,
        },
    })
}

/// Serialize [`settings_json`] and print it. Split from the builder so the redaction property can
/// be asserted on the DOCUMENT rather than on stdout.
pub(super) fn print_json(
    d: &Description,
    secrets: &StoreStatus,
    files: &[FileRow],
    envs: &[Resolved],
    unknown: &UnknownKeys,
    profile_risk: &RunProfileRows,
    venue: &[crate::cmd::config::venue::VenueRow],
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&settings_json(
        d,
        secrets,
        files,
        envs,
        unknown,
        profile_risk,
        venue,
    ))
    .map_err(|e| format!("cannot serialize settings JSON: {e}"))?;
    println!("{text}");
    Ok(())
}
