//! `vike-cli node setup` — the DAEMON's side: mint both node keys, set the bind address, and say
//! what to restart.
//!
//! Five of the six manual steps, in one command. What it does NOT do is the sixth — the client's
//! half — because that step happens on a different box; [`super::connect`] is its other end.
//!
//! # The order of operations IS the safety argument
//!
//! 1. **Validate the two key NAMES** against [`vike_model::credential_keys::PLATFORM_KEYS`], before
//!    anything is opened. A writer that cannot name its keys must not touch a store.
//! 2. **Open the store** — present, absent (REFUSE) or unreadable (ERROR); [`super::open_store`]
//!    carries the argument for the three answers.
//! 3. **Refuse an overwrite** unless `--rotate`, naming what rotation costs. Nothing has been
//!    written at this point, so the refusal is total.
//! 4. **Write the SETTINGS first** — the bind address, then `flags.tradehub_control` under
//!    `--control`. ⚠ This ordering is deliberate and it is the reverse of the obvious one: a
//!    settings write is REVERSIBLE (edit the file, restart) while minting a key is not (every client
//!    holding the old one stops working). A malformed `--addr` therefore fails while the store is
//!    still untouched, instead of leaving a box with fresh keys, a stale address and no way back to
//!    the keys the clients already have. `vike_config::set_setting` validates through the real
//!    loader before a byte lands, so this step is where a bad address is caught.
//! 5. **Mint and write the keys**, through `vike_secrets::save_credentials` — the workspace's one
//!    in-place upsert, which replaces exactly the two lines it was handed and leaves every other
//!    byte, comment and ordering intact.
//! 6. **Record**, and a ledger failure does not fail the call: the credentials ARE on disk.
//!
//! # Nothing here can print, log or error with a key
//!
//! Each minted value lives in one local, is moved into the update pair, and every line this module
//! emits names a KEY NAME, a PATH, a settings value or a `key_id`. There is no branch on which a key
//! reaches a stream — `crates/vike-cli/tests/node_cli.rs`'s
//! `setup_mints_two_keys_prints_their_ids_and_never_a_key` asserts that over the real binary's
//! two streams, and `rotate_replaces_both_keys_and_preserves_the_rest_of_the_store` asserts it again
//! on the path an operator runs twice.

use vike_config::SettingsFile;

use super::{Args, Ctx, DEFAULT_BIND_ADDR, key_id, mint_key, open_store, record_credential_write};
use crate::exit::{CliError, CmdResult};

/// `config.toml`'s bind key, spelled once. The dotted form `vike_config::set_setting` takes — its
/// first segment names the file, and the remainder is the path inside it.
const BIND_ADDR_KEY: &str = "config.tradehub_addr";

/// `flags.toml`'s control gate, same dotted form. ⚠ Written ONLY under `--control`: without the
/// flag this key is not touched at all, so a box that had it on keeps it on and a box that had it
/// off keeps it off. Setup arms nothing an operator did not ask for, and infers nothing from
/// whether the daemon is live.
const CONTROL_FLAG_KEY: &str = "flags.tradehub_control";

/// Run `node setup`. See the module doc for why the steps are in this order.
pub(super) fn run_setup(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    // 1. The names, against the table that validates them.
    let names = super::validated_names()?;

    // 2. The store — the PROJECT's, always. There is no `--file` on any verb in this module.
    let path = super::store_path(ctx);
    let existing = open_store(&path)?;

    // 3. The overwrite refusal. A key already in the store is a key some client is already signing
    // with, and replacing it is a decision with a blast radius rather than an idempotent re-run.
    let present: Vec<&str> = names
        .into_iter()
        .filter(|n| existing.get(*n).is_some_and(|v| !v.trim().is_empty()))
        .collect();
    if !present.is_empty() && !args.rotate {
        return Err(CliError::failed(rotation_refusal(&present, &path.display().to_string())));
    }

    // 4. The settings, BEFORE the irreversible half — see the module doc. The bind address always;
    // the control gate only when it was asked for.
    let addr = args.addr.as_deref().unwrap_or(DEFAULT_BIND_ADDR);
    println!("settings:");
    super::set_setting_journalled(ctx, SettingsFile::Config, BIND_ADDR_KEY, addr)?;
    if args.control {
        super::set_setting_journalled(ctx, SettingsFile::Flags, CONTROL_FLAG_KEY, "true")?;
    }

    // 5. The mint and the write — one call, both keys, so the pair can never be half-rotated by a
    // failure between two writes.
    let observe = mint_key();
    let control = mint_key();
    let ids = [key_id(&observe), key_id(&control)];
    let updates = [(names[0].to_string(), observe), (names[1].to_string(), control)];
    vike_secrets::save_credentials(&path, &updates)
        .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;

    // 6. The durable record — key NAMES only. `Change::credential_write` takes no value parameter
    // at all, so nothing here CAN put a credential in the ledger.
    record_credential_write(ctx, &path, &names);

    for line in report(&names, &ids, addr, args.control, &present, &path.display().to_string()) {
        println!("{line}");
    }
    Ok(())
}

/// The refusal for keys that are already in the store, naming what `--rotate` costs.
///
/// PURE, so the wording is unit-tested rather than reached only by arranging a populated store.
///
/// ⚠ It states the consequence in the CLIENT's terms, not the daemon's, because that is where the
/// damage lands: the daemon comes back up perfectly, and every laptop, container and thin GUI still
/// holding the old key fails its handshake with an opaque `AuthDenied` that reads like a revoked
/// credential. `docs/ops/tradehub-the CI box.md` records the mirror-image confusion — a version skew
/// diagnosed as a bad key — costing a rotation that was not needed.
fn rotation_refusal(present: &[&str], store: &str) -> String {
    let names = present.join(" and ");
    format!(
        "{names} already in {store}, and `setup` will not silently replace a key some client is \
         signing with.\n\
         ⚠ Re-run with --rotate to replace BOTH keys. Every client already holding the old ones — \
         a laptop's `vike-cli`, a `vike-app --observe`, a thin container — stops working the moment \
         the daemon restarts, and its symptom is an auth denial that reads like a revoked \
         credential rather than like a rotation. Re-run `vike-cli node connect` on each of them \
         afterwards.\n\
         Nothing was written."
    )
}

/// The success report, as lines. PURE — every cell is a key NAME, a `key_id`, an address or a file,
/// and there is no branch on which a key VALUE can reach it.
///
/// The two `key_id`s are the point of the whole print: `connect` prints the same two on the client's
/// box, and comparing them by eye is this design's answer to *did I attach to the right node with
/// the right key*. They are safe to read aloud, paste into an issue and keep in a ledger —
/// [`super::key_id`] carries why.
fn report(
    names: &[&str; 2],
    ids: &[String; 2],
    addr: &str,
    control: bool,
    rotated: &[&str],
    store: &str,
) -> Vec<String> {
    let verb = if rotated.is_empty() { "minted" } else { "ROTATED" };
    let width = names.iter().map(|n| n.len()).max().unwrap_or(0);
    let mut lines = vec![
        String::new(),
        format!("two node keys {verb} into {store} (the VALUES were not printed and never are):"),
        format!("  {:<width$}  {}   read plane", names[0], ids[0]),
        format!("  {:<width$}  {}   write plane", names[1], ids[1]),
        String::new(),
    ];
    // ⚠ The consequence is repeated on the SUCCESS path, not just in the refusal that `--rotate`
    // gets past. An operator reaches this line having already typed the flag — sometimes on a second
    // reading of a runbook, sometimes because the refusal told them to — and the clients that just
    // stopped working are on other boxes, where the symptom is an auth denial that reads like a
    // revoked credential. Saying it once, in the message they overrode, is saying it to the wrong
    // person at the wrong time.
    if !rotated.is_empty() {
        lines.extend([
            "⚠ THESE REPLACED KEYS THAT WERE ALREADY IN THE STORE. Every client still holding the"
                .to_string(),
            "  old pair — a laptop's `vike-cli`, a `vike-app --observe`, a thin container — stops"
                .to_string(),
            "  working at the restart below. Re-run `vike-cli node connect` on each of them."
                .to_string(),
            String::new(),
        ]);
    }
    lines.push(if control {
        format!(
            "control is ARMED: {CONTROL_FLAG_KEY} = true, so a peer holding the write key can \
             place orders on this node (bounded by policy.max_notional_per_order, \
             VIKE_TRADEHUB_CONTROL_RATE and the core risk gate)."
        )
    } else {
        format!(
            "control is NOT armed — {CONTROL_FLAG_KEY} was left exactly as it was. The write key \
             exists and opens nothing until that flag is on; re-run with --control when you mean \
             to admit orders."
        )
    });
    lines.extend([
        String::new(),
        "⚠ The daemon reads both keys ONCE, at start — there is no credential hot-reload, so this"
            .to_string(),
        "  changes nothing until it restarts:".to_string(),
        "    sudo systemctl restart vike-tradehub          # a systemd deployment".to_string(),
        "    docker restart vike-tradehub                  # the container".to_string(),
        String::new(),
        format!("Then, on the CLIENT box, with this node reachable at {addr}:"),
        "    vike-cli node connect <this host> --manual".to_string(),
        "  …and compare the two key ids it prints with the two above. They match or you are"
            .to_string(),
        "  talking to a different node.".to_string(),
    ]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> [String; 2] {
        [key_id("observe-key"), key_id("control-key")]
    }

    /// **The report names both keys, both ids, and NEITHER key.** The properties that make this
    /// command's output safe to paste into an issue, asserted over the pure renderer so every branch
    /// is reachable without a store.
    #[test]
    fn the_report_carries_key_ids_and_no_key() {
        let names = ["OBSERVE_NAME", "CONTROL_NAME"];
        let ids = ids();
        let text = report(&names, &ids, "127.0.0.1:7879", false, &[], "/p/settings/secrets.env")
            .join("\n");
        assert!(text.contains("OBSERVE_NAME") && text.contains("CONTROL_NAME"), "{text}");
        assert!(text.contains(&ids[0]) && text.contains(&ids[1]), "{text}");
        assert!(
            !text.contains("observe-key") && !text.contains("control-key"),
            "a key leaked:\n{text}"
        );
        assert!(text.contains("127.0.0.1:7879"), "the bind address is reported: {text}");
    }

    /// The restart line is present and says WHY it is needed. A `setup` whose output did not demand
    /// a restart would leave an operator watching a daemon that is still verifying the old keys —
    /// the keys are read once, in `start_observe_server`, and nothing re-reads them.
    #[test]
    fn the_report_demands_a_restart_and_says_why() {
        let text = report(&["A", "B"], &ids(), "127.0.0.1:7879", false, &[], "/p/x").join("\n");
        assert!(text.contains("restart"), "{text}");
        assert!(text.contains("reads both keys ONCE"), "{text}");
        assert!(text.contains("systemctl restart vike-tradehub"), "{text}");
        assert!(text.contains("docker restart"), "{text}");
    }

    /// **Both arms of the control report exist, and the un-armed one is not silence.** An operator
    /// who forgot `--control` and was told nothing would conclude the write channel was open,
    /// discover otherwise only when an order is refused, and have no idea which of the two boxes to
    /// look at.
    #[test]
    fn the_control_line_is_printed_whether_or_not_it_was_armed() {
        let armed = report(&["A", "B"], &ids(), "1:2", true, &[], "/p/x").join("\n");
        assert!(armed.contains("control is ARMED"), "{armed}");
        assert!(armed.contains(CONTROL_FLAG_KEY), "it names the key: {armed}");

        let quiet = report(&["A", "B"], &ids(), "1:2", false, &[], "/p/x").join("\n");
        assert!(quiet.contains("control is NOT armed"), "{quiet}");
        assert!(
            quiet.contains("left exactly as it was"),
            "it must not claim to have written: {quiet}"
        );
        assert!(quiet.contains("--control"), "it names the flag that would arm it: {quiet}");
    }

    /// A rotation says so in the report AND repeats what it cost — to the person who just typed
    /// `--rotate`, who is not the person the refusal was written for. A first run says neither.
    #[test]
    fn a_rotation_is_reported_as_one_and_repeats_its_blast_radius() {
        let rotated = report(&["A", "B"], &ids(), "1:2", false, &["A"], "/p/x").join("\n");
        assert!(rotated.contains("ROTATED"), "{rotated}");
        assert!(rotated.contains("stops"), "the cost must be repeated on success: {rotated}");
        assert!(rotated.contains("node connect"), "…with the fix for each client: {rotated}");

        let first = report(&["A", "B"], &ids(), "1:2", false, &[], "/p/x").join("\n");
        assert!(!first.contains("REPLACED"), "a first run replaced nothing: {first}");
    }

    /// The overwrite refusal names each key already present, names the flag, and states the cost in
    /// the CLIENT's terms — and writes nothing.
    #[test]
    fn the_rotation_refusal_names_the_keys_the_flag_and_the_blast_radius() {
        let msg = rotation_refusal(&["OBSERVE_NAME", "CONTROL_NAME"], "/p/settings/secrets.env");
        assert!(msg.contains("OBSERVE_NAME") && msg.contains("CONTROL_NAME"), "{msg}");
        assert!(msg.contains("/p/settings/secrets.env"), "{msg}");
        assert!(msg.contains("--rotate"), "{msg}");
        assert!(msg.contains("stops working"), "the cost must be stated plainly: {msg}");
        assert!(msg.contains("Nothing was written."), "{msg}");
    }

    /// The two settings keys are the dotted spellings `vike_config::set_setting` takes: the first
    /// segment names the file, the rest is the path inside it. A key whose section did not match its
    /// file is refused by that function with `BadKey`, which would surface as a runtime failure on a
    /// command that had already written credentials.
    #[test]
    fn the_settings_keys_are_sectioned_for_the_files_they_are_written_to() {
        assert_eq!(BIND_ADDR_KEY.split('.').next(), Some(SettingsFile::Config.section()));
        assert_eq!(CONTROL_FLAG_KEY.split('.').next(), Some(SettingsFile::Flags.section()));
        // …and both are real settings this workspace resolves, not names invented here.
        let keys = vike_config::provenance::setting_keys();
        for key in [BIND_ADDR_KEY, CONTROL_FLAG_KEY] {
            assert!(keys.iter().any(|k| k.key == key), "{key} is not a settings key");
        }
    }
}
