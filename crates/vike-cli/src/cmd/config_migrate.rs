//! `vike-cli config migrate-store` — apply the settings store's pending migrations and say what
//! they did: decision 0095's `live`-means-mainnet ceiling rewrite, then the venue links' move onto
//! `venue_id`.
//!
//! It is the command a daemon's boot refusal names when the daemon cannot write `settings/db`
//! (`vike_boot::Ceilings::Interpret`). This process's own boot will usually have applied decision
//! 0095 already — `vike-cli` boots with `Ceilings::InterpretOrMark` — in which case its rewrite
//! lines were printed to stderr at startup and this verb reports that migration as not pending.
//! Journalled with `Actor::cli("vike-cli")`.
//!
//! ⚠ **It is also the operator's one DELIBERATE door onto the venue links' move**
//! (`vike_secrets::venue_links::apply_venue_links`). That move rides the write funnel, so without
//! this verb it happens at whichever write a store meets first — days later, perhaps, and at a
//! moment nobody chose — and on a box whose daemon cannot write `settings/` only an operator's
//! process can make it at all. This verb carries the store on purpose and says so, says when there
//! was nothing to carry without opening the store for writing, and is refused by a store only a
//! human can repair exactly as every writer is. (⚠ Until the venue-links plan's final fix wave it
//! answered "the settings store is current" whenever decision 0095 had nothing to do, while the
//! store's linked tables could still be waiting for that move.) Since the plan's second release it
//! also CONTRACTS a store — drops the text `venue` column the first release left beside the number
//! — on purpose, through the same step, and a store the drop refuses is refused here (exit 1) rather
//! than reported finished; it said "already carried" of a first-release store until that release's
//! first fix round.

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::live_means_mainnet::{LiveMeansMainnet, REASON};
use vike_secrets::venue_links::VenueLinks;

use crate::exit::{CliError, CmdResult, Exit};

const USAGE: &str = "usage: vike-cli config migrate-store\n\n  Applies the settings store's pending \
     migrations and says what each did: decision 0095 (`live` ceilings of \
     binance/bybit/okx/hyperliquid become `demo`, journalled), then the venue links' move onto \
     `venue_id` (the store's account, venue-setting and arming tables read their venue by number) \
     and the drop of the text `venue` column that move leaves behind in the account, credential \
     and venue-setting tables. A store with nothing pending is not opened for writing. Run it as \
     the user that owns <project>/settings/db.";

/// What this verb needs from the dispatcher's one boot.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub settings_dir: Option<&'a Path>,
    pub now_ms: i64,
}

/// Entry point. `args` is everything after `config migrate-store`.
pub fn run(mut args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    if let Some(a) = args.next() {
        if matches!(a.as_str(), "-h" | "--help") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        eprintln!("vike-cli config migrate-store: unexpected argument `{a}`\n{USAGE}");
        return Exit::Usage.into();
    }
    match execute(&ctx, &mut |line: String| println!("{line}")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli config migrate-store: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Run both steps, handing each report line to `out` as soon as its step has answered.
fn execute(ctx: &Ctx<'_>, out: &mut dyn FnMut(String)) -> CmdResult<()> {
    let Some(dir) = ctx.settings_dir else {
        return Err(CliError::failed(
            "no settings directory resolved — `cd` into the project, or name it with \
             $VIKE_SETTINGS_DIR"
                .to_string(),
        ));
    };
    let ceilings = vike_secrets::live_means_mainnet::apply_live_means_mainnet(
        dir,
        vike_model::change_journal::Actor::cli("vike-cli"),
        vike_model::change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
        ctx.now_ms,
    )
    .map_err(|e| refusal(dir, &e))?;
    // ⚠ 0095's lines go out BEFORE the second step, and that is not tidiness: an `Applied` rewrite
    // is already committed and journalled, so a venue-links step that then fails (a busy store, a
    // repair refusal) must not take the report of a change the store now holds down with it. Until
    // review the whole report was printed after both steps, and that failure printed none of it.
    for line in ceiling_lines(&ceilings) {
        out(line);
    }
    // AFTER decision 0095, in every outcome of it. When 0095 was pending and applied, the funnel
    // already ran in that transaction — carry and contraction both — and this probe reads "already
    // carried and contracted".
    let links = vike_secrets::venue_links::apply_venue_links(dir).map_err(|e| refusal(dir, &e))?;
    for line in link_lines(matches!(ceilings, LiveMeansMainnet::NoDatabase), &links) {
        out(line);
    }
    Ok(())
}

/// **One arm per kind of failure** — the shape `vike_boot`'s `ceiling_migration_refusal` gives the
/// same two.
///
/// A refusal of the store's REPAIR (`vike_secrets::DbErrorKind::RepairRefused`: a row naming a
/// venue the roster lacks, a key two rows would share, a dangling reference) is not an ownership
/// matter, and running this verb as another user changes nothing: every writer meets the same
/// refusal, this one included, and the refusal itself names the rows and the SQLite-client repair.
/// (⚠ Until the venue-links plan's final fix wave it carried the ownership hint below as well.)
/// Every other failure is the store refusing this process — above all one that cannot write
/// `settings/db` — and the hint is what to do about it.
fn refusal(dir: &Path, e: &vike_secrets::DbError) -> CliError {
    match &e.kind {
        vike_secrets::DbErrorKind::RepairRefused(_) => CliError::failed(format!(
            "{e}\n\nNo `vike-cli` verb can make this repair, this one included: every one of them \
             runs the same step and is refused the same way. Repair the rows the refusal names, \
             then run `vike-cli config migrate-store` again."
        )),
        _ => CliError::failed(format!(
            "the store could not be migrated: {e} — run this as the user that owns {}",
            vike_secrets::db_path_in(dir).display()
        )),
    }
}

/// Decision 0095's lines. PURE, so the wording is unit-tested.
///
/// EMPTY for `NoDatabase`: that answer waits for the venue links' own ([`link_lines`]), so that one
/// line can speak for both steps when neither found a database.
fn ceiling_lines(ceilings: &LiveMeansMainnet) -> Vec<String> {
    match ceilings {
        LiveMeansMainnet::NoDatabase => Vec::new(),
        LiveMeansMainnet::NotPending => {
            vec!["decision 0095's ceiling migration: not pending".to_string()]
        }
        LiveMeansMainnet::Applied { rewrites, journal_error } => {
            let mut out = vec![format!(
                "decision 0095 applied: {} ceiling row(s) rewritten from `live` to `demo`",
                rewrites.len()
            )];
            out.extend(rewrites.iter().map(|r| format!("  {}: live -> demo", r.key())));
            out.push(format!("  why: {REASON}"));
            if let Some(e) = journal_error {
                out.push(format!("  ⚠ the change journal could not record it: {e}"));
            }
            out
        }
    }
}

/// The venue links' line, and 0095's deferred `NoDatabase` line with it — `ceilings_found_none` is
/// whether decision 0095's step found no database. PURE, so the wording is unit-tested.
fn link_lines(ceilings_found_none: bool, links: &VenueLinks) -> Vec<String> {
    let mut out = Vec::new();
    if ceilings_found_none {
        // …and the venue links' answer is then `NoDatabase` too: one line says it for both.
        if *links == VenueLinks::NoDatabase {
            return vec!["no settings database on this box — nothing to migrate".to_string()];
        }
        out.push("decision 0095's ceiling migration: no settings database".to_string());
    }
    match links {
        VenueLinks::NoDatabase => out.push("venue links: no settings database".to_string()),
        VenueLinks::OlderSchema { found } => out.push(format!(
            "venue links: nothing to carry — this store is at schema {found}, which predates the \
             account table, and nothing was written; `vike-cli secrets migrate` brings it to the \
             current schema"
        )),
        VenueLinks::AlreadyCarried => {
            out.push("venue links: already carried and contracted".to_string());
        }
        // One line per step the transaction took, both when both: a store still before the plan's
        // first release is carried AND contracted by one run, a store that release carried is only
        // contracted.
        VenueLinks::Carried { onto_venue_id, text_dropped_from } => {
            if !onto_venue_id.is_empty() {
                out.push("venue links: carried onto venue_id".to_string());
            }
            if !text_dropped_from.is_empty() {
                out.push(format!(
                    "venue links: text venue column dropped from {}",
                    text_dropped_from.join(", ")
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::live_means_mainnet::CeilingRewrite;

    /// The whole report, in the order `execute` hands it out.
    fn report_lines(ceilings: &LiveMeansMainnet, links: &VenueLinks) -> Vec<String> {
        let mut lines = ceiling_lines(ceilings);
        lines.extend(link_lines(matches!(ceilings, LiveMeansMainnet::NoDatabase), links));
        lines
    }

    #[test]
    fn an_applied_migration_names_every_row_and_the_reason() {
        let lines = report_lines(
            &LiveMeansMainnet::Applied {
                rewrites: vec![CeilingRewrite { venue: "hyperliquid".to_string(), label: None }],
                journal_error: None,
            },
            &VenueLinks::AlreadyCarried,
        );
        let text = lines.join("\n");
        assert!(text.contains("policy.venues.hyperliquid: live -> demo"), "{text}");
        assert!(text.contains("0095"), "{text}");
    }

    /// A store decision 0095 has nothing left to do on is NOT called current: that is all this
    /// line knows, and the venue links' own line says what they found.
    #[test]
    fn a_store_with_nothing_pending_for_0095_says_only_that() {
        let carried =
            VenueLinks::Carried { onto_venue_id: vec!["account"], text_dropped_from: Vec::new() };
        let text = report_lines(&LiveMeansMainnet::NotPending, &carried).join("\n");
        assert!(text.contains("decision 0095's ceiling migration: not pending"), "{text}");
        assert!(!text.contains("current"), "{text}");
    }

    /// The venue links' lines say which step the run took: the carry, the contraction, or both —
    /// and a store with nothing owed says it is carried AND contracted, never only one.
    #[test]
    fn the_link_lines_name_each_step_the_run_took() {
        let lines = |links: &VenueLinks| link_lines(false, links);
        let contracted = VenueLinks::Carried {
            onto_venue_id: Vec::new(),
            text_dropped_from: vec!["account", "credential", "venue_setting"],
        };
        assert_eq!(
            lines(&contracted),
            ["venue links: text venue column dropped from account, credential, venue_setting"]
        );
        let both = VenueLinks::Carried {
            onto_venue_id: vec!["account", "venue_setting", "venue_arming"],
            text_dropped_from: vec!["account", "credential", "venue_setting"],
        };
        assert_eq!(
            lines(&both),
            [
                "venue links: carried onto venue_id",
                "venue links: text venue column dropped from account, credential, venue_setting"
            ]
        );
        assert_eq!(
            lines(&VenueLinks::AlreadyCarried),
            ["venue links: already carried and contracted"]
        );
    }
}
