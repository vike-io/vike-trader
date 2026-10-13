//! `import`'s executor: the plan, the typed confirmation, and one request per month to a datahub.
use std::io::IsTerminal;

use vike_node_proto::auth::{NodeKeys, Scope};

use super::rm::confirm_phrase;
use super::{Args, ImportArgs, import};
use crate::cmd::data::shared::connect;
use crate::exit::{CliError, CmdResult};

/// The request an import sends for the window `[from_day, to_day]` in `mode` — the ONE place a
/// request's `dry_run` and `verify` are decided, from [`import::Mode`]: a plan or a verify never
/// writes, and only a verify decodes without writing.
pub(super) fn import_request(
    im: &ImportArgs,
    from_day: Option<i64>,
    to_day: Option<i64>,
    mode: import::Mode,
) -> vike_datahub_client::archive::ImportSpec {
    vike_datahub_client::archive::ImportSpec {
        format: im.format.clone(),
        dataset: im.dataset.clone(),
        from_day,
        to_day,
        bars: im.bars.clone(),
        dry_run: !mode.writes(),
        verify: mode == import::Mode::Verify,
    }
}

/// `data hist import FORMAT DATASET` — the plan, the confirmation, one request per calendar month,
/// and a summary. [`import`]'s module doc carries the flow, the two-plans rule and the exit rung;
/// this is the dial, the requests and the prints.
///
/// # ⚠ The no-terminal refusal happens HERE, before the socket — `rm`'s rule
///
/// An import with no `--yes` and no terminal on stdin is REFUSED on the usage rung before anything
/// is dialled: a confirmation read from a pipe is not a confirmation, and refusing before the round
/// trip is strictly better than after it (`execute_rm` argues the ordering).
///
/// # ⚠ The month requests ride a FRESH connection
///
/// The preview's connection is dropped once the plan is in, and a new one is opened for the months.
/// Between the two a human may be reading a long plan at the prompt, and the datahub closes a
/// connection idle past its read timeout (`crates/vike-datahub/src/server/limits.rs`'s
/// `IDLE_READ_TIMEOUT`) — so a confirmation typed after it would have been answered by a broken
/// pipe on the first month. One extra handshake is the whole cost.
///
/// # Where the lines go
///
/// The plan and the summary are the ANSWER: stdout, or stderr under `--json`, where stdout is the
/// one document ([`import::run::document`]) and nothing else — `fetch`'s and `rm`'s split. The progress
/// lines are not the answer and go to stderr always, as a per-year `fetch`'s do.
///
/// [`Scope::Write`] for every request, the plan included: `Request::ImportArchive` is Control
/// whatever its `dry_run` (`crates/vike-datahub-client/src/proto/verbs.rs`'s `required_scope`).
pub(super) fn execute_import(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    use vike_datahub_client::archive::DatasetDir;

    let im = args.import.as_ref().expect("`parse` builds an ImportArgs for every Sub::Import");
    let mode = import::Mode::of(im.dry_run, im.verify);
    if mode == import::Mode::Import && !im.yes && !std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "refusing to import without --yes: stdin is not a terminal, so there is nobody to type \
             the confirmation. Pass --yes (a deliberate, greppable token in shell history), or \
             --dry-run to see the plan and stop.",
        ));
    }
    let say = |line: &str| {
        if args.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };

    // 1. THE PREVIEW — one plan-only request over the whole window, on its own connection.
    let preview = connect(&args.addr, keys, Scope::Write)?
        .import_archive(&import_request(im, im.from_day, im.to_day, import::Mode::Plan))
        .map_err(|e| CliError::failed(import::run::refusal(e, &args.addr)))?
        .plan;
    for line in import::plan::plan_lines(&preview, &args.addr) {
        say(&line);
    }
    if preview.dir != DatasetDir::Present {
        for line in import::plan::dir_lines(&preview, &args.addr) {
            say(&line);
        }
        if args.json {
            println!("{}", import::run::document(&args.addr, mode, &preview, &[]));
        }
        return Err(CliError::failed(import::plan::dir_failure(&preview)));
    }

    // 2. `--dry-run` stops at the plan; so does a window with nothing to act on.
    let months = import::plan::months_of(&preview);
    let todo = import::plan::actionable(&preview, mode);
    if mode == import::Mode::Plan || todo == 0 || months.is_empty() {
        let refused = import::ClassCounts::of(&preview).refused.len();
        if mode == import::Mode::Plan {
            say(
                "--dry-run: nothing was decoded and nothing was written. Run it without --dry-run \
                 to import, or with --dry-run --verify to decode every importable file first",
            );
        } else {
            let verb = if mode == import::Mode::Verify { "verify" } else { "import" };
            say(&format!(
                "nothing to {verb}: no day in the window is {} — nothing was sent after the plan",
                if mode == import::Mode::Verify { "importable" } else { "importable or held" }
            ));
        }
        if args.json {
            println!("{}", import::run::document(&args.addr, mode, &preview, &[]));
        }
        return if refused == 0 {
            Ok(())
        } else {
            Err(CliError::failed(import::run::refused_failure(refused, import::Mode::Plan)))
        };
    }

    // 3. The confirmation — an import only, bound to the plan's own count.
    if mode == import::Mode::Import && !im.yes {
        confirm_phrase(&import::plan::confirmation(&preview), "nothing was imported")?;
    }

    // 4. ONE REQUEST PER CALENDAR MONTH, on a fresh connection (see the doc above).
    let mut client = connect(&args.addr, keys, Scope::Write)?;
    let header = import::run::header_line(&preview, &months, mode);
    let done = import::run::run_months(
        &months,
        &preview,
        mode,
        &header,
        |from, to| {
            client
                .import_archive(&import_request(im, Some(from), Some(to), mode))
                .map_err(|e| import::run::refusal(e, &args.addr))
        },
        |line| eprintln!("{line}"),
    )
    .map_err(|failed| CliError::failed(failed.message(mode)))?;

    // 5. The summary — and the document, which carries what the lines do.
    let tally = import::Tally::of(&done);
    for line in import::run::summary_lines(&preview, &tally, mode) {
        say(&line);
    }
    if args.json {
        println!("{}", import::run::document(&args.addr, mode, &preview, &done));
    }
    if tally.refused.is_empty() {
        Ok(())
    } else {
        Err(CliError::failed(import::run::refused_failure(tally.refused.len(), mode)))
    }
}

// ─── `rm`: the one verb that reaches EITHER store ───────────────────────────────────────────────
