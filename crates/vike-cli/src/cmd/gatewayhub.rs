//! `vike-cli gatewayhub setup` — MINT the gatewayhub's hub keys: the observe/control pair and the order-scope
//! (`Trade`) key.
//!
//! ```text
//! vike-cli gatewayhub setup [--rotate]
//! ```
//!
//! # Why it exists, and why it is a verb of its own
//!
//! `vike-gatewayhub` is the daemon that owns every venue connection
//! (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`, verdict 6: *"the
//! gatewayhub adds its own hub keys"*). Its clients authenticate with the HMAC handshake the
//! tradehub and the datahub use — an observe key and a control key — and the datahub, its one
//! price consumer, is the client that holds the same two names. Nothing else in this tree can
//! generate that pair: `backend setup` mints the TRADEHUB's, `datahub setup` the datahub's, and
//! `secrets set` refuses every node name by design, because a pair a human invents is exactly what
//! [`crate::cmd::backend`]'s module doc argues against. So this verb does the one thing nothing else can.
//!
//! ⚠ **It is not `datahub setup` with another name, and not a flag on it.** `datahub setup` writes
//! the pair the DATAHUB serves its own clients with (`[2]`/`[3]` of
//! `vike_venues::credential_keys::PLATFORM_KEYS`); this writes the keys the GATEWAYHUB verifies
//! (`gatewayhub_key_pair` and `gatewayhub_trade_key`). No key is shared across services, so a verb that took the service as a parameter
//! would make one command able to rotate any service's pair, which is the blast radius the
//! three-way split of that table exists to contain. A separate verb costs a word and keeps each
//! command's help true, the argument [`crate::cmd::datahub`] makes against folding it into `backend`.
//!
//! # What it deliberately does NOT do
//!
//! **It writes no settings** — not the gatewayhub's bind address, not a flag. It MINTS the pair and the `Trade` key, and
//! the daemon's own rows are `vike-cli config set`'s. And it mints no ADMIN key: `VIKE_GATEWAYHUB_ADMIN_KEY` is
//! named in the table and minted by nothing, because the gatewayhub's `Scope::Account` stays refused.
//!
//! # Everything `datahub setup`'s doc says about key handling applies here unchanged
//!
//! No argv form, no stdin form, no `--from-env` — there is no operator-supplied value at all. The
//! keys are CSPRNG bytes handed straight to `vike_secrets::save_credentials_to_store` in-process,
//! in ONE call so the three are never half-rotated. Nothing is printed but each key's `key_id`
//! (`vike_hub_proto::auth`'s `key_fingerprint`, an HMAC tag under its own domain separator). An
//! existing key is a key a client is already signing with, so replacing one needs `--rotate`. The
//! store is the project's hub-key store — the settings database's `hub_key` table
//! (`crate::cmd::backend::landed`) — and an ABSENT one is REFUSED rather than created, which is
//! `docs/decisions/0036`'s fence. The writer journals the write itself: key NAMES only.
//!
//! # The datahub's half
//!
//! On a box where the gatewayhub and the datahub share a project, this mint is the whole job: the
//! datahub holds the pair as a CLIENT. The TRADEHUB holds the `Trade` key as a client (and only that
//! name: its root reads the gatewayhub's order-scope key by name, never the family). A datahub or a
//! tradehub on another project takes the SAME keys with `vike-cli secrets copy-hub-keys
//! --from-settings-dir <DIR> --only gatewayhub`, never by minting a second set.

use std::process::ExitCode;

use crate::cmd::args::{self, Stop};
use crate::cmd::backend::{
    Ctx, hub_key_journal, key_id, landed, mint_key, open_store, report_unrecorded_hub_keys,
    settings_dir,
};
use crate::exit::{CliError, CmdResult};

/// The program name this verb is read under.
const BIN: &str = "vike-cli gatewayhub";

/// The `gatewayhub` command line: a sub-verb is required.
#[derive(clap::Parser, Debug)]
#[command(
    about = "MINT the hub keys a vike-gatewayhub server authenticates with (setup): the observe and \
             control pair and the trade key, on that server's box",
    after_help = "Only those three keys are written: this verb sets no address and no flag, and it \
                  takes no key — the keys are generated here and never printed. A datahub or a \
                  tradehub on another project takes the SAME keys with `vike-cli secrets \
                  copy-hub-keys --from-settings-dir DIR --only gatewayhub`."
)]
struct Cli {
    #[command(subcommand)]
    sub: Args,
}

/// The one subcommand.
#[derive(clap::Subcommand, Debug)]
enum Args {
    /// MINT the three hub keys a vike-gatewayhub server authenticates with (observe, control and
    /// trade), into the settings
    /// database (`vike-cli secrets init` creates it). Run it on the GATEWAYHUB's box. It never
    /// accepts a key and never prints one: what it prints is each key's ID, which is what you
    /// compare against the server's own log line.
    Setup {
        /// ⚠ Not an inline value: `--rotate=maybe` is refused, because a command line whose author
        /// believed it meant something is how a person ends up thinking they did not rotate when
        /// they did.
        #[arg(
            long,
            help = "replace keys already in the store. Every client still holding the old ones \
                    stops working the moment the server restarts"
        )]
        rotate: bool,
    },
}

fn parse(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    args::parse::<Cli, _, _>(BIN, argv).map(|cli| cli.sub)
}

pub fn run(argv: impl Iterator<Item = String>, ctx: &Ctx<'_>) -> ExitCode {
    let parsed = match parse(argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    // One subcommand, so the destructuring cannot fail; a second verb turns this into a `match`.
    let Args::Setup { rotate } = parsed;
    match run_setup(rotate, ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli gatewayhub: {}", e.msg);
            e.exit.into()
        }
    }
}

/// The gatewayhub's three key names to MINT (observe, control, trade), from the one table that
/// classifies them.
///
/// ⚠ Read through `vike_venues::credential_keys::gatewayhub_key_pair` rather than spelled here,
/// and validated against `platform_key_service` — a hand copy of these names is exactly what
/// `vike_tradehub_client::auth`'s doc warns re-opens the gap that table exists to close. The
/// names have no second spelling in a server crate, so the classifier check IS the guard: a
/// reorder of the table would otherwise make this verb mint under another service's names, writing
/// a pair no gatewayhub reads and replacing one a different service is using. The ADMIN key
/// (`PLATFORM_KEYS[8]`) is not among them: nothing mints it.
fn gatewayhub_key_names() -> CmdResult<[&'static str; 3]> {
    let [observe, control] = vike_venues::credential_keys::gatewayhub_key_pair();
    let names = [observe, control, vike_venues::credential_keys::gatewayhub_trade_key()];
    for n in names {
        if vike_venues::credential_keys::platform_key_service(n)
            != Some(vike_venues::credential_keys::GATEWAYHUB_SERVICE)
        {
            return Err(CliError::failed(format!(
                "{n} is not classified as a vike-gatewayhub hub key — PLATFORM_KEYS was reordered \
                 without this verb being updated, and minting under the wrong name would write a \
                 key no server reads"
            )));
        }
    }
    Ok(names)
}

fn run_setup(rotate: bool, ctx: &Ctx<'_>) -> CmdResult<()> {
    let names = gatewayhub_key_names()?;
    // ⚠ The store that ANSWERS, not the file — `datahub setup`'s argument unchanged: the overwrite
    // refusal below and the write must both be asked of the same store, which
    // `vike_secrets::backend_in` decides once for both.
    let dir = settings_dir(ctx);
    let path = landed(ctx, &vike_secrets::backend_in(&dir));
    let existing = open_store(&dir, ctx.scope)?;

    // A key already in the store is a key some client is already signing with. Replacing one is a
    // decision with a blast radius, not an idempotent re-run — the same refusal `datahub setup`
    // makes.
    let present: Vec<&str> = names
        .into_iter()
        .filter(|n| existing.get(*n).is_some_and(|v| !v.trim().is_empty()))
        .collect();
    if !present.is_empty() && !rotate {
        return Err(CliError::failed(format!(
            "{} already in {} — refusing to replace {} without --rotate. Every client holding the \
             old key stops working the moment the server restarts, so this is a decision rather \
             than a re-run.",
            present.join(" and "),
            path.display(),
            if present.len() == 1 { "it" } else { "them" }
        )));
    }

    // One call, all three keys, so they can never be half-rotated by a failure between two writes.
    let observe = mint_key();
    let control = mint_key();
    let trade = mint_key();
    let ids = [key_id(&observe), key_id(&control), key_id(&trade)];
    let updates = [
        (names[0].to_string(), observe),
        (names[1].to_string(), control),
        (names[2].to_string(), trade),
    ];
    // The writer records the write in the same call: key NAMES only — `Change::credential_write`
    // takes no value parameter at all, so nothing here CAN put a credential in the ledger.
    let (_landed, journal_error) = vike_secrets::save_credentials_to_store(
        &dir,
        vike_secrets::Table::HubKey,
        &updates,
        // `Add`: a minted key may be a name the `hub_key` table does not hold yet, and a hub key
        // has no account and no classification (decision 0051): `(name, value)` in every schema.
        vike_secrets::UpsertIntent::Add,
        hub_key_journal(ctx.now_ms),
    )
    .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;
    report_unrecorded_hub_keys(journal_error);

    println!("minted into {}:", path.display());
    println!("  {}  id {}", names[0], ids[0]);
    println!("  {}  id {}", names[1], ids[1]);
    println!("  {}  id {}", names[2], ids[2]);
    println!();
    println!("restart the server so it loads them, then the clients that dial it:");
    println!("  sudo systemctl restart vike-gatewayhub");
    println!("  sudo systemctl restart vike-datahub");
    println!("  sudo systemctl restart vike-tradehub");
    println!();
    println!(
        "a client authenticates by holding the same keys (the datahub the pair, the tradehub the"
    );
    println!(
        "trade key); `vike-cli secrets path` prints where this box keeps them. A datahub or a"
    );
    println!("tradehub on another project takes them with");
    println!("`vike-cli secrets copy-hub-keys --from-settings-dir <DIR> --only gatewayhub`.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    fn argv(a: &[&str]) -> impl Iterator<Item = String> + use<> {
        a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn a_subcommand_is_required_and_an_unknown_one_is_a_clean_error() {
        assert!(parse(argv(&[])).is_err());
        assert!(parse(argv(&["connect"])).is_err(), "only `setup` exists here today");
        // `control-key` is the DATAHUB's verb: it hands out a key a launcher needs, and this
        // service has no launcher that reads one.
        assert!(parse(argv(&["control-key"])).is_err());
    }

    #[test]
    fn rotate_is_off_unless_asked() {
        assert_matches!(parse(argv(&["setup"])), Ok(Args::Setup { rotate: false }));
        assert_matches!(parse(argv(&["setup", "--rotate"])), Ok(Args::Setup { rotate: true }));
        assert!(parse(argv(&["setup", "--rotate=maybe"])).is_err(), "an inline value is refused");
    }

    /// ⚠ The names come from the shared table, and this is the assertion that the INDEXING is right.
    ///
    /// `PLATFORM_KEYS` is ordered — tradehub first, datahub next, gatewayhub appended — and
    /// `gatewayhub_key_pair` is a claim about that order. A reorder would silently make this verb mint under
    /// another service's names; the classifier check inside `gatewayhub_key_names` is what turns
    /// that into a refusal, and this proves the check passes on the real table.
    #[test]
    fn the_names_are_the_gatewayhubs_and_are_classified_as_such() {
        let names = gatewayhub_key_names().expect("the table classifies both");
        assert!(names[0].contains("GATEWAYHUB") && names[0].contains("OBSERVE"), "{names:?}");
        assert!(names[1].contains("GATEWAYHUB") && names[1].contains("CONTROL"), "{names:?}");
        assert!(names[2].contains("GATEWAYHUB") && names[2].contains("TRADE"), "{names:?}");
        assert_ne!(names[0], names[1]);
        assert_ne!(names[0], names[2]);
        assert_ne!(names[1], names[2]);
        for n in names {
            assert_eq!(
                vike_venues::credential_keys::platform_key_service(n),
                Some(vike_venues::credential_keys::GATEWAYHUB_SERVICE)
            );
            assert!(vike_venues::credential_keys::is_gatewayhub_daemon_key(n));
        }
        // The pair is the datahub's to hold as a client; the trade key is the tradehub's, and
        // the admin key is minted by nothing.
        assert!(vike_venues::credential_keys::is_datahub_gatewayhub_client_key(names[0]));
        assert!(vike_venues::credential_keys::is_datahub_gatewayhub_client_key(names[1]));
        assert!(!vike_venues::credential_keys::is_datahub_gatewayhub_client_key(names[2]));
        assert!(vike_venues::credential_keys::is_tradehub_gatewayhub_trade_client_key(names[2]));
        assert!(!names.contains(&vike_venues::credential_keys::PLATFORM_KEYS[8]), "no admin mint");
    }

    /// The usage may not promise what the verb does not do: it takes no address and no flag
    /// beyond `--rotate`, and says so.
    #[test]
    fn the_usage_says_it_writes_no_settings() {
        // Whitespace-normalised: the help renderer wraps at the terminal width, and a phrase that
        // straddles a wrap is still the phrase.
        let usage = args::help_tree(&args::command::<Cli>(BIN))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(usage.contains("--rotate"), "{usage}");
        assert!(usage.contains("sets no address and no flag"), "{usage}");
        assert!(usage.contains("--only gatewayhub"), "it must name how the datahub gets the pair");
        assert!(!usage.contains("--addr"), "this verb takes no address flag");
        assert!(!usage.contains("--control"), "there is no control flag on this service");
    }
}
