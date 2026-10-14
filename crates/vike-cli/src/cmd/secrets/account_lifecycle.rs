//! `vike-cli secrets account ACTION` — the account table's LIFECYCLE: `add`, `rename`, `set-tier`,
//! `set-exposure`, `deactivate`, `activate` and `remove`.
//!
//! ⚠ **An ACTIVE row trades at its own tier from the next restart**: the `account` table's `tier`
//! and `active` ARE the arming, and no per-venue ceiling stands above them
//! (`crates/vike-tradehub-core/src/arming.rs`'s `account_tier` reads them).
//! So `add`, `activate` and `set-tier` each ARM an account, and every reply here says so.
//!
//! One subcommand with an ACTION positional rather than one subcommand per action, for the reason
//! `Sub::Account` carries. Split out of `cmd/secrets.rs` (code-layout phase 2, task 10); the
//! design is `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §5. This
//! file holds the call sites of `vike_secrets::edit_account_in`, which is what
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins.

use vike_change_journal::Actor;

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
pub(super) const ACCOUNT_ACTIONS: [&str; 7] =
    ["add", "rename", "set-tier", "set-exposure", "deactivate", "activate", "remove"];

/// What `account` with no ACTION — or with a word that is no action — is refused with. A function
/// rather than a `const` so it can render [`ACCOUNT_ACTIONS`]; the examples below stay literal
/// because an example is a whole command line, not a word in a list.
pub(super) fn account_action_missing() -> String {
    format!(
        "`account` needs an ACTION: {}.\n  vike-cli secrets account add --venue binance --tier \
         live --label HEDGE\n  vike-cli secrets account rename --id 7 --label SWISS\n  vike-cli \
         secrets account set-tier --id 7 --tier demo\n  vike-cli secrets account set-exposure \
         --id 7 --max-exposure 5000\n  vike-cli secrets account deactivate --id 7\n  vike-cli \
         secrets account remove --id 7 --confirm 7\nRun `vike-cli secrets accounts` \
         for the ids, and add --dry-run to any of these to see the row without changing it.",
        ACCOUNT_ACTIONS.join(" | ")
    )
}

/// An `account` action that takes `--tier` and was given none — pulled into its own function so the
/// pinning test below calls the exact string production sends rather than a hand-copied twin of it.
///
/// ⚠ **This used to deny `paper` by name while printing the very roster that contains it** — the
/// 2026-09-23 `sim` -> `paper` rename (ruling 7) put `paper` INTO the tier vocabulary
/// (`vike_model::AccountTier` since decision 0129),
/// and this message's old text (*"there is no `paper` here and there must not be"*) was never
/// swept, so the rendered refusal contradicted itself in one sentence and `--tier paper` silently
/// started succeeding under it. Corrected to state what the code does: an active row at tier
/// `paper` trades on the paper simulator only. And since decision 0156 item 6 this flag is the ONE
/// place an account's tier is written — no credential write creates an account or chooses its
/// tier — so the message says that too.
///
/// ⚠ `action` is a PARAMETER because two actions take `--tier` now (`add` and `set-tier`), and a
/// message hard-coding `add` would send the operator of the other one to the wrong command.
pub(super) fn tier_missing_message(action: &str) -> String {
    format!(
        "`account {action}` needs --tier, one of: {}. The tier is what this account TRADES at \
         while it is active, from the next restart. `paper` is a tier too: the paper simulator. \
         The tier is written here and nowhere else — no credential write creates an account or \
         chooses its tier.",
        vike_model::AccountTier::legal_words()
    )
}

/// `--tier` naming a word `vike_model::AccountTier`'s parser refuses — same reason as
/// [`tier_missing_message`], and the same rename left the same contradiction in this twin.
///
/// ⚠ It DOES echo the word: the token came off the command line the operator is still looking at,
/// so naming it back is what turns a refusal into a fix. This is the ONE refusal of a bad tier
/// word on this path: the store takes an `AccountTier` (decision 0129), so its old
/// `AccountTierUnknown`, which deliberately did not echo, has no input left to answer.
pub(super) fn tier_unknown_message(action: &str, tier: &str) -> String {
    format!(
        "unknown tier '{tier}'. `account {action} --tier` takes one of: {}. `paper` IS one of \
         them — the paper simulator.",
        vike_model::AccountTier::legal_words()
    )
}

/// `tier` as the operator reads it beside `venue`: the word, and for `demo` what the venue ITSELF
/// calls that environment — `demo (ibkr: paper account)` — from `vike_venues::venue_demo_word`
/// (decision 0156 item 8). A display hint, never parsed: the tier's one spelling stays
/// `paper`/`demo`/`live`. A venue with no word of its own (polymarket) prints the tier alone.
pub(super) fn tier_with_venue_word(venue: &str, tier: vike_model::AccountTier) -> String {
    match (tier, vike_venues::venue_demo_word(venue)) {
        (vike_model::AccountTier::Demo, Some(word)) => format!("{tier} ({venue}: {word})"),
        _ => tier.to_string(),
    }
}

/// The warning `account add|set-tier --tier paper` owes on a venue whose own word for our `demo`
/// says "paper" (IBKR's "paper account"; decision 0156 item 8): our `paper` is the LOCAL simulator
/// and reaches no venue, so an operator who meant the venue's paper account typed the wrong tier.
/// A warning, never a refusal — a `paper` row there is legal and may be exactly what was meant.
pub(super) fn paper_tier_warning(venue: &str, tier: vike_model::AccountTier) -> Option<String> {
    let word = vike_venues::venue_demo_word(venue)?;
    (tier == vike_model::AccountTier::Paper && word.contains("paper")).then(|| {
        format!(
            "⚠ `paper` is vike's LOCAL simulator: no order of this account reaches {venue}. What \
             {venue} itself calls its \"{word}\" is our `demo` tier — if you meant that, use \
             `--tier demo`."
        )
    })
}

/// What `account set-exposure` without `--max-exposure` is refused with — a function for
/// [`tier_missing_message`]'s reason: the pinning test calls the string production sends.
pub(super) fn max_exposure_missing_message() -> String {
    "`account set-exposure` needs --max-exposure: a finite figure greater than 0 (this account's \
     own exposure ceiling), or `none` to clear it. The tighter of it and the box-wide \
     policy.max_account_exposure is the one the runtime applies. Nothing was written."
        .to_string()
}

/// `--max-exposure`'s token, read: `none` (any case) clears the figure, anything else must be a
/// number [`vike_secrets::MaxExposure::new`] accepts — finite and `> 0.0`, the column's own CHECK.
///
/// ⚠ **`none` is a word, never an empty token.** An empty `--max-exposure=` is refused rather than
/// read as *clear*: removing an account's ceiling makes it UNBOUNDED but for the box-wide figure,
/// and that is an act somebody types, not what a blank shell variable expands to.
///
/// It echoes the token, as [`tier_unknown_message`] does and for its reason: a figure is not a
/// secret, and it came off the command line the operator is still looking at.
pub(super) fn parse_max_exposure(raw: &str) -> Result<Option<vike_secrets::MaxExposure>, String> {
    let token = raw.trim();
    if token.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    token.parse::<f64>().ok().and_then(vike_secrets::MaxExposure::new).map(Some).ok_or_else(|| {
        format!(
            "--max-exposure '{token}' is not a figure this column accepts: it takes a finite \
             number greater than 0, or `none` to clear the ceiling. Nothing was written."
        )
    })
}

/// The sentence `add`, `activate` and `set-tier` owe the operator: what an ACTIVE row at `tier`
/// does from the next restart. `id` is the row's id, or a placeholder before an insert assigns one.
///
/// ⚠ It names the TierConflict rule as well, because `add` and `set-tier` are exactly the acts
/// that create one: two ACTIVE rows of one venue and label at two non-paper tiers trade
/// NEITHER — and for a venue-scoped tier (`vike_venues::TierScope::Venue`: ibkr, decision 0140)
/// two ACTIVE rows of the venue under ANY labels.
pub(super) fn arming_consequence(venue: &str, tier: vike_model::AccountTier, id: &str) -> String {
    if tier == vike_model::AccountTier::Paper {
        return format!(
            "an ACTIVE {venue} row at tier `paper` trades on the paper simulator only: it arms \
             nothing real."
        );
    }
    let money = if tier == vike_model::AccountTier::Live { " — REAL MONEY" } else { "" };
    // A VENUE-scoped tier (decision 0140: ibkr rides ONE session per box) conflicts over EVERY
    // label; every other venue keeps 0119's per-label rule.
    let conflict = if vike_venues::venue_tier_scope(venue) == vike_venues::TierScope::Venue {
        format!(
            "{venue} rides ONE session per box: another ACTIVE {venue} row at another tier, \
             under ANY label, holds every {venue} account at paper until one is deactivated"
        )
    } else {
        "If another ACTIVE row of this venue and label sits at a different non-paper tier, the \
         account trades NEITHER and stays paper until one is deactivated"
            .to_string()
    };
    format!(
        "⚠ this ARMS the account: from the next restart an ACTIVE {venue} row at tier `{tier}` \
         trades {tier} with its own credentials{money}. {conflict}. A RUNNING daemon notices \
         nothing until it restarts. To keep it on paper: `vike-cli secrets account deactivate \
         --id {id}`."
    )
}

/// A row's `max_exposure` as the echo and the reply print it: the figure, or `none (unbounded)`.
fn exposure_text(figure: Option<vike_secrets::MaxExposure>) -> String {
    figure.map_or_else(|| "none (unbounded)".to_string(), |f| f.to_string())
}

/// **`account ACTION` — the account table's LIFECYCLE**, through
/// `vike_secrets::edit_account_in`: the Backend-aware router, never the db function directly, so
/// this verb exercises the store choice as well as the write.
///
/// # What this verb is FOR, and what it deliberately is not
///
/// It is the ONE way an account comes into being, and the ONE place its tier is written (decision
/// 0156 item 6): a credential write never creates an account or chooses its tier — `vike-cli secrets
/// set --account ID FIELD` writes a field of a row that already exists, and refuses an id no row
/// carries, naming this verb.
///
/// ⚠ **It ARMS.** A row is born `active`, and an active row is what the runtime trades at its own
/// tier from the next restart — with that account's credentials in the store, a `live` row is
/// real money. The reply says so on every `add`, `activate` and `set-tier`, rehearsal included,
/// because the act and the consequence are a restart apart and need not be the same person's.
///
/// # The ceremony, and whose shape it is
///
/// `remove` requires `--confirm` to equal the `--id` EXACTLY. That is
/// `crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting` contract verbatim — the client's job
/// is to make the operator TYPE it, never pre-fill it, and the acceptance path's job is to refuse
/// anything else, *because the friction IS the protection*. Missing and mismatched confirms get
/// distinct messages, the same split that arm makes.
///
/// # `set-tier`, and why it has no ceremony of its own
///
/// It moves `account.tier`, the cure for a row `add`ed at the wrong one — which until it existed
/// could only be reached through `remove`, itself refused the moment the row owned a credential.
/// It takes no `--confirm` because the act is reversible by running it again. The row's credentials
/// are filed against its `id` and no credential name spells a tier (decision 0156 item 6), so the
/// keys MOVE WITH the row. What it DOES owe the operator is the consequence that lands elsewhere —
/// an active row now trades at the NEW tier, with those keys, from the next restart — and the reply
/// says so on every run, rehearsal included.
///
/// # `set-exposure`
///
/// It writes `account.max_exposure`, this one account's own exposure ceiling, or clears it with
/// `none`. The runtime applies the TIGHTER of it and the box-wide `policy.max_account_exposure`, so
/// the figure can only narrow what the box allows. It takes no `--confirm` for `set-tier`'s
/// reason: it is reversible by running it again.
///
/// # What it never does
///
/// It creates no database (`vike-cli secrets init` is the only thing that may), writes
/// and reads no `credential` row's VALUE,
/// and prints no value on any path. The one credential fact it prints is key NAMES, which
/// `vike-cli secrets list` already prints by an explicit decision.
pub(super) fn run_account(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let action = args.account_action.as_deref().expect("parse refuses `account` with no action");
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let store = vike_secrets::db_path_in(&settings);
    // The handle every read below goes through: the same directory, under the process's scope.
    let credentials = credential_store(ctx.settings_dir, ctx.settings_dir_override, ctx.scope);

    // ⚠ THE STORE IS NAMED BEFORE ANYTHING ELSE, and on the REHEARSAL as much as on the apply —
    // `run_set_book`'s rule, for its reason: a rehearsal on the wrong box (the wrong checkout, an
    // inherited `VIKE_SETTINGS_DIR`, an ssh session one hop from where the operator thinks they
    // are) reads exactly like a rehearsal on the right one.
    println!("store: {}", store.display());

    // The table, and the `Backend::Absent` refusal — asked BEFORE anything is validated, so an
    // unmigrated box is told the one thing it needs rather than being walked through a grammar
    // lesson about a table it does not have.
    let accounts = credentials.accounts().map_err(|e| {
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
                 was created. The repair is named above; rehearse it first with `--dry-run`."
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

    // `--max-exposure` is `set-exposure`'s alone. The parser admits it on every `account` action
    // (one guard, `sub != Sub::Account`), so it is refused here rather than dropped: an operator
    // who typed it beside `add` would believe the new row was born capped.
    if args.max_exposure.is_some() && action != "set-exposure" {
        return Err(CliError::usage(format!(
            "`account {action}` does not take --max-exposure: only `account set-exposure --id N \
             --max-exposure <n|none>` writes it. Nothing was written."
        )));
    }

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
            // the same late-validation rule every roster-checked flag here obeys.
            if !vike_model::VENUES.contains(&venue) {
                return Err(CliError::usage(format!(
                    "unknown venue '{venue}'. The roster is: {}",
                    vike_model::VENUES.join(", ")
                )));
            }
            let Some(word) = args.tier.as_deref() else {
                return Err(CliError::usage(tier_missing_message("add")));
            };
            let Ok(tier) = word.parse::<vike_model::AccountTier>() else {
                return Err(CliError::usage(tier_unknown_message("add", word)));
            };
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
                     venue's DEFAULT account, and there may be only one per (venue, tier) — so \
                     which of the two you meant is not a thing this command will guess at. Nothing \
                     was written."
                        .to_string(),
                ));
            }
            println!(
                "would add: venue={venue}  tier={}  label={}",
                tier_with_venue_word(venue, tier),
                label.unwrap_or("(none)")
            );
            if let Some(warning) = paper_tier_warning(venue, tier) {
                println!("{warning}");
            }
            // ⚠ THE SENTENCE THE VERB OWES, on the rehearsal too: a row is born ACTIVE, so the
            // apply below is the arming act itself.
            println!("{}", arming_consequence(venue, tier, "N"));
            if args.dry_run {
                println!(
                    "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same \
                     command without --dry-run.",
                    store.display()
                );
                return Ok(());
            }
            let (done, journal_error) = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::Create { venue, tier, label },
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            report_lifecycle_unrecorded(&done, journal_error);
            let row = done.after.as_ref().expect("a create returns its row");
            println!("\nadded account {} in {}", row.id, store.display());
            println!("{}", arming_consequence(venue, tier, &row.id.to_string()));
            println!(
                "The account holds no credentials yet — `vike-cli secrets set --account {} \
                 <FIELD>` is what puts them there, and until it does the runtime finds no keys for \
                 it and keeps it on paper.",
                row.id
            );
            Ok(())
        }
        "rename" => {
            let id = require_account_id(args, "rename")?;
            let row = require_row(rows, id)?;
            echo_row(&credentials, row);
            if args.label.is_none() && !args.no_label {
                return Err(CliError::usage(
                    "`account rename` needs --label LABEL or --no-label. ⚠ --no-label CLEARS the \
                     label, which is an act with consequences — the account's arming moves with \
                     its label, so whatever addresses the old one stops resolving to this row at \
                     the next restart — and it is not what a forgotten flag does. Nothing was \
                     written."
                        .to_string(),
                ));
            }
            println!(
                "  label: {} -> {}",
                row.label.as_deref().unwrap_or("(none)"),
                label.unwrap_or("(none)")
            );
            // ⚠ The consequences a rehearsal must state, because they land ELSEWHERE and LATER:
            // `crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` and `vike_dukascopy::resolve_account`
            // both address by the label STRING, and the account table's arming is keyed on it, so
            // renaming a row a mount names turns an armed account into a REFUSED node at the next
            // restart — and the operator who renames and the operator who restarts need not be the
            // same person.
            //
            // ⚠ **This block used to call that failure "an outage, never a misroute". It is not.**
            // Measured 2026-09-17: every catch fires on a label that STOPS resolving, and none
            // looks at one that now resolves to a DIFFERENT row. Rename B away from `ALT`, then
            // rename A to `ALT`, and the mount's `ALT` resolves again — to the other account. On
            // dukascopy the two demo accounts are different LEGAL ENTITIES, addressed by their
            // labels (decision 0156 item 6), and nothing in the store pins a label to the keys
            // filed against the row — they follow the `id`. The second warning below is the honest
            // replacement for the sentence that was there. `server.rs`'s `Rename` arm carries
            // the full sequence and names the guard 0065 §5 specifies to close it.
            if let Some(old) = row.label.as_deref() {
                println!(
                    "  ⚠ this row's arming moves WITH its label: from the next restart a mount or \
                     strategy addressing {} account `{old}` no longer finds it and is REFUSED. \
                     Update whatever addresses it too.",
                    row.venue
                );
                println!(
                    "  ⚠ and if `{old}` is later given to a DIFFERENT {} account, nothing refuses \
                     anything — whatever addresses `{old}` resolves again, to that other account. On \
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
            let (done, journal_error) = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::Rename { id, label },
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            report_lifecycle_unrecorded(&done, journal_error);
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
            echo_row(&credentials, row);
            // ⚠ The LABEL flags are refused on this action rather than ignored. `parse` admits both
            // on every `account` action (one guard, `sub != Sub::Account`), so an operator who
            // reached for `rename`'s muscle memory and typed `--label` beside `--tier` would have
            // it silently dropped — which is how somebody comes to believe a row was renamed. This
            // verb moves a tier and nothing else; renaming is its own action, and a row cannot be
            // moved and renamed in one transaction because each has its own refusal to make.
            if args.label.is_some() || args.no_label {
                return Err(CliError::usage(
                    "`account set-tier` takes neither --label nor --no-label: it moves the TIER \
                     and nothing else. Rename with `account rename --id N --label LABEL`. Nothing \
                     was written."
                        .to_string(),
                ));
            }
            let Some(word) = args.tier.as_deref() else {
                return Err(CliError::usage(tier_missing_message("set-tier")));
            };
            let Ok(tier) = word.parse::<vike_model::AccountTier>() else {
                return Err(CliError::usage(tier_unknown_message("set-tier", word)));
            };
            println!(
                "  tier: {} -> {}",
                tier_with_venue_word(&row.venue, row.tier),
                tier_with_venue_word(&row.venue, tier)
            );
            if let Some(warning) = paper_tier_warning(&row.venue, tier) {
                println!("  {warning}");
            }
            // ⚠ THE KEYS MOVE WITH THE ROW, stated at the REHEARSAL because the row's key names are
            // on the screen directly above and this is where an operator can read them: they are
            // filed against the row's `id`, and no credential name spells a tier (decision 0156
            // item 6), so a moved row trades its OWN keys at the new tier — on a venue whose
            // network IS the tier (binance's mainnet against its testnet), keys issued for the old
            // network are refused by the new one until they are replaced.
            if done_keys_block(&credentials, id) {
                println!(
                    "\n⚠ this row owns the credential keys listed above, and they MOVE WITH IT: \
                     from the next restart the account presents them at `{tier}`. Keys a venue \
                     issued for another tier's network are refused there — replace them with \
                     `vike-cli secrets set --account {id} <FIELD>`."
                );
            }
            // ⚠ The consequence that lands ELSEWHERE, and the one an operator must not learn later:
            // the row's tier IS what the mount trades it at while it is active, so moving it moves
            // the account between the paper simulator, the demo network and real money.
            if row.active {
                println!("  {}", arming_consequence(&row.venue, tier, &id.to_string()));
            } else {
                println!(
                    "  this row is INACTIVE, so it trades nothing at any tier — `account activate \
                     --id {id}` is what would arm it at `{tier}`."
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
            let (done, journal_error) = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::SetTier { id, tier },
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            report_lifecycle_unrecorded(&done, journal_error);
            if done.changed {
                println!("\nmoved account {id} to tier {tier} in {}", store.display());
            } else {
                println!("\nunchanged — that row was already at this tier. Nothing was written.");
            }
            Ok(())
        }
        "set-exposure" => {
            let id = require_account_id(args, "set-exposure")?;
            let row = require_row(rows, id)?;
            echo_row(&credentials, row);
            // ⚠ The tier and label flags are refused rather than ignored, for `set-tier`'s reason:
            // a flag typed beside this action and silently dropped is how somebody comes to
            // believe a row was moved or renamed as well.
            if args.label.is_some() || args.no_label || args.tier.is_some() {
                return Err(CliError::usage(
                    "`account set-exposure` takes none of --tier, --label or --no-label: it writes \
                     the account's exposure ceiling and nothing else. Nothing was written."
                        .to_string(),
                ));
            }
            let Some(raw) = args.max_exposure.as_deref() else {
                return Err(CliError::usage(max_exposure_missing_message()));
            };
            let max_exposure = parse_max_exposure(raw).map_err(CliError::usage)?;
            println!(
                "  max_exposure: {} -> {}",
                exposure_text(row.max_exposure),
                exposure_text(max_exposure)
            );
            // The consequence that lands ELSEWHERE: the mount applies the TIGHTER of this figure
            // and the box-wide one, and reads both once, at boot.
            println!(
                "  the runtime applies the tighter of this figure and the box-wide \
                 policy.max_account_exposure, from the next restart. A RUNNING daemon keeps the \
                 figure it booted with."
            );
            if args.dry_run {
                println!(
                    "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same \
                     command without --dry-run.",
                    store.display()
                );
                return Ok(());
            }
            let (done, journal_error) = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::SetMaxExposure { id, max_exposure },
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            report_lifecycle_unrecorded(&done, journal_error);
            if done.changed {
                println!(
                    "\nset account {id}'s max_exposure to {} in {}",
                    exposure_text(max_exposure),
                    store.display()
                );
            } else {
                println!(
                    "\nunchanged — that row already carried this max_exposure. Nothing was written."
                );
            }
            Ok(())
        }
        verb @ ("deactivate" | "activate") => {
            let id = require_account_id(args, verb)?;
            let row = require_row(rows, id)?;
            echo_row(&credentials, row);
            let active = verb == "activate";
            // ⚠ Activating IS arming: said on the rehearsal as well as the apply.
            if active {
                println!("  {}", arming_consequence(&row.venue, row.tier, &id.to_string()));
            }
            if args.dry_run {
                println!(
                    "\nwould set active={} on account {id}.\nthis was a DRY RUN — nothing was \
                     written to {}.",
                    if active { "yes" } else { "no" },
                    store.display()
                );
                return Ok(());
            }
            let (done, journal_error) = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::SetActive { id, active },
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            report_lifecycle_unrecorded(&done, journal_error);
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
            echo_row(&credentials, row);
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
                if done_keys_block(&credentials, id) {
                    println!(
                        "\n⚠ the apply would be REFUSED: this row still owns live credential keys \
                         (listed above). DEACTIVATE this account instead — deleting the keys \
                         (`secrets delete`) RETIRES their rows, and a retired row still names this \
                         account."
                    );
                } else {
                    println!(
                        "\nthe apply would DELETE account {id}, unless RETIRED credential rows \
                         (a `secrets delete` keeps them as evidence) still name it — the apply \
                         refuses then, naming them. This cannot be undone."
                    );
                }
                println!("this was a DRY RUN — nothing was written to {}.", store.display());
                return Ok(());
            }
            let (done, journal_error) = vike_secrets::edit_account_in(
                &settings,
                vike_secrets::AccountEdit::Remove { id },
                account_journal(ctx, Actor::cli("vike-cli")),
            )
            .map_err(|e| CliError::failed(e.to_string()))?;
            report_lifecycle_unrecorded(&done, journal_error);
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
fn echo_row(credentials: &vike_secrets::CredentialStore, row: &vike_secrets::Account) {
    println!(
        "account {}  venue={}  tier={}  label={}  active={}  venue_account_id={}  max_exposure={}",
        row.id,
        row.venue,
        row.tier,
        row.label.as_deref().unwrap_or("(none)"),
        if row.active { "yes" } else { "no" },
        row.venue_account_id.as_deref().unwrap_or("(not yet known)"),
        exposure_text(row.max_exposure)
    );
    match credentials.account_keys() {
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
fn done_keys_block(credentials: &vike_secrets::CredentialStore, id: i64) -> bool {
    matches!(
        credentials.account_keys(),
        Ok(Some(map)) if map.get(&id).is_some_and(|k| !k.names.is_empty())
    )
}

/// Say that an account lifecycle write landed and its change-journal record did not.
///
/// `vike_secrets::edit_account_in` records the write itself — `Change::account_lifecycle`, with the
/// actor this verb handed it (`Actor::cli`, because a human at a keyboard performed it), key NAMES
/// only, and nothing at all for an edit that changed nothing — and hands a ledger failure back as
/// data. This is that failure's line, naming the row that EXISTS after the act or, for a `remove`,
/// the one that went.
fn report_lifecycle_unrecorded(
    done: &vike_secrets::AccountWrite,
    journal_error: Option<vike_secrets::JournalAppendError>,
) {
    if let Some(row) = done.after.as_ref().or(done.before.as_ref()) {
        report_unrecorded_account(row.id, journal_error);
    }
}
