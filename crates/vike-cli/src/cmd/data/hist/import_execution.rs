//! `import`'s executor: the plan, the typed confirmation, and one request per month to a datahub; and
//! `import csv-bars --file`'s, which sends the file from this machine instead.
use std::io::IsTerminal;

use vike_hub_protocol::auth::{HubKeys, Scope};

use super::rm::confirm_phrase;
use super::{Args, ImportArgs, import};
use crate::cmd::data::shared::connect;
use crate::exit::{CliError, CommandResult};

/// The request an import sends for the window `[from_day, to_day]` in `mode` — the ONE place a
/// request's `dry_run` and `verify` are decided, from [`import::Mode`]: a plan or a verify never
/// writes, and only a verify decodes without writing.
pub(super) fn import_request(
    import_arguments: &ImportArgs,
    from_day: Option<i64>,
    to_day: Option<i64>,
    mode: import::Mode,
) -> vike_datahub_client::archive::ImportSpec {
    vike_datahub_client::archive::ImportSpec {
        format: import_arguments.format.clone(),
        dataset: import_arguments.dataset.clone(),
        from_day,
        to_day,
        bars: import_arguments.bars.clone(),
        dry_run: !mode.writes(),
        verify: mode == import::Mode::Verify,
        replace_holed: import_arguments.replace_holed,
        target: import_arguments.target.clone(),
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
/// connection idle past its read timeout (`crates/vike-hub-protocol/src/limits.rs`'s
/// `SERVER_IDLE_READ_TIMEOUT`) — so a confirmation typed after it would have been answered by a broken
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
pub(super) fn execute_import(args: &Args, keys: Option<&HubKeys>) -> CommandResult<()> {
    use vike_datahub_client::archive::ImportDatasetDirectory;

    let import_arguments =
        args.import.as_ref().expect("`parse` builds an ImportArgs for every Sub::Import");
    if let Some(file) = &import_arguments.file {
        return execute_upload(args, import_arguments, file, keys);
    }
    let mode = import::Mode::of(import_arguments.dry_run, import_arguments.verify);
    if mode == import::Mode::Import && !import_arguments.yes && !std::io::stdin().is_terminal() {
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

    // 0. `--fetch`: the datahub downloads the window's missing files first (`import::fetch`'s flow).
    if let Some(concurrency) = import_arguments.fetch
        && !fetch_first(args, import_arguments, keys, concurrency, mode, &say)?
    {
        return Ok(());
    }

    // 1. THE PREVIEW — one plan-only request over the whole window, on its own connection.
    let preview = connect(&args.addr, keys, Scope::Write)?
        .import_archive(&import_request(
            import_arguments,
            import_arguments.from_day,
            import_arguments.to_day,
            import::Mode::Plan,
        ))
        .map_err(|e| CliError::failed(import::run::refusal(e, &args.addr)))?
        .plan;
    for line in import::plan::plan_lines(&preview, &args.addr) {
        say(&line);
    }
    if preview.dir != ImportDatasetDirectory::Present {
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
    if mode == import::Mode::Import && !import_arguments.yes {
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
                .import_archive(&import_request(import_arguments, Some(from), Some(to), mode))
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

/// `import … --fetch`'s download phase ([`import::fetch`]'s flow): the plan and its estimated cost,
/// then, unless the run is a dry run, the typed confirmation and one request per calendar month that
/// holds a missing file. `Ok(true)` goes on to the import; `Ok(false)` ends the run having downloaded
/// nothing (a dry run with files missing). A month that fails, or a file that could not be downloaded,
/// fails the run before the import, naming what to re-run.
fn fetch_first(
    args: &Args,
    import_arguments: &ImportArgs,
    keys: Option<&HubKeys>,
    concurrency: u32,
    mode: import::Mode,
    say: &dyn Fn(&str),
) -> CommandResult<bool> {
    use vike_datahub_client::archive_fetch::{FetchOutcome, FetchSpec};
    // `import::fetch::parse` refuses --fetch without both bounds.
    let (Some(from), Some(to)) = (import_arguments.from_day, import_arguments.to_day) else {
        return Err(CliError::usage("--fetch needs --from and --to"));
    };
    let spec = |from_day, to_day, go| FetchSpec {
        format: import_arguments.format.clone(),
        dataset: import_arguments.dataset.clone(),
        from_day,
        to_day,
        go,
        concurrency,
    };
    let failed = |e: String| CliError::failed(format!("datahub at {}: {e}", args.addr));
    let plan = connect(&args.addr, keys, Scope::Write)?
        .fetch_archive(&spec(from, to, false))
        .map_err(failed)?
        .plan;
    for line in import::fetch::plan_lines(&plan, &args.addr) {
        say(&line);
    }
    if plan.missing.is_empty() {
        say("nothing to download: every daily file of the window is in the folder");
        return Ok(true);
    }
    if mode != import::Mode::Import {
        say("--dry-run: nothing was downloaded. Run it without --dry-run to download, then import");
        return Ok(false);
    }
    if !import_arguments.yes {
        confirm_phrase(&import::fetch::confirmation(&plan), "nothing was downloaded")?;
    }
    // A fresh connection, for the preview's reason in `execute_import`'s doc.
    let mut client = connect(&args.addr, keys, Scope::Write)?;
    let months = import::fetch::months_to_fetch(&plan, &import::plan::months(from, to));
    let mut total = FetchOutcome::default();
    let mut month_lists = 0;
    for (index, &(a, b)) in months.iter().enumerate() {
        let done = client.fetch_archive(&spec(a, b, true)).map_err(|e| {
            failed(format!(
                "{e}\n  the download stopped at month {} of {}; every file written before it stays, \
                 and the same command resumes",
                index + 1,
                months.len()
            ))
        })?;
        month_lists += done.plan.list_requests;
        let got = done.fetched.unwrap_or_default();
        eprintln!("{}", import::fetch::progress_line(index + 1, months.len(), a, &got));
        total.fetched += got.fetched;
        total.bytes += got.bytes;
        total.get_requests += got.get_requests;
        total.failed.extend(got.failed);
    }
    say(&import::fetch::summary_line(plan.list_requests, &total, month_lists));
    if total.failed.is_empty() {
        Ok(true)
    } else {
        Err(CliError::failed(import::fetch::failure(&total)))
    }
}

/// `data hist import csv-bars --file PATH` — SEND a user's file from this machine into a NEW `user-*`
/// series of the datahub's store (decision 0272): read it here, send it in chunks with a progress line
/// on stderr each, and print how the datahub read it and what it stored. `--dry-run` sends the whole
/// file and stores nothing. No confirmation: an upload can neither add to nor replace a series.
///
/// [`Scope::Read`]: `Request::CsvUpload` is an OBSERVE-scope verb, so the observe key a PC holds is
/// enough (`crates/vike-datahub-client/src/proto/verbs.rs`'s `required_scope` carries why).
///
/// The exit: `0` when the datahub read the file (and, unless a dry run, stored it); `1` when it refused
/// the upload or the file, naming why; a usage error when the file cannot be read here or is not UTF-8.
fn execute_upload(
    args: &Args,
    import_arguments: &ImportArgs,
    file: &std::path::Path,
    keys: Option<&HubKeys>,
) -> CommandResult<()> {
    let target =
        import_arguments.target.as_ref().expect("`parse_target` builds a target for csv-bars");
    let shown = file.display().to_string();
    let bytes = std::fs::read(file)
        .map_err(|e| CliError::usage(format!("--file {shown}: cannot read it here: {e}")))?;
    let text = vike_datahub_client::upload::file_text(&bytes)
        .map_err(|e| CliError::usage(format!("--file {shown}: {e}")))?;
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let done = client
        .upload_csv(target, text, import_arguments.dry_run, false, &mut |sent, total| {
            eprintln!("{}", import::upload::progress_line(sent, total));
        })
        .map_err(|why| CliError::failed(format!("datahub at {}: {why}", args.addr)))?;
    for line in import::upload::upload_lines(
        &done,
        target,
        &shown,
        text.len() as u64,
        &args.addr,
        import_arguments.dry_run,
    ) {
        if args.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    if args.json {
        let doc = serde_json::json!({
            "addr": args.addr,
            "file": shown,
            "dry_run": import_arguments.dry_run,
            "lane": format!("user-{}", target.label),
            "upload": done,
        });
        println!("{doc}");
    }
    match &done.refused {
        None => Ok(()),
        Some(why) => Err(CliError::failed(format!(
            "the datahub refused {shown}, and nothing was stored: {why}"
        ))),
    }
}

// ─── `rm`: the one verb that reaches EITHER store ───────────────────────────────────────────────
