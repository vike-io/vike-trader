//! `vike-cli secrets account ACTION` — the account table's LIFECYCLE: `add`, `rename`, `set-tier`,
//! `deactivate`, `activate` and `remove`.
//!
//! One subcommand with an ACTION positional rather than six subcommands, for the reason `Sub::Account`
//! carries. Split out of `cmd/secrets.rs` (code-layout phase 2, task 10); the design is
//! `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §5. This file holds the
//! call sites of `vike_secrets::edit_account_in`, which is what
//! `crates/vike-ops/tests/credential_writer_gate.rs` pins.

use std::path::Path;

use vike_model::change_journal::Actor;

use super::*;
use crate::exit::{CliError, CmdResult};

/// **THE `account` ACTION LIST — ONE spelling, and it was two until `set-tier` was added.**
///
/// ⚠ [`account_action_missing`]'s doc has always claimed the list was *"spelled once, because the
/// parser and the run path both need it and a second copy would drift"*, and the claim was FALSE
/// when it was written: `parse`'s second-positional refusal (*"`account` takes ONE action: add |
/// rename | …"*) carried a hand copy, and the comment above it called an action *"one of five
/// words"*. Two copies and a count, none of them derived. Adding a sixth action is exactly the edit
/// that would have drifted them, so the claim is made TRUE here rather than left as a comment about
/// an intention — both refusals and the `unknown action` message now render this array.
///
/// It is NOT the dispatcher: `run_account`'s `match` is still spelled out, because a list cannot
/// say what each action DOES. What this removes is a roster that can disagree with itself, and its
/// `other =>` arm is what catches a word that is in neither.
pub(super) const ACCOUNT_ACTIONS: [&str; 6] =
    ["add", "rename", "set-tier", "deactivate", "activate", "remove"];

/// What `account` with no ACTION — or with a word that is no action — is refused with. A function
/// rather than a `const` so it can render [`ACCOUNT_ACTIONS`]; the examples below stay literal
/// because an example is a whole command line, not a word in a list.
pub(super) fn account_action_missing() -> String {
    format!(
        "`account` needs an ACTION: {}.\n  vike-cli secrets account add --venue binance --tier \
         live --label HEDGE\n  vike-cli secrets account rename --id 7 --label SWISS\n  vike-cli \
         secrets account set-tier --id 7 --tier demo\n  vike-cli secrets account deactivate --id \
         7\n  vike-cli secrets account remove --id 7 --confirm 7\nRun `vike-cli secrets accounts` \
         for the ids, and add --dry-run to any of these to see the row without changing it.",
        ACCOUNT_ACTIONS.join(" | ")
    )
}

/// An `account` action that takes `--tier` and was given none — pulled into its own function so the
/// pinning test below calls the exact string production sends rather than a hand-copied twin of it.
///
/// ⚠ **This used to deny `paper` by name while printing the very roster that contains it** — the
/// 2026-09-23 `sim` -> `paper` rename (ruling 7) put `paper` INTO [`vike_secrets::ACCOUNT_TIERS`],
/// and this message's old text (*"there is no `paper` here and there must not be"*) was never
/// swept, so the rendered refusal contradicted itself in one sentence and `--tier paper` silently
/// started succeeding under it. Corrected to state what the code does: `paper` names the account a
/// `{VENUE}_SIM_*` credential mints, and it is unrelated to the `policy.venues.<venue>` row's own
/// `paper` CEILING, which is a separate concept that happens to share the word.
///
/// ⚠ `action` is a PARAMETER because two actions take `--tier` now (`add` and `set-tier`), and a
/// message hard-coding `add` would send the operator of the other one to the wrong command.
pub(super) fn tier_missing_message(action: &str) -> String {
    format!(
        "`account {action}` needs --tier, one of: {}. `paper` is a tier, not the \
         policy.venues.<venue> CEILING of the same name: it is the account a {{VENUE}}_SIM_* \
         credential mints, and it is NOT what arms a venue. A venue with no credential at all \
         still has no row.",
        vike_secrets::ACCOUNT_TIERS.join(" | ")
    )
}

/// `--tier` naming a word outside [`vike_secrets::ACCOUNT_TIERS`] — same reason as
/// [`tier_missing_message`], and the same rename left the same contradiction in this twin.
///
/// ⚠ It DOES echo the word, unlike `vike_secrets::DbErrorKind::AccountTierUnknown` one layer down,
/// and the split is deliberate: here the token came off the command line the operator is still
/// looking at, so naming it back is what turns a refusal into a fix. The store's own refusal
/// cannot know that about its caller, so it names the vocabulary and nothing else.
pub(super) fn tier_unknown_message(action: &str, tier: &str) -> String {
    format!(
        "unknown tier '{tier}'. `account {action} --tier` takes one of: {}. `paper` IS one of \
         them — it is the account a {{VENUE}}_SIM_* credential mints, not the \
         policy.venues.<venue> CEILING of the same name.",
        vike_secrets::ACCOUNT_TIERS.join(" | ")
    )
}

/// **`account ACTION` — the account table's LIFECYCLE**, through
/// `vike_secrets::edit_account_in`: the Backend-aware router, never the db function directly, so
/// this verb exercises the store choice as well as the write.
///
/// # What this verb is FOR, and what it deliberately is not
///
/// Before it, the only accounts that existed on a box were the ones the migration derived from
/// credential key NAMES — plus the ones `crates/vike-secrets/src/schema.rs`'s
/// `AccountResolver::resolve` creates as a SIDE EFFECT of saving a credential whose name the
/// classifier has never seen. So an operator who wanted a second account of a venue could only get
/// one by writing a `__LABEL` key and hoping. This is the deliberate act.
///
/// ⚠ **It arms NOTHING.** `policy.venues.<venue>` is consulted ABOVE the credential read by
/// `vike_mount::make_engine`, so an account plus its keys leaves the venue on PAPER until a policy
/// edit. The reply says so on every `add`, because an operator who adds an account expects it to
/// trade and the verb has to tell them it will not.
///
/// # The ceremony, and whose shape it is
///
/// `remove` requires `--confirm` to equal the `--id` EXACTLY. That is
/// `crates/vike-tradehub/src/server.rs`'s `apply_set_setting` contract verbatim — the client's job
/// is to make the operator TYPE it, never pre-fill it, and the acceptance path's job is to refuse
/// anything else, *because the friction IS the protection*. Missing and mismatched confirms get
/// distinct messages, the same split that arm makes.
///
/// # `set-tier`, and why it has no ceremony of its own
///
/// It moves `account.tier`, the cure for a row `add`ed at the wrong one — which until it existed
/// could only be reached through `remove`, itself refused the moment the row owned a credential.
/// It takes no `--confirm` because the act is reversible by running it again, and because its own
/// refusal already stands where the damage would be: a row whose credential keys SPELL another
/// tier cannot move at all (`vike_secrets::DbErrorKind::AccountKeysPinTheTier`). What it DOES owe
/// the operator is the consequence that lands elsewhere — the derived `armed` bit is recomputed
/// against the new tier — and the reply says so on every run, rehearsal included.
///
/// # What it never does
///
/// It creates no database (`vike-cli secrets migrate` is still the only thing that may — and an
/// account verb that created one would make every credential in `secrets.env` unread on that box
/// in the same act), writes and reads no `credential` row's VALUE, touches neither credential FILE,
/// and prints no value on any path. The one credential fact it prints is key NAMES, which
/// `vike-cli secrets list` already prints by an explicit decision.
pub(super) fn run_account(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let action = args.account_action.as_deref().expect("parse refuses `account` with no action");
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let store = vike_secrets::db_path_in(&settings);

    // ⚠ THE STORE IS NAMED BEFORE ANYTHING ELSE, and on the REHEARSAL as much as on the apply —
    // `run_set_book`'s rule, for its reason: a rehearsal on the wrong box (the wrong checkout, an
    // inherited `VIKE_SETTINGS_DIR`, an ssh session one hop from where the operator thinks they
    // are) reads exactly like a rehearsal on the right one.
    println!("store: {}", store.display());

    // The table, and the `Backend::Files` refusal — asked BEFORE anything is validated, so an
    // unmigrated box is told the one thing it needs rather than being walked through a grammar
    // lesson about a table it does not have.
    let accounts = vike_secrets::resolve_accounts_in(&settings).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. Nothing was written."
        ))
    })?;
    let rows = match &accounts {
        vike_secrets::Accounts::Known(rows) => rows,
        vike_secrets::Accounts::Unanswerable(why) => {
            return Err(CliError::failed(format!(
                "{why} — so there is no account table to edit. NOTHING WAS WRITTEN and no database \
                 was created. Move this box into the settings database first:\n  vike-cli secrets \
                 migrate --dry-run\n  vike-cli secrets migrate"
            )));
        }
    };

    // The LABEL, through `vike_model::accounts::account_keys::AccountLabel::parse` — the AUTHORITY for the
    // grammar. Its `AccountKeyError` names the rule that was broken, which is why this refusal does
    // not restate one. `vike_secrets::normalized_account_label` is the store's own floor under it
    // and refuses the same set (`crates/vike-bridge-core/tests/account_label_spellings.rs`).
    //
    // ⚠ The refusal does NOT echo the token. On this verb the label flag sits beside nothing that
    // carries a secret — but the WIRE verb this shares a store with does, and a message that
    // quoted its argument is the shape `ARGV_VALUE_REFUSAL` exists to refuse. One rule, both
    // surfaces.
    let label: Option<&str> = match (args.label.as_deref(), args.no_label) {
        (Some(raw), _) => {
            let trimmed = raw.trim();
            if let Err(e) = vike_model::accounts::account_keys::AccountLabel::parse(trimmed) {
                return Err(CliError::usage(format!(
                    "--label: {e} Nothing was written, and what you typed is deliberately not \
                     quoted back here."
                )));
            }
            Some(trimmed)
        }
        (None, _) => None,
    };

    match action {
        "add" => {
            let Some(venue) = args.venue.as_deref() else {
                return Err(CliError::usage(
                    "`account add` needs --venue: an account belongs to exactly one venue, and \
                     there is no default. `vike-cli secrets accounts` prints the venues this store \
                     already has rows for."
                        .to_string(),
                ));
            };
            // The roster check is here rather than in `parse` so the refusal can PRINT the roster —
            // the same late-validation rule `template --venue` obeys.
            if !vike_model::VENUES.contains(&venue) {
                return Err(CliError::usage(format!(
                    "unknown venue '{venue}'. The roster is: {}",
                    vike_model::VENUES.join(", ")
                )));
            }
            let Some(tier) = args.tier.as_deref() else {
                return Err(CliError::usage(tier_missing_message("add")));
            };
            if !vike_secrets::ACCOUNT_TIERS.contains(&tier) {
                return Err(CliError::usage(tier_unknown_message("add", tier)));
            }
            if args.account_id.is_some() {
                return Err(CliError::usage(
                    "`account add` does not take --id: the id is assigned BY the insert and is \
                     the identity, so a caller naming one is naming a row that already exists. \
                     Nothing was written."
                        .to_string(),
                ));
            }
            if args.label.is_none() && !args.no_label {
                return Err(CliError::usage(
                    "`account add` needs --label LABEL or --no-label. An unlabelled row is the \
                     account this venue's PLAIN keys already address, and there may be only one \
                     per (venue, tier) — so which of the two you meant is not a thing this command \
                     will guess at. Nothing was written."
                        .to_string(),
                ));
            }
            println!("would add: venue={venue}  tier={tier}  label={}", label.unwrap_or("(none)"));
            if args.dry_run {
                println!(
                    "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same \
                     command without --dry-run.",
                    store.display()
                );
                return Ok(());
            }
            let done = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::Create { venue, tier, label },
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            let row = done.after.as_ref().expect("a create returns its row");
            println!("\nadded account {} in {}", row.id, store.display());
            // ⚠ THE SENTENCE THE VERB OWES. An operator who adds an account expects it to trade.
            println!(
                "⚠ this arms NOTHING. A row is not a ceiling: policy.venues.{venue} is read ABOVE \
                 the credential store by the mount, so this venue stays PAPER until that line says \
                 otherwise. The account also holds no credentials yet — `vike-cli secrets set` is \
                 what puts them there."
            );
            Ok(())
        }
        "rename" => {
            let id = require_account_id(args, "rename")?;
            let row = require_row(rows, id)?;
            echo_row(&settings, row);
            if args.label.is_none() && !args.no_label {
                return Err(CliError::usage(
                    "`account rename` needs --label LABEL or --no-label. ⚠ --no-label CLEARS the \
                     label, which is an act with consequences — a policy.accounts line naming it \
                     stops resolving at the next restart — so it is not what a forgotten flag \
                     does. Nothing was written."
                        .to_string(),
                ));
            }
            println!(
                "  label: {} -> {}",
                row.label.as_deref().unwrap_or("(none)"),
                label.unwrap_or("(none)")
            );
            // ⚠ The consequences a rehearsal must state, because they land ELSEWHERE and LATER:
            // `crates/vike-mount/src/node.rs`'s `refuse_unarmed_mount_accounts` and `vike_dukascopy::resolve_account`
            // both address by the label STRING, so renaming a row a policy line
            // names turns an armed account into a REFUSED node at the next restart — and the
            // operator who renames and the operator who restarts need not be the same person.
            //
            // ⚠ **This block used to call that failure "an outage, never a misroute". It is not.**
            // Measured 2026-09-17: every catch fires on a label that STOPS resolving, and none
            // looks at one that now resolves to a DIFFERENT row. Rename B away from `ALT`, then
            // rename A to `ALT`, and the policy line resolves again — to the other account. On
            // dukascopy the two demo accounts are different LEGAL ENTITIES
            // (`DukascopyAccount::key_prefix` discriminates inside the base name, so the
            // `AccountKeysPinTheLabel` guard — which looks for keys ending in `__ALT` — cannot fire
            // on the one venue where a label selects a broker). The second warning below is the
            // honest replacement for the sentence that was there. `server.rs`'s `Rename` arm
            // carries the full sequence and names the guard 0065 §5 specifies to close it.
            if let Some(old) = row.label.as_deref() {
                println!(
                    "  ⚠ if a `policy.accounts.{}.{old}` row names this account, that \
                     row will stop resolving: a mount addressing it is REFUSED at the next \
                     restart. Update the policy row too.",
                    row.venue
                );
                println!(
                    "  ⚠ and if `{old}` is later given to a DIFFERENT {} account, nothing refuses \
                     anything — the policy line resolves again, to that other account. On \
                     dukascopy the demo accounts are different LEGAL ENTITIES, so that is an order \
                     routed to a broker nobody chose. Check which row the label names before you \
                     restart.",
                    row.venue
                );
            }
            if args.dry_run {
                println!(
                    "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same \
                     command without --dry-run.",
                    store.display()
                );
                return Ok(());
            }
            let done = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::Rename { id, label },
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            if done.changed {
                println!("\nrenamed account {id} in {}", store.display());
            } else {
                println!("\nunchanged — that row already carried this label. Nothing was written.");
            }
            Ok(())
        }
        "set-tier" => {
            let id = require_account_id(args, "set-tier")?;
            let row = require_row(rows, id)?;
            echo_row(&settings, row);
            // ⚠ The LABEL flags are refused on this action rather than ignored. `parse` admits both
            // on every `account` action (one guard, `sub != Sub::Account`), so an operator who
            // reached for `rename`'s muscle memory and typed `--label` beside `--tier` would have
            // it silently dropped — which is how somebody comes to believe a row was renamed. This
            // verb moves a tier and nothing else; renaming is its own action, and a row cannot be
            // moved and renamed in one transaction because each has its own refusal to make.
            if args.label.is_some() || args.no_label {
                return Err(CliError::usage(
                    "`account set-tier` takes neither --label nor --no-label: it moves the TIER \
                     and nothing else. Rename with `account rename --id N --label LABEL`, which \
                     has its own refusal to make about credential keys that spell the old label. \
                     Nothing was written."
                        .to_string(),
                ));
            }
            let Some(tier) = args.tier.as_deref() else {
                return Err(CliError::usage(tier_missing_message("set-tier")));
            };
            if !vike_secrets::ACCOUNT_TIERS.contains(&tier) {
                return Err(CliError::usage(tier_unknown_message("set-tier", tier)));
            }
            println!("  tier: {} -> {tier}", row.tier);
            // ⚠ THE REFUSAL THIS VERB IS SHAPED BY, stated at the REHEARSAL because the row's key
            // names are on the screen directly above and this is where an operator can read them.
            // It is deliberately NOT predicted as a yes/no the way `remove`'s is: `remove` is
            // refused by the mere EXISTENCE of a live key, which `done_keys_block` can answer from
            // out here, while this one turns on what each key NAME SPELLS — a question only
            // `vike_secrets::edit_account` can ask, and one this command would have to re-derive
            // (wrongly, on the first venue whose tier token is not its tier word) to answer here.
            if done_keys_block(&settings, id) {
                println!(
                    "\n⚠ this row owns the credential keys listed above, and THEY DO NOT MOVE — \
                     nothing in this workspace rewrites a credential name. If any of them spells a \
                     tier other than `{tier}`, the apply is REFUSED and names them: the classifier \
                     would read the old tier out of those names again and CREATE A SECOND ACCOUNT \
                     the next time one of them is written. Moving an account that HAS keys means \
                     moving the KEYS — write them under the new tier's names and retire the old \
                     ones."
                );
            }
            // ⚠ The consequence that lands ELSEWHERE, and the one an operator must not learn later:
            // `account.armed` is DERIVED — an account is armed exactly where the operator's arming
            // mode names the tier the row CARRIES — so moving a row moves it in and out of that
            // answer. Said with its current reach rather than inflated: the mount still consults
            // `venue_arming` through `vike_config::VenuePolicy`, so this changes nothing a venue
            // does TODAY, and it changes the store's own answer about this account immediately.
            println!(
                "  ⚠ the row's derived `armed` bit is recomputed from policy.venues.{} against the \
                 NEW tier. That is the store's answer, not the mount's — the mount still reads the \
                 arming table, so nothing a venue does changes until that line does. A RUNNING \
                 daemon notices neither: its arming snapshot is read ONCE at boot.",
                row.venue
            );
            if args.dry_run {
                println!(
                    "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same \
                     command without --dry-run.",
                    store.display()
                );
                return Ok(());
            }
            let done = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::SetTier { id, tier },
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            if done.changed {
                println!("\nmoved account {id} to tier {tier} in {}", store.display());
            } else {
                println!("\nunchanged — that row was already at this tier. Nothing was written.");
            }
            Ok(())
        }
        verb @ ("deactivate" | "activate") => {
            let id = require_account_id(args, verb)?;
            let row = require_row(rows, id)?;
            echo_row(&settings, row);
            let active = verb == "activate";
            if args.dry_run {
                println!(
                    "\nwould set active={} on account {id}.\nthis was a DRY RUN — nothing was \
                     written to {}.",
                    if active { "yes" } else { "no" },
                    store.display()
                );
                return Ok(());
            }
            let done = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::SetActive { id, active },
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            if !done.changed {
                println!("\nunchanged — that row was already {verb}d. Nothing was written.");
                return Ok(());
            }
            println!("\n{verb}d account {id} in {}", store.display());
            if !active {
                // ⚠ The two residuals `AccountEdit::SetActive`'s own doc names, said to the
                // operator rather than left in a doc comment they are not reading.
                println!(
                    "⚠ a RUNNING daemon does not notice: the arming snapshot is read ONCE at boot, \
                     so its engines keep their credentials and keep trading until it restarts."
                );
                println!(
                    "⚠ the row and its credential keys are still there — that is what makes this \
                     reversible (`account activate --id {id}`), and it is why this is the act to \
                     reach for rather than `remove`."
                );
            }
            Ok(())
        }
        "remove" => {
            let id = require_account_id(args, "remove")?;
            let row = require_row(rows, id)?;
            echo_row(&settings, row);
            // ⚠ THE CEREMONY, server-side-shaped: `apply_set_setting`'s policy contract verbatim.
            // Missing and mismatched get DISTINCT messages, each naming the expected spelling, and
            // nothing pre-fills it — the friction IS the protection.
            match args.confirm.as_deref() {
                None => {
                    return Err(CliError::usage(format!(
                        "`account remove` DELETES a row — a typed confirm is required: re-send \
                         with --confirm {id}. (Deactivating is the reversible act and the one to \
                         reach for: `vike-cli secrets account deactivate --id {id}`.)"
                    )));
                }
                Some(c) if c.trim() != id.to_string() => {
                    return Err(CliError::usage(format!(
                        "confirm mismatch: --confirm must equal the exact id being removed \
                         ({id}) — nothing was written."
                    )));
                }
                Some(_) => {}
            }
            if args.dry_run {
                if done_keys_block(&settings, id) {
                    println!(
                        "\n⚠ the apply would be REFUSED: this row still owns live credential keys \
                         (listed above). Remove them first, or DEACTIVATE this account instead."
                    );
                } else {
                    println!("\nthe apply would DELETE account {id}. This cannot be undone.");
                }
                println!("this was a DRY RUN — nothing was written to {}.", store.display());
                return Ok(());
            }
            let done =
                vike_secrets::edit_account_in(&settings, vike_secrets::AccountEdit::Remove { id })
                    .map_err(|e| CliError::failed(e.to_string()))?;
            record_account_lifecycle(ctx, &store, &done);
            println!("\nremoved account {id} from {}", store.display());
            Ok(())
        }
        other => Err(CliError::usage(format!(
            "unknown `account` action '{other}'.\n{}",
            account_action_missing()
        ))),
    }
}

/// `--id`, or the refusal that names the listing verb. Every `account` action but `add` addresses a
/// ROW, and there is no default: a verb that guessed would be guessing which account it edits.
/// (Spelled as "every one but `add`" rather than as a fraction — the fraction here was written as
/// *four of five* and went stale the moment [`ACCOUNT_ACTIONS`] grew a sixth.)
fn require_account_id(args: &Args, action: &str) -> CmdResult<i64> {
    args.account_id.ok_or_else(|| {
        CliError::usage(format!(
            "`account {action}` needs --id N — the `id` column `vike-cli secrets accounts` prints \
             with each row's venue, tier and CREDENTIAL KEY NAMES beside it. The id is the \
             identity; a label is not."
        ))
    })
}

/// The row, or the refusal `set_venue_account_id` makes for the same input: an id no row carries is
/// a TYPO, never an instruction to create one.
fn require_row(rows: &[vike_secrets::Account], id: i64) -> CmdResult<&vike_secrets::Account> {
    rows.iter().find(|a| a.id == id).ok_or_else(|| {
        CliError::usage(format!(
            "no account with id {id} in this store — and none was created, because the id IS the \
             identity. `vike-cli secrets accounts` lists the {} row(s) this store holds.",
            rows.len()
        ))
    })
}

/// Print the row an action is about, plus its credential key NAMES.
///
/// ⚠ The key names are the point, for `run_accounts`' reason: on two rows of one venue at one tier
/// with both labels blank — which is what a migration leaves — every other cell is identical, so
/// the echo would confirm nothing an operator could check `--id` against. **NAMES only, never a
/// value**: `vike_secrets::AccountKeys`' statement has no `value` column in it.
fn echo_row(settings: &Path, row: &vike_secrets::Account) {
    println!(
        "account {}  venue={}  tier={}  label={}  active={}  venue_account_id={}",
        row.id,
        row.venue,
        row.tier,
        row.label.as_deref().unwrap_or("(none)"),
        if row.active { "yes" } else { "no" },
        row.venue_account_id.as_deref().unwrap_or("(not yet known)")
    );
    match vike_secrets::resolve_account_keys_in(settings) {
        Ok(Some(map)) => match map.get(&row.id) {
            Some(keys) if !keys.names.is_empty() => {
                println!("  credential keys: {}", keys.names.join(", "));
            }
            _ => println!(
                "  credential keys: (none — no live credential row names this account, so nothing \
                 but the id identifies it)"
            ),
        },
        // Loud, never a blank line: this echo is the only check the operator has on `--id`.
        Ok(None) | Err(_) => println!(
            "  credential keys: ⚠ could not be read — this row is identified by its id alone here"
        ),
    }
}

/// Does this row still own live credential keys? The REHEARSAL's half of the remove refusal — the
/// apply asks the same question inside its own transaction, which is the one that decides.
fn done_keys_block(settings: &Path, id: i64) -> bool {
    matches!(
        vike_secrets::resolve_account_keys_in(settings),
        Ok(Some(map)) if map.get(&id).is_some_and(|k| !k.names.is_empty())
    )
}

/// Append the durable record for an account lifecycle write — `Actor::cli`, because on this verb a
/// human at a keyboard performed it.
///
/// ⚠ **Key NAMES only, and the signature is the enforcement**:
/// `vike_model::change_journal::Change::account_lifecycle` takes no value parameter, exactly as
/// `credential_write` and `account_book` do not.
fn record_account_lifecycle(ctx: &Ctx<'_>, store: &Path, done: &vike_secrets::AccountWrite) {
    use vike_model::change_journal::{Change, ChangeJournal, Outcome, Proc};

    // Nothing to record when nothing changed: a ledger line for a no-op reads as an edit that did
    // not happen, which is `record_book_write`'s rule and for its reason.
    if !done.changed {
        return;
    }
    // No project above the working directory ⇒ NO ledger, rather than an append-only record in a
    // guessed directory.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::DB_FILE);
    // The row that EXISTS after the act, falling back to the one that was deleted — a `remove` has
    // no `after`, and its `before` is the only description of what went.
    let Some(row) = done.after.as_ref().or(done.before.as_ref()) else { return };
    let keys: Vec<&str> = done.keys.iter().map(String::as_str).collect();
    let change = Change::account_lifecycle(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        file,
        done.verb,
        row.id,
        &row.venue,
        &row.tier,
        done.before.as_ref().and_then(|b| b.label.as_deref()),
        done.after.as_ref().and_then(|a| a.label.as_deref()),
        done.after.as_ref().is_some_and(|a| a.active),
        &keys,
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The row IS written, so
        // this is a finding about the ledger and not about the write.
        eprintln!(
            "vike-cli secrets: ⚠ account {} was written, but the change journal in {} could not \
             record it: {e}",
            row.id,
            journal.dir().display()
        );
    }
}
