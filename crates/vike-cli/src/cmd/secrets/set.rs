//! `vike-cli secrets set KEY` — the ONE narrow credential writer this command carries, and every
//! rule that fences it: the value from stdin or a named environment variable and never argv, the key
//! validated and refused by name, an absent store refused rather than created, and one journalled
//! `credential_write` per write.
//!
//! Split out of `cmd/secrets.rs` (code-layout phase 2, task 10); its module doc ("ONE writer, and
//! its shape was fixed BEFORE it was built") is the argument. This file holds the call site of the
//! workspace's one upsert, which is what `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins.

use std::path::Path;

// The workspace's gated catalog of every environment variable it reads — the authority behind
// `set`'s read-but-not-settable refusal. See [`registry_readers`] for why the answer is derived
// from it rather than from a table of this command's own.
use vike_ops::settings::all_settings;
use vike_secrets::Source;

use super::*;
use crate::exit::{CliError, CmdResult};

/// `set KEY` — upsert ONE credential into an EXISTING store, and record it.
///
/// The whole of the writer `docs/decisions/0036`'s reopen clause fixed the shape of; this module's
/// doc lists the properties and why each is there. What this function adds beyond them is the
/// ORDER, and the order is the argument:
///
/// 1. **Validate the KEY first**, as far as a PURE rule can, before anything is opened or asked of
///    stdin. A refused key must not have consumed the operator's piped secret on its way to the
///    error. ⚠ One of the three admission rules — ROTATION, a name the store already holds — cannot
///    be pure, and it runs at 2½ rather than here. The property this ordering exists for is
///    unaffected: every refusal still precedes step 3.
/// 2. **Then open the store** — through `vike_secrets::resolve_store_in`, the same reader `list`
///    uses, which answers three ways: the database (proceed), none (REFUSE — this command creates
///    nothing, and names `secrets init`), and unreadable (an ERROR, never "not configured"; a
///    permissions bug wearing the fresh-install answer is the failure `vike_secrets::SecretsError`
///    exists for). It also tells us whether the key is REPLACED or APPENDED, which the writer does
///    not report.
/// 3. **Then take the value**, from stdin or from the named variable.
/// 4. **Then write**, through the workspace's one upsert.
/// 5. **Then record**, and a failure here does NOT fail the call — the credential IS on disk, and
///    sending a caller down an error path for a write that succeeded is worse than a missing ledger
///    line. The same disposition `vike_ctrader::token_store`'s `record_rotation` takes.
///
/// ⚠ **Nothing in this function can print, log or ERROR with a value.** The value lives in one
/// local, is moved into the update pair, and every message built here names a KEY, a PATH or an
/// environment VARIABLE. `crates/vike-cli/tests/secrets_cli/set.rs`'s
/// `set_from_stdin_appends_the_key_and_preserves_every_other_byte` is the assertion over the real
/// binary's two streams, and its sibling
/// `a_value_in_argv_is_refused_on_the_usage_rung_and_never_echoed` is the same claim about the
/// REFUSAL path — the one an operator reaches with the secret already typed.
pub(super) fn run_set(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let key = args.key.as_deref().expect("parse refuses `set` with no key");
    // 1. The key. The GRID answers first; a name outside it is admitted only when the REGISTRY
    //    proves something reads it — see `settable_outside_the_grid` for what stays refused and
    //    why the editor route it replaces is dead.
    //
    // ⚠ **The PURE half only.** The third rule — ROTATION, a name the store already holds — needs
    // the store open, so it is applied at step 2½ below. Nothing about that reaches stdin: the
    // property this ordering exists for is that a REFUSED KEY NEVER CONSUMES THE PIPED SECRET, and
    // the value is still taken at step 3, after every refusal.
    let pure_owner = match vike_model::credential_keys::key_owner(key) {
        Some((v, t)) => Some((v.to_string(), t.map(str::to_string))),
        None => settable_outside_the_grid(key),
    };

    // 2. The store — the PROJECT's, always: the settings database, or none. Three things
    // downstream hang off this read: the ABSENT refusal, the replaced-vs-appended report, and —
    // through `settings_dir_of` feeding the writer below — WHERE THE KEY LANDS.
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let db = vike_secrets::db_path_in(&dir);
    let resolved = resolve_store(ctx.settings_dir, ctx.settings_dir_override).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
                 having none. Nothing was written."
        ))
    })?;
    if matches!(resolved.source, Source::None) {
        return Err(CliError::failed(format!(
            "no credential store at {} — this command upserts into an existing store and creates \
             none. Make the EMPTY store (the settings database, the only credential store), then \
             set the key:\n  vike-cli secrets init",
            db.display()
        )));
    }
    // Same finding, same stream and same shape as `list`'s — a path and an octal mode, never a
    // credential. A finding is never a refusal.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // The one thing the writer does not report back — whether the key replaced one already held.
    // Asked HERE, of a map we already hold, rather than by re-reading the store after the write.
    let replaced = resolved.secrets.keys().any(|k| k == key);

    // 2¼. ⚠ A VENUE SETTING's credential-style name (decision 0095). Nothing reads one from the
    // credential store, so `set` never writes one, whether or not the store already holds a row
    // under it, and names the `config set` line that writes the setting instead. Still BEFORE the
    // value.
    if let Some((_, setting)) =
        vike_secrets::venue_setting::stranded_venue_setting_names([key]).into_iter().next()
    {
        return Err(CliError::usage(stranded_setting_message(key, &setting)));
    }

    // 2½. The key's LAST rule, the one that needed the store. A name this box already holds is a
    // credential in force and rotating it is the act that had no writer at all; `rotation_owner`
    // argues it. Asked HERE rather than at step 1 because it is the same read `replaced` just did —
    // no second store open — and still BEFORE the value, so the refusal below cannot happen with a
    // secret in hand.
    let (venue, tier) = match pure_owner {
        Some(owner) => owner,
        None => rotation_owner(key, &resolved.secrets)
            .ok_or_else(|| CliError::usage(unknown_key_message(key)))?,
    };

    // 3. The value. AFTER every refusal above, so none of them can happen with a secret in hand.
    let value = value_for(args, ctx)?;

    // 4. The write — a SECOND CALL SITE of the one writer, never a second writer.
    //
    // ⚠ Routed to the store that ANSWERS — the settings database. `save_credentials_to_store` asks
    // the same `backend_in` the read above asked, so this command cannot report on one store and
    // write to another, and with no database it refuses rather than writing anywhere else. There is no
    // `--file` arm, so the destination of a write is never operator-supplied.
    let landed = vike_secrets::save_credentials_to_store(
        &dir,
        vike_secrets::Table::Credential,
        &[(key.to_string(), value)],
        // Schema 2's `credential.field` is NOT NULL, so a name this store has never held needs the
        // account classification `vike_bridge_core::credentials::classify_credential_name` derives.
        // A key that is already there is replaced in place and never reaches it.
        Some(&vike_bridge_core::credentials::classify_credential_name),
    )
    .map_err(|e| CliError::failed(format!("could not write {}: {e}", db.display())))?;

    // The STORE that was written, so neither the sentence NOR THE LEDGER can name a store the key
    // did not go into. On success that is always the database (the writer refuses with none).
    let where_it_landed = match landed {
        vike_secrets::Backend::Database(db) => db,
        vike_secrets::Backend::Absent => db.clone(),
    };

    // 5. The durable record.
    record_write(ctx, &where_it_landed, key, &venue, tier.as_deref());

    println!(
        "{key} {} in {}",
        if replaced { "replaced" } else { "appended" },
        where_it_landed.display()
    );
    Ok(())
}

/// The value to write: stdin, or the environment variable `--from-env` named.
///
/// ⚠ **The two are trimmed DIFFERENTLY, on purpose.** A piped value arrives with the newline the
/// shell or the operator's editor put there, and `printf %s` is not what anybody types by default —
/// so stdin is trimmed, and a store full of values with trailing newlines is not a thing this
/// command can produce. An environment variable carries exactly what was exported into it, so it is
/// taken VERBATIM: trimming it would silently alter a credential whose leading or trailing
/// whitespace is real, and the database stores the value byte for byte.
///
/// Both refuse EMPTY — and both refuse a value that spans more than ONE LINE. An empty value is
/// equivalent to an absent key (the venue stays on paper), so writing one would report success for
/// a change that arms nothing, which is the failure class this workspace deleted a settings key
/// over; "empty" is asked AFTER a trim, because a variable holding three spaces is that same state
/// wearing a value, and the store hands it back non-empty so the mount arms with
/// a garbage secret and fails at the venue instead of staying on paper.
///
/// # ⚠ ONE LINE, and the multi-line case was an INJECTION rather than an untidiness
///
/// The retired file store's grammar was one credential per line. A `--from-env` value carrying a
/// `\n` used to be written verbatim: the file writer quoted it (a newline is whitespace) and joined with
/// `\n`, so the value's own break became a physical line break, and the reader then returned the
/// first half as a SILENTLY TRUNCATED credential and read the second half as a whole new
/// `KEY=VALUE` — a credential for a venue the operator never configured, past a key name this
/// command had validated. `vike-cli secrets set KEY --from-env NAME` is the CI/deploy-script form,
/// and a multi-line secret is the ordinary shape of a Vault- or Actions-injected variable.
///
/// It is refused HERE as well as in `vike_secrets::save_credentials_to_store` deliberately, and the
/// two are not redundant: the writer's refusal is the property (a credential is ONE line, for every
/// caller including the GUI), while this one names
/// the VARIABLE the operator can go and look at, which an `io::Error` surfacing from three layers
/// down cannot.
pub(super) fn value_for(args: &Args, ctx: &Ctx<'_>) -> CmdResult<String> {
    match args.from_env.as_deref() {
        Some(name) => {
            // The map the DISPATCHER swept — never `std::env::var`, which would put a `src/cmd/`
            // file on the settings registry's `Layer::Library` work-list.
            let value = ctx.env.get(name).cloned().unwrap_or_default();
            if value.trim().is_empty() {
                // Names the VARIABLE, never its content — and "unset, empty or blank" is ONE
                // message, because to this command they are the same state. A CI variable that
                // expanded to nothing, or a template that rendered blank, arrives as any of them.
                return Err(CliError::usage(format!(
                    "--from-env {name}: that variable is unset, empty or only whitespace in this \
                     process's environment, so there is no value to write"
                )));
            }
            if value.contains(['\n', '\r']) {
                return Err(CliError::usage(format!(
                    "--from-env {name}: that variable's value spans more than one line, and a \
                     credential is ONE line. The store cannot represent it — written out, the \
                     break would truncate the credential and turn the remainder into a second \
                     KEY=VALUE line for a key you did not name. Nothing was written."
                )));
            }
            Ok(value)
        }
        None => {
            let line = read_stdin_line().map_err(|e| {
                CliError::failed(format!("could not read the value from stdin: {e}"))
            })?;
            let value = line.trim();
            // ⚠ Asked on this arm too, though `read_stdin_line` stops at the first `\n`. It bounds
            // the value only by ACCIDENT of that choice, and only for `\n`: a lone `\r` (a CR line
            // ending, or a CRLF value pasted mid-line) survives both the read and the trim, and
            // reaches the store inside the value. One rule for both arms is cheaper to keep true
            // than an argument about which reader happens to bound what.
            if value.contains(['\n', '\r']) {
                return Err(CliError::usage(
                    "the value on stdin spans more than one line, and a credential is ONE line. \
                     The store cannot represent it — written out, the break would truncate the \
                     credential and turn the remainder into a second KEY=VALUE line for a key you \
                     did not name. Nothing was written."
                        .to_string(),
                ));
            }
            if value.is_empty() {
                let key = args.key.as_deref().unwrap_or("KEY");
                return Err(CliError::usage(format!(
                    "no value on stdin. The value never goes on the command line — one of:\n  \
                     printf %s \"$SECRET\" | vike-cli secrets set {key}\n  \
                     vike-cli secrets set {key} --from-env NAME"
                )));
            }
            Ok(value.to_string())
        }
    }
}

/// ONE line off stdin. Split out so [`value_for`]'s two arms read as the two POLICIES they are,
/// with the I/O named rather than inlined.
///
/// One line, not the whole stream: a credential is one line, and reading to EOF would let a
/// mis-aimed `cat file |` write a whole file's contents into the store as one value.
fn read_stdin_line() -> std::io::Result<String> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// The refusal for a key name `set` cannot write, with the nearest real names.
///
/// PURE, so the shape is unit-tested below. It names the offending key — a key NAME is not a secret
/// (`list` prints them, by an explicit decision in the root `CLAUDE.md`) — and never a value,
/// because it never has one: [`run_set`] validates before it reads stdin.
///
/// ⚠ **The suggestions matter more here than they would on an ordinary typo'd flag.** The store is
/// a flat `KEY=VALUE` file, so a hand-edited `BINANCE_LIVE_API_KEY_` is written just as happily as
/// the real name — and the venue then stays on paper with no error anywhere, which is failure
/// reason 3 in `docs/decisions/0036`. Refusing by name is what removes that class for this command;
/// the nearest names are what make the refusal actionable.
///
/// ⚠ **THREE refusals, not one, because ONE of them used to be FALSE.** The refusal itself is
/// unchanged in every case — `set` writes the enumerable GRID and nothing wider — but the SENTENCE
/// that explained it made a claim about the whole WORKSPACE ("setting it would write a line nothing
/// would ever load") on the strength of a fact about this one command. For a name the workspace
/// genuinely reads through some other loader that sentence is simply untrue, and it was measured
/// untrue on `VIKE_TRADEHUB_OBSERVE_KEY`: `crates/vike-tradehub/src/tradehub_cli.rs`'s
/// `start_observe_server` reads that exact name out of the credential map, and the operator who
/// followed the refusal was told a key they had just configured was inert and given no route at
/// all. The escape-hatch paragraph did not cover them either — it named the FX login pairs and "the
/// other per-bridge spellings", and a node key is neither per-bridge nor has a bridge loader to be
/// pointed at.
///
/// So the message now splits on the only question that makes the old sentence safe: **does anything
/// in this workspace read this NAME at all?** [`registry_readers`] is the answer, and the split is
/// DERIVED rather than a second roster — see that function for why the registry is the authority
/// and for the one thing it deliberately does not claim.
///
/// ⚠ **FOUR now, not three, and the fourth exists because the third's ADVICE went stale.** The
/// read-but-not-settable arm pointed every outside-the-grid name at an editor, which was the honest
/// route while nothing in this tree generated a key. `vike-cli backend setup` generates the two
/// `vike-tradehub` node keys, so for those two names the editor sentence became the same class of
/// defect the arm was built to end — correct about the refusal, wrong about the route. They are
/// separated by [`vike_model::credential_keys::is_platform_key`], the names-only table that exists
/// for exactly this distinction, and their message names the command and which BOX to run it on.
pub(super) fn unknown_key_message(key: &str) -> String {
    // ⚠ **A LABELLED ACCOUNT gets its own refusal and NO suggestions**, and the reason is that the
    // obvious suggestion was dangerous rather than merely unhelpful.
    //
    // `HYPERLIQUID_LIVE_API_KEY__ALT` names a SECOND ACCOUNT — a name
    // `vike_model::accounts::account_keys::accounts_in_store` parses and
    // `vike_bridge_core::credentials::load_credentials_for_account` genuinely reads, and which
    // `secrets list` prints. This command still cannot write it (the grid is a fixed enumeration and
    // a label is an unbounded name set), so it is refused — but `nearest_keys` scored the UNLABELLED
    // base as the closest name and offered it first, and that name is real, settable and accepted.
    // Following the suggestion overwrote the DEFAULT account's live signing key with a second
    // account's, exit 0, "replaced". The second suggestion was worse in kind: it proposed writing an
    // API key into the API SECRET slot.
    //
    // So when the base resolves, the message says the one thing the operator has to know — these
    // are two different ACCOUNTS — and offers nothing to copy.
    // ⚠ **THE LABELLED-ACCOUNT ARM IS GONE, and it is `settable_outside_the_grid` that took it.**
    // It refused `{BASE}__{LABEL}` for being an unbounded name set and sent the operator to an
    // editor — the dead route, for the family that needed a writer most, since `secrets list`
    // PRINTS these accounts. `labelled_account` admits a label only when its BASE is a grid key, so
    // a labelled name that reaches this function has a base the grid does not carry, and that
    // helper answers `None` for it: the message it would have produced is unreachable, and the
    // generic tail below is the honest one for what is left.
    //
    // ⚠ What did NOT go with it is the hazard it was written about — `nearest_keys` offering the
    // UNLABELLED base, which an operator then set, overwriting the DEFAULT account's signing key.
    // That is guarded where it lives: `nearest_keys`' own doc, and
    // `a_labelled_account_is_written_rather_than_refused_and_the_base_is_never_offered`.
    // ⚠ **THE PLATFORM-KEY ARM, and it names a COMMAND rather than an editor.** The two
    // `vike-tradehub` node keys are read by this workspace, are outside the grid `set` writes, and
    // — since `vike-cli backend setup` landed — are no longer something an operator writes by hand
    // at all. They are the one outside-the-grid family with a real route, so they get their own
    // sentence: telling somebody to invent a 256-bit HMAC key in an editor is exactly the advice
    // that command exists to delete, and it is the advice this message used to give.
    if let Some(service) = vike_model::credential_keys::platform_key_service(key) {
        // ⚠ The VERB is chosen from the service, never assumed. This arm named the tradehub verb
        // unconditionally while `PLATFORM_KEYS` held one pair; the day the datahub pair joined, a
        // constant here would have sent an operator to the command for a DIFFERENT service — the
        // same defect this arm exists to end, wearing the other service's clothes.
        // ⚠ THE CLIENT LINE IS PER-SERVICE BECAUSE THE COMMAND IS. `vike-cli backend connect`
        // exists; there is no `datahub connect` — the datahub's client half is not built. Naming
        // one anyway would be this arm's own defect wearing the other service's clothes: a refusal
        // that is right about refusing and wrong about the route. Each service names only what it
        // has.
        let (verb, fallback, client) = match service {
            "vike-datahub" => (
                "datahub",
                "the datahub server",
                "\n→ A project whose box can read the server project's settings directory takes \
                 the SAME pair with `vike-cli secrets copy-node-keys --from-settings-dir <DIR>` \
                 (database to database, never a value in hand); a CLIENT elsewhere exports the \
                 two variables. A client authenticates by holding the identical pair.",
            ),
            _ => (
                "backend",
                "the tradehub daemon",
                "\n→ On a CLIENT box, `vike-cli backend connect <host> --manual` writes the pair \
                 it reads from stdin; a project re-homed on a box that can read the old one's \
                 settings directory takes the SAME keys with `vike-cli secrets copy-node-keys \
                 --from-settings-dir <DIR>`.",
            ),
        };
        return format!(
            "'{key}' IS read by this workspace — `vike_ops::settings` records {} reading it — so \
             this is NOT a line nothing would load. `set` refuses it because it is not a credential \
             you should ever TYPE: it is a 256-bit HMAC key, and a hand-pasted one that is \
             truncated fails as an opaque auth denial rather than as anything readable.\n→ On the \
             {service} box — the one RUNNING it — `vike-cli {verb} setup` MINTS both of that \
             service's node keys and prints each key's id. It never accepts a key and never prints \
             one.{client}",
            registry_readers(key).unwrap_or_else(|| fallback.to_string())
        );
    }
    // ⚠ **THE READ-BUT-NOT-SETTABLE ARM IS GONE, and its absence is the change.** It used to catch
    // every outside-the-grid name the registry knew a reader for — the bespoke venue logins, the
    // `POLY_*` trio, the Telegram pair — and send the operator to an EDITOR. That route DIED with
    // `docs/decisions/0054`'s credential half: once `settings/db/vike.db` exists the credential
    // FILES are not read at all, so the edit changes nothing and the advice was a dead end.
    // `settable_outside_the_grid` admits those names now, so nothing reaches here with a registry
    // reader: a labelled key was caught above, a node key was caught above, and what is left is a
    // name no row mentions at all.
    // ⚠ **THE SETTINGS-KEY ARM, and it exists because the tail below would otherwise LIE.** A name
    // the registry DOES carry but the credential classifier cannot place is a SETTING, not a
    // credential: `VIKE_HIST_STORE` is a store path, read out of the process-env sweep rather than
    // the credential map. Reaching this function means the ROTATION rule declined it too — the
    // store does not hold it — so there is nothing to rotate and nothing to invent. Printing "no
    // `vike_ops::settings` row names it at all" over a key with five rows would be a fresh
    // instance of the measured lie this whole area exists to remove, wearing the other direction.
    if let Some(readers) = registry_readers(key) {
        return format!(
            "'{key}' IS read by this workspace — `vike_ops::settings` records {readers} reading it \
             — but not out of the CREDENTIAL store: `secrets set` writes credentials, and this is \
             a SETTING. This box's store does not hold it either, so there is no value here to \
             rotate.\n→ `vike-cli config show` prints where this key's value actually comes from, \
             and `vike-cli config set` is what writes a settings key."
        );
    }
    let near = nearest_keys(key);
    let tail = if near.is_empty() {
        "`vike-cli config show` lists every key name this workspace reads".to_string()
    } else {
        format!("did you mean: {}", near.join(", "))
    };
    // The ORIGINAL sentence, now printed ONLY where the arm above proved it true: no registry row
    // names this key, so nothing the settings gate can resolve reads it under any spelling.
    format!(
        "'{key}' is not a credential key this workspace reads — no `vike_ops::settings` row names \
         it at all — so setting it would write a line nothing would ever load — {tail}.\n⚠ What \
         this command CAN write is wider than the fixed key grid: the grid itself, plus every \
         bespoke per-bridge name the settings registry records a reader for, plus a LABELLED \
         account's KEY__LABEL whose base is a grid key, plus anything this box's store already \
         holds. `vike-cli config show` lists the names and what reads each one."
    )
}

/// **The refusal for a VENUE SETTING's legacy name** — `key` is a credential name a declared venue
/// field was read under until decision 0095's Task 7, and `setting` is that field's dotted key.
///
/// Names the `config set` line that writes the setting, and the stdin form for a SECRET field
/// (`venue.polymarket.socks_proxy` may carry `user:password@`, and `config set` takes such a value
/// from stdin only). Names only: the value has not been read when this is built.
fn stranded_setting_message(key: &str, setting: &str) -> String {
    let secret = vike_secrets::venue_setting::parse_venue_setting_key(setting)
        .and_then(|(venue, _, field)| vike_secrets::venue_setting::declared_field(&venue, &field))
        .is_some_and(|f| f.secret);
    let write = if secret {
        format!("vike-cli config set {setting} -   (the value on stdin)")
    } else {
        format!("vike-cli config set {setting} <value>")
    };
    format!(
        "'{key}' is not a credential — it is the venue setting `{setting}`, read from the settings \
         database (decision 0095). A credential row under this name is read by nothing. Write \
         the setting instead:\n  {write}"
    )
}

/// The ledger's venue cell for a credential that belongs to the DEPLOYMENT rather than to any
/// venue — a Cloudflare token, the Telegram pair. `CredentialTarget::venue` documents `"multi"` as
/// the same kind of word: a cell that says what the row IS when no venue owns it.
pub(super) const DEPLOYMENT_OWNER: &str = "deployment";

/// **May `set` write this key, and under whose venue does the ledger record it?**
///
/// The GRID answers first — `{VENUE}_{TIER}_{SUFFIX}`, the enumerable shape. For a name outside it
/// the answer is the REGISTRY: if `vike_ops::settings` records something reading this key, writing
/// it is not writing a line nothing would load, which is the whole property the grid was a proxy
/// for.
///
/// # ⚠ Why the grid stopped being enough
///
/// MEASURED on the CI box 2026-09-21. The bespoke keys — `DUKASCOPY_*_SERVER`, `FXCM_*_CONNECTION`,
/// `IBKR_*_HOST`, the `POLY_PROXY_*` trio, the Telegram pair — are outside the grid, and the route
/// this command used to name for them was *add it with an EDITOR*. That route DIED with
/// `docs/decisions/0054`'s credential half: the settings database is the only store, and nobody
/// edits it in an editor. Half the store became unwritable by anything, and the refusal still
/// advised the dead route.
///
/// # ⚠ What stays refused, and it is not a residual
///
/// * a **node key** — `vike-cli backend setup` MINTS those; a hand-pasted 256-bit HMAC that is
///   truncated fails as an opaque auth denial rather than as anything readable. The arm above this
///   one names the minting verb.
/// * a key **no registry row names at all** — nothing reads it under any spelling, so writing it
///   would be the dead line `vike_config::CONSUMPTION` exists to refuse.
/// * a registry name the classifier cannot place and that is NOT in
///   `vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID` — a SETTING read off the process
///   environment or the settings database, which a credential row would never reach.
///
/// # ⚠ A LABELLED account is ADMITTED, and that is a reversal
///
/// `{BASE}__{LABEL}` was refused here for being an unbounded name set, and the refusal sent the
/// operator to an editor — the same dead route, for the one family that needs a writer MOST:
/// `vike-cli secrets list` PRINTS these accounts, so the store already holds them and an operator
/// can see a credential they have no way to rotate.
///
/// The unboundedness was never an argument against WRITING one, only against ENUMERATING them:
/// [`labelled_account`] admits a label only when its BASE is a grid key, so the name is bounded by
/// the grid on the half that matters and the venue/tier come from the base rather than from a
/// guess. The genuine hazard the old arm names is a different one and survives untouched — it was
/// [`nearest_keys`] SUGGESTING the unlabelled base, which an operator then set, overwriting the
/// DEFAULT account's signing key with a second account's. Admitting the labelled name is what
/// removes the temptation: the key the operator typed is the key that is written.
///
/// The ledger cell comes from `vike_bridge_core::credentials::classify_credential_name` — the same
/// classifier the store's own migration files rows by, so a key's venue cannot read one way here
/// and another in the table. It splits `__{LABEL}` off itself and files the row under
/// `vike_secrets::AccountKey::label`, so a labelled key written here and one migrated in land on
/// the same account row.
pub(super) fn settable_outside_the_grid(key: &str) -> Option<(String, Option<String>)> {
    if vike_model::credential_keys::platform_key_service(key).is_some() {
        return None;
    }
    // ⚠ **The CLOSED list of names the workspace reads out of the credential store that the
    // classifier cannot place** — the pager trio, the Telegram control bot, the collector and
    // data-API keys, the JForex tools, the builder fees, the Studio chat pane's two provider keys.
    // Admitted BY NAME from
    // `vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID`, never by a rule over the name:
    // whether a read consults the CREDENTIAL map is a fact about its call site, and every rule
    // tried over the name either admitted `VIKE_HIST_STORE` (a store path off the process
    // environment) or refused the pager token. Before this they reached `set` only by ROTATION —
    // a box whose store did not already hold one had no writer for it at all. The ledger files
    // them where the classifier does (the deployment, for all of them today).
    // `crates/vike-cli/src/cmd/secrets/tests/set_tests.rs`'s
    // `the_settable_list_and_the_registry_agree_both_ways` holds the table to the registry.
    if vike_model::credential_keys::is_store_key_outside_the_grid(key) {
        return owner_of(&vike_bridge_core::credentials::classify_credential_name(key));
    }
    // ⚠ BEFORE the registry, because a labelled name has no `SETTINGS` row and never can: the
    // registry declares NAMES, and this family's names are generated per account.
    if let Some((base, _)) = labelled_account(key) {
        let (venue, tier) = vike_model::credential_keys::key_owner(base)?;
        return Some((venue.to_string(), tier.map(str::to_string)));
    }
    registry_readers(key)?;
    // ⚠ **AND the CLASSIFIER must RECOGNISE it**, which is the half that keeps this from admitting
    // the whole registry. `vike_ops::settings` catalogs every variable a resolvable call site
    // reads, and most of them are SETTINGS, not credentials: `VIKE_HIST_STORE` is a store path,
    // `RUST_LOG` a log filter. Neither is read out of the CREDENTIAL map — `vike_config` reads the
    // process-env sweep, a different map — so a row written for one here would be read by nothing,
    // which is precisely the dead line the refusal exists to prevent. MEASURED rather than assumed:
    // `VIKE_HIST_STORE`'s rows are `Naming::MapLookup` like a credential's, so the registry alone
    // cannot tell them apart and `naming` is not the discriminator it looks like.
    //
    // `Placement::Infrastructure` is the classifier's CATCH-ALL for a name it cannot place, so an
    // unrecognised key lands there and must NOT be read as "a deployment credential". The
    // deployment's own secrets reach `set` by the ROTATION rule instead — see [`rotation_owner`].
    let class = vike_bridge_core::credentials::classify_credential_name(key);
    if !class.recognised {
        return None;
    }
    owner_of(&class)
}

/// The `(venue, tier)` cell a classification implies, for the ledger.
///
/// Split out because [`settable_outside_the_grid`] and [`rotation_owner`] must not be able to file
/// the same key two different ways — the defect the ledger's own store-name column already had
/// once, where the sentence and the record named different stores.
fn owner_of(class: &vike_secrets::Classification) -> Option<(String, Option<String>)> {
    match &class.placement {
        vike_secrets::Placement::Account(k) => Some((k.venue.clone(), Some(k.tier.clone()))),
        vike_secrets::Placement::Venue(v) => Some((v.clone(), None)),
        // ⚠ The DEPLOYMENT's own credentials — a Cloudflare token, the Telegram trio. They belong
        // to NO venue and NO account, and the ledger says that outright rather than filing them
        // under a blank one. `CredentialTarget::venue` already carries a non-venue word for the
        // same reason (`"multi"`, when one save spanned several), so this is that column's own
        // vocabulary rather than a new convention.
        vike_secrets::Placement::Infrastructure => Some((DEPLOYMENT_OWNER.to_string(), None)),
    }
}

/// **The ROTATION rule: a name this box's store ALREADY HOLDS is settable, whatever its shape.**
///
/// This is the half that reaches the DEPLOYMENT's own secrets — the Telegram trio, a Cloudflare
/// token — without a hand-kept roster of them. They are `VIKE_*`/`CLOUDFLARE_*` names the credential
/// classifier has no positive rule for, and the registry cannot tell them from a settings key, so
/// every predicate built on NAME alone either admits `VIKE_HIST_STORE` or refuses the Telegram
/// token. Asking the STORE sidesteps the question: the row is THERE, something put it there, and
/// `vike-cli secrets list` prints it — so an operator can SEE a credential they have no way to
/// rotate, which is the exact complaint this change answers.
///
/// # ⚠ Why this cannot write a dead line
///
/// The other rules admit a name on the strength of an argument about what READS it. This one admits
/// it on the strength of the row's existence, so the worst case is overwriting a value that was
/// already there — never inventing one. A key nothing reads is not made deader by being rotated.
///
/// # ⚠ What it still refuses
///
/// A node key, for the reason [`settable_outside_the_grid`] gives: `backend setup` MINTS those, and
/// the store HOLDING one is exactly the state in which a hand-pasted replacement does the damage.
/// ⚠ It takes the map `run_set` ALREADY resolved rather than opening the store itself — the same
/// read that answers `replaced`. `SecretMap` deliberately exposes key NAMES and no values, so this
/// predicate cannot see a credential even by accident.
pub(super) fn rotation_owner(
    key: &str,
    live: &vike_secrets::SecretMap,
) -> Option<(String, Option<String>)> {
    if vike_model::credential_keys::platform_key_service(key).is_some() {
        return None;
    }
    if !live.keys().any(|k| k == key) {
        return None;
    }
    owner_of(&vike_bridge_core::credentials::classify_credential_name(key))
}

/// **The crates `vike_ops::settings` records as READING `key`**, deduplicated and rendered — or
/// `None` when no row names it at all.
///
/// `None` is the whole point: it is the one state in which [`unknown_key_message`]'s original
/// sentence ("setting it would write a line nothing would ever load") is a true claim about the
/// workspace rather than about this command — and, since this function became
/// [`settable_outside_the_grid`]'s admission test, the one state in which `set` still refuses a
/// name outright.
///
/// ⚠ **The registry is the authority here rather than a table of our own, and that is the design.**
/// A hand-kept "these names are read elsewhere" list is exactly the shape this repository has
/// watched rot: `vike_ops::settings::all_settings` is the workspace's own catalog of every environment
/// variable a resolvable call site reads, and `crates/vike-ops/tests/settings_secrets/settings_registry.rs` fails CI
/// in BOTH directions over it — an undeclared read is red, and so is a row nothing reads any more.
/// A second roster here would go stale between those two gates with nothing to notice. It also
/// costs no dependency: this crate already links `vike-ops` (taken plainly — `crates/vike-cli/Cargo.toml`
/// carries why) for `config show`, whose env half is driven by the same table.
///
/// ⚠ **What it deliberately does NOT answer: WHICH store the reader consults.** The tempting
/// refinement is to split the message on
/// `vike_ops::settings::Setting::naming` — `MapLookup` ⇒ a caller-supplied map (so the credential
/// store is a plausible route), `Literal`/`Konst` ⇒ a direct `env::var` (so it is not). The first
/// half holds; **the second does not**, and a message built on it would have shipped a fresh
/// instance of the very lie this function exists to remove. `settings.rs`' own tie-break says
/// `naming` records the DIRECT read when one name is read at BOTH kinds of site, and
/// `crates/vike-backfill/src/bin/databento_backfill.rs`'s `api_key` is the counterexample in the
/// tree: its key's row is declared `Naming::Literal`, and that function asks the credential STORE
/// first and only then falls back to `env::var`. So a `Literal` row proves a direct read exists and
/// proves nothing about the store. The message states the hedge instead and sends the operator to
/// `vike-cli config show`, which resolves both stores and prints the answer for real —
/// `crates/vike-cli/src/cmd/config/resolve.rs`'s `Reads` carries the same limitation from its own side.
///
/// The crate names are rendered here rather than returned as a list because there is exactly one
/// RENDERING caller and one sentence; a `Vec` would be a second shape for it.
pub(super) fn registry_readers(key: &str) -> Option<String> {
    let mut krates: Vec<&'static str> =
        all_settings().filter(|s| s.name == key).map(|s| s.krate).collect();
    krates.sort_unstable();
    krates.dedup();
    (!krates.is_empty()).then(|| krates.join(", "))
}

/// The valid key names closest to `key` — at most three, and only when they are genuinely close.
///
/// Case first, edit distance second. An operator typing a key in lower case is the single most
/// likely near-miss and is an exact match one `to_uppercase` away, while Levenshtein scores it as
/// far away as a different venue — every letter differs.
///
/// The distance CEILING is what keeps this honest: with no bound, a nonsense key returns three
/// unrelated names presented as guesses, which is worse than the bare refusal. It scales with the
/// name's length, because these names are long and a one-character slip in
/// `HYPERLIQUID_LIVE_API_PASSPHRASE` should still be caught.
pub(super) fn nearest_keys(key: &str) -> Vec<String> {
    // A labelled account's base is always within the ceiling below (a `__LABEL` suffix costs a
    // handful of edits against names this long), so without this it is ALWAYS the first suggestion —
    // and it is a different ACCOUNT's real, settable key. Asked here as well as in
    // [`unknown_key_message`]'s own arm so the dangerous suggestion cannot come back through a
    // second caller; [`labelled_account`] is the one place the question is answered.
    if labelled_account(key).is_some() {
        return Vec::new();
    }
    // The grid AND the closed list outside it — both are names `set` writes, so a typo of the
    // pager token is offered the real spelling exactly as a typo of a venue key is.
    let mut all = vike_model::credential_keys::lookup_keys();
    all.extend(
        vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
            .iter()
            .map(|(name, _)| (*name).to_string()),
    );
    let upper = key.to_uppercase();
    if let Some(exact) = all.iter().find(|k| **k == upper) {
        return vec![exact.clone()];
    }
    let ceiling = (key.len() / 3).clamp(2, 6);
    let mut scored: Vec<(usize, String)> = all
        .into_iter()
        .map(|k| (edit_distance(&upper, &k), k))
        .filter(|(d, _)| *d <= ceiling)
        .collect();
    // Distance first, then the NAME, so the list is deterministic — two candidates at the same
    // distance must not reorder between runs.
    scored.sort();
    scored.into_iter().take(3).map(|(_, k)| k).collect()
}

/// `(base, label)` when `key` is a LABELLED ACCOUNT name whose base is a real credential key —
/// `HYPERLIQUID_LIVE_API_KEY__ALT` — and `None` otherwise.
///
/// The grammar is `vike_model::account_keys`': split at the FIRST `ACCOUNT_SEPARATOR`, which is a
/// DOUBLE underscore. The base must RESOLVE, so a single-underscore near-miss
/// (`..._API_KEY_ALT`, which nothing reads) is not one of these and still gets the ordinary
/// refusal with its suggestions — that name is a typo of a settable key, and this one is not.
pub(super) fn labelled_account(key: &str) -> Option<(&str, &str)> {
    let (base, label) = key.split_once(vike_model::accounts::account_keys::ACCOUNT_SEPARATOR)?;
    vike_model::credential_keys::key_owner(base).is_some().then_some((base, label))
}

/// Levenshtein distance, two rows. Written out rather than pulled in: this crate's identity is
/// adding no dependency, and the whole algorithm is nine lines.
pub(super) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The durable half of [`run_set`]: ONE `credential_write` record for the key that was just
/// written.
///
/// ⚠ **It appends DIRECTLY rather than through `vike_secrets::save_credentials_to_store_journalled`,
/// the journalled writer the GUI calls**, which this crate already links. Whether this path should
/// switch to it is a code decision this note does not take;
/// `crates/bridges/ctrader/src/token_store.rs`'s `record_rotation` is in the same position.
///
/// What keeps the spellings honest is not a shared function but a GATE:
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins the EXACT set of files that call
/// `save_credentials_to_store` or its journalled wrapper, with a reason per row, so a THIRD writer cannot
/// appear unnoticed and a row whose caller is gone cannot linger.
///
/// ⚠ Nothing here can put a credential in the ledger:
/// `vike_model::change_journal::Change::credential_write` takes no old/new/value parameter at all,
/// and the only cell fed to it from this write is the key NAME.
fn record_write(ctx: &Ctx<'_>, store: &Path, key: &str, venue: &str, tier: Option<&str>) {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    // No project above the working directory ⇒ NO ledger. Nothing is recorded, rather than an
    // append-only record in a guessed directory — `vike_boot::journal_boot_settings`' rule.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    // The FILE NAME of the store the write LANDED in — `vike.db` — not the path; the ledger sits
    // under the same `<project>/settings` the store does. Deriving it from a constant instead is
    // what once produced a ledger naming a file the key never reached.
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::DB_FILE);
    // An ATTRIBUTION key has no tier — a broker/builder code is per-venue — so the cell is empty
    // rather than carrying a tier that was never part of the name.
    let change = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        file,
        venue,
        tier.unwrap_or(""),
        &[key],
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The key IS saved, so
        // this is a finding about the ledger and not about the write.
        eprintln!(
            "vike-cli secrets: ⚠ {key} was saved, but the change journal in {} could not record \
             it: {e}",
            journal.dir().display()
        );
    }
}
