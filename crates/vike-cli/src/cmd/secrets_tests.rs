use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn each_subcommand_parses() {
    assert_eq!(parse_of(&["list"]).unwrap().sub, Sub::List);
    assert_eq!(parse_of(&["path"]).unwrap().sub, Sub::Path);
}

#[test]
fn options_parse_in_both_flag_forms() {
    assert_eq!(
        parse_of(&["list", "--file", "/tmp/a.env"]).unwrap().file,
        Some(PathBuf::from("/tmp/a.env"))
    );
    assert_eq!(
        parse_of(&["path", "--file=/tmp/b.env"]).unwrap().file,
        Some(PathBuf::from("/tmp/b.env"))
    );
}

#[test]
fn defaults_are_none_so_the_runner_resolves_them() {
    assert_eq!(
        parse_of(&["list"]).unwrap(),
        Args {
            sub: Sub::List,
            file: None,
            venue: None,
            json: false,
            key: None,
            from_env: None,
            dry_run: false,
            account_id: None,
            venue_account_id: None,
            replace: false,
            clear: false,
            account_action: None,
            tier: None,
            label: None,
            no_label: false,
            confirm: None,
        }
    );
}

#[test]
fn usage_errors_are_clean() {
    assert!(parse_of(&[]).unwrap_err().contains("subcommand is required"));
    assert!(parse_of(&["frobnicate"]).unwrap_err().contains("unknown `secrets` subcommand"));
    assert!(parse_of(&["list", "--nope"]).unwrap_err().contains("unknown option"));
    assert!(parse_of(&["list", "--file"]).unwrap_err().contains("requires a value"));
}

#[test]
fn help_short_circuits_at_both_levels() {
    assert_eq!(parse_of(&["--help"]).unwrap_err(), "help requested");
    assert_eq!(parse_of(&["list", "-h"]).unwrap_err(), "help requested");
}

fn args_with(file: Option<&str>) -> Args {
    Args {
        sub: Sub::List,
        file: file.map(PathBuf::from),
        venue: None,
        json: false,
        key: None,
        from_env: None,
        dry_run: false,
        account_id: None,
        venue_account_id: None,
        replace: false,
        clear: false,
        account_action: None,
        tier: None,
        label: None,
        no_label: false,
        confirm: None,
    }
}

/// **The store is the settings directory's `secrets.env`, and nothing resolves a second one.**
///
/// `--file` outranks it so an operator can inspect a store directly; with neither, the walk from
/// the working directory answers — the same resolver `load_workspace_dotenv` takes, so the two
/// cannot disagree about what a daemon on this box would read.
///
/// With NO override in hand the last arm is byte-identical to the blind spelling it replaced,
/// which is why that substitution changed nothing for the ordinary checkout. The arm where it
/// is NOT identical has its own test below.
#[test]
fn the_store_is_secrets_env_inside_the_dispatchers_settings_dir() {
    let settings = Path::new("/opt/vike/settings");
    assert_eq!(store_path(&args_with(None), Some(settings), None), settings.join("secrets.env"));
    assert_eq!(
        store_path(&args_with(Some("/tmp/explicit.env")), Some(settings), None),
        PathBuf::from("/tmp/explicit.env")
    );
    assert_eq!(store_path(&args_with(None), None, None), vike_secrets::workspace_dotenv_path());
    assert_eq!(
        store_path(&args_with(Some("/tmp/explicit.env")), None, None),
        PathBuf::from("/tmp/explicit.env")
    );
}

/// **Given a `None` directory beside a `Some` override, [`store_path`] resolves the NAMED
/// store — and this is the test that would go red the day somebody spells the blind resolver
/// here again.**
///
/// The claim it refutes is a reasonable-sounding one: *the last arm of [`store_path`] runs only
/// when the dispatcher's boot found nothing, and in that case
/// `vike_secrets::workspace_dotenv_path` is the same pure resolver under the same `None`, so it
/// cannot answer differently.* It cannot — the two resolvers genuinely diverge on that input,
/// which is what this pins.
///
/// What the defect COST, when the pairing was reachable: on such a box every daemon read the
/// named store (`load_workspace_secrets_from_env` -> `resolve_project` ->
/// `workspace_dotenv_path_from`, the override carried the whole way), while `vike-cli secrets
/// path` printed the relative last resort `settings/secrets.env`. That is the one answer this
/// command may not get wrong — the operator running it is already looking for a
/// misconfiguration, and it would point them at a file nothing on the box reads.
///
/// ⚠ **`crate::run` can no longer PRODUCE this pairing**, because `vike_boot::boot` honours a
/// name with no walk (`vike_secrets::project_settings_dir_for`). The inputs here are therefore
/// synthetic ON PURPOSE: this is a unit test of [`store_path`]'s own contract, so it keeps
/// covering the fallback arm whether or not any caller can reach it, and it is deliberately not
/// deleted along with the reachability —
/// `a_boot_with_no_working_directory_resolves_the_named_directory` is the pin on the upstream
/// half, and if that one regresses this one is what still holds the behaviour.
///
/// The `assert_ne!` is deliberate and load-bearing: the two `assert_eq!`s above it pass under
/// the blind spelling too whenever the CHECKOUT the test runs in happens to walk to a matching
/// path, and only the inequality states the property that actually broke.
#[test]
fn the_store_honours_the_override_when_no_settings_dir_was_resolved() {
    let named = "/srv/vike-<unit>/settings";

    // What every OTHER credential reader on that box resolves, override in hand.
    let daemon_reads = vike_secrets::workspace_dotenv_path_from(Some(named));
    assert_eq!(daemon_reads, Path::new(named).join(SECRETS_FILE));

    assert_eq!(
        store_path(&args_with(None), None, Some(named)),
        daemon_reads,
        "`secrets path` must name the file the rest of the program opens"
    );
    assert_ne!(
        vike_secrets::workspace_dotenv_path(),
        daemon_reads,
        "the override-BLIND spelling cannot produce the named store — that is the defect"
    );

    // `--file` still outranks everything, override or no override.
    assert_eq!(
        store_path(&args_with(Some("/tmp/explicit.env")), None, Some(named)),
        PathBuf::from("/tmp/explicit.env")
    );
    // …and a settings directory that WAS resolved is still what answers: the override rung is a
    // fallback, never a second opinion about a directory the boot already named.
    let settings = Path::new("/opt/vike/settings");
    assert_eq!(
        store_path(&args_with(None), Some(settings), Some(named)),
        settings.join(SECRETS_FILE)
    );
}

/// **The reachability half — and the direction it measures has FLIPPED, which is the finding.**
///
/// It used to assert that this dispatcher really does hand [`store_path`] a `None` directory
/// beside a `Some` override, because `vike_boot::boot` resolved the directory as
/// `spec.cwd.and_then(..)` and so dropped a name that needed no walk. #1514 taught this command
/// to survive that pairing; the ROOT CAUSE is now fixed in `vike_boot::boot`, which calls
/// `vike_secrets::project_settings_dir_for` — so the pairing no longer exists and the FIRST
/// assertion below is the one that would go red if it came back.
///
/// The test is kept rather than deleted precisely because of that: it is the pin on the upstream
/// behaviour this command's fallback rung was written for, and a regression there is silent —
/// every daemon on the box keeps reading the named store while `secrets path` starts printing a
/// relative last resort, with nothing failing anywhere. `store_path`'s own unit test
/// (`the_store_honours_the_override_when_no_settings_dir_was_resolved`) still drives the
/// synthetic pairing directly, so the fallback stays covered whether or not a boot can produce
/// it.
///
/// It runs the REAL `vike_boot::boot` under the same spec `crate::resolve_policy` builds, with
/// the input that used to produce the pairing: no working directory. `cwd` is a `BootSpec`
/// FIELD, so this needs no process-global mutation and races nothing — the same reason
/// `vike_secrets::project_settings_dir_for` takes it as a parameter one layer down.
///
/// ⚠ `settings: SettingsLoad::Load` mirrors `crate::resolve_policy` rather than skipping: the
/// claim is about the sequence this binary actually runs. The named directory below does not
/// exist, so the loader opens no file (absent files inside a settings directory are skipped
/// individually), and `env` is this test's own map — nothing here reads the real environment or
/// the real settings tree.
#[test]
fn a_boot_with_no_working_directory_resolves_the_named_directory() {
    let named = "/srv/vike-<unit>/settings";
    let env: std::collections::HashMap<String, String> =
        [("VIKE_SETTINGS_DIR".to_string(), named.to_string())].into_iter().collect();

    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env: &env,
        cwd: None,
        identity: vike_boot::Identity { name: "vike-cli", version: "0.0.0-test" },
        removed_env: vike_boot::RemovedEnv::Refuse,
        settings: vike_boot::SettingsLoad::Load,
        ceilings: vike_boot::Ceilings::NotInterpreted("this test reads no ceiling"),

        credentials: vike_boot::Credentials::Deferred("this test opens no credential file"),
        log_home: vike_boot::LogHome::Elsewhere("this CLI builds no subscriber"),
        disclosure: vike_boot::Disclosure::Skip("no disclosure is rendered here"),
    })
    .expect("a boot with no working directory is a legitimate boot, not a failure");

    assert_eq!(
        booted.settings_dir.as_deref(),
        Some(Path::new(named)),
        "a NAMED settings directory needs no walk — dropping it because the working directory \
             is gone is the defect this asserts against"
    );
    assert_eq!(
        booted.settings_dir_override.as_deref(),
        Some(named),
        "…and the rung that answered is still reported"
    );

    assert_eq!(
        store_path(
            &args_with(None),
            booted.settings_dir.as_deref(),
            booted.settings_dir_override.as_deref(),
        ),
        Path::new(named).join(SECRETS_FILE),
        "end to end: the dispatcher's own values must resolve the NAMED store — now through \
             the FIRST arm, where they used to reach the fallback"
    );
}

/// Every `Source` variant must be distinguishable in the output, and none may carry a value.
#[test]
fn describe_names_the_store_and_never_a_secret() {
    let found = describe(&Source::File(PathBuf::from("/p/settings/secrets.env")));
    assert!(found.contains("/p/settings/secrets.env"));
    assert!(describe(&Source::None).contains("paper"));
    assert_ne!(found, describe(&Source::None));
    assert!(!found.contains('='), "a store description must never carry a KEY=value: {found}");
}

/// **Only `NotFound` is absence; a probe that FAILED is its own answer.**
///
/// This is the guard, and it is the whole point of [`Presence`]. The old probe was
/// `Path::exists`, which folds EVERY error into `false` — so an unsearchable project root
/// (`EACCES`) reported the store as cleanly absent and `path` printed the fresh-install advice
/// for a permissions bug. The `exists()` assertion below is that defect, reproduced: it is what
/// this command used to print from, and it still answers `false` here.
///
/// ⚠ The third state is produced by probing THROUGH a regular file (`ENOTDIR`) rather than by
/// `chmod 0o000` on a parent — a mode-based denial is a no-op for root, and CI runs as root, so
/// that shape would pass VACUOUSLY there. `ENOTDIR` is uid-independent. It is the same trick
/// `crates/vike-secrets/src/store_tests.rs`'s `only_not_found_counts_as_absent_and_a_rootless_path_is_skipped`
/// uses, for the same reason. Unix-only: Windows reports a path under a file as
/// `ERROR_PATH_NOT_FOUND`, which IS `NotFound`, so there is no non-`NotFound` error to make
/// portably — the two states above are still checked there.
///
/// ⚠ **The fixture is a BOUND `tempfile::TempDir`, and all three reasons are load-bearing.**
/// It used to mint `<system-temp>/vike-cli-presence-<pid>-<ThreadId>` directly in the shared
/// system temp root, and that shape failed this test's own contract three ways at once. (1) Its
/// FIRST assertion needs the store to be ABSENT, so any leftover of that name decides the
/// verdict from outside the fixture — and Linux recycles pids while nextest runs one test per
/// process, so the name is not as unique across users as it looks. (2) `/tmp` is 1777 sticky:
/// a leftover owned by the OTHER user makes the leading `remove_dir_all` fail `EACCES`, which
/// `let _ =` swallowed, after which `create_dir_all` returns **Ok** (std treats `EEXIST` on a
/// directory as success) and the test asserts about a stranger's directory. (3) Its only
/// cleanup was the LAST STATEMENT, so any earlier panic leaked the directory permanently —
/// which is the state that arms (1) and (2) for the next user. A `TempDir` claims its name
/// `O_EXCL` and removes it on the panic path too, so none of the three survives.
#[test]
fn only_not_found_reads_as_absent_and_a_failed_probe_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path().to_path_buf();
    let store = d.join("secrets.env");

    let p = presence(&store);
    assert!(matches!(p, Presence::Absent), "nothing there: NotFound IS established absence");
    assert_eq!(p.label(), "absent");

    std::fs::write(&store, "BINANCE_LIVE_API_KEY=never-printed\n").unwrap();
    let p = presence(&store);
    assert!(matches!(p, Presence::Present), "a file that stats is present");
    assert_eq!(p.label(), "present");

    #[cfg(unix)]
    {
        let under_a_file = store.join("settings").join(SECRETS_FILE);
        assert!(
            !under_a_file.exists(),
            "the OLD probe answers `false` here — that is the defect, not the fixture"
        );
        let p = presence(&under_a_file);
        assert!(matches!(p, Presence::Undetermined(_)), "ENOTDIR is not absence: {p:?}");
        let label = p.label();
        assert!(label.contains("could not be determined"), "{label}");
        assert!(!label.contains("absent"), "a failed probe must not read as absence: {label}");
    }

    // **THE POSITIVE PROOF that the fixture is OWNED**, and the reason this is a `drop` rather
    // than the `remove_dir_all` that stood here. A green run cannot tell an owned directory
    // from a leaked one — the old inline cleanup ran on the PASSING path too, and every
    // directory it leaked was left by a run that panicked first. Dropping the handle and
    // finding the tree gone is the property itself, and it holds on the panic path by
    // construction because `Drop` is what runs there.
    drop(dir);
    assert!(
        !d.exists(),
        "the fixture must remove itself: {} survived its own TempDir, and a leftover here is \
             exactly what decides this test's FIRST assertion from outside the fixture for the \
             next user on a 1777 sticky /tmp",
        d.display()
    );
}

#[test]
fn usage_documents_every_subcommand_and_where_the_store_is() {
    for needle in [
        "list",
        "path",
        "template",
        "set",
        "migrate",
        "--file",
        "--dry-run",
        "settings/secrets.env",
        "VIKE_SETTINGS_DIR",
    ] {
        assert!(USAGE.contains(needle), "USAGE must mention {needle}");
    }
}

// --- USAGE and the missing-subcommand error name every subcommand `parse` accepts -----------------
//
// `move-venue-config` was accepted by `parse`, shipped in the CHANGELOG, and absent from
// `vike-cli secrets --help` — which is where an operator looking for it looks, and what a probe of
// "does this box's binary have the verb" reads. The hand-written needle list in
// `usage_documents_every_subcommand_and_where_the_store_is` above cannot notice a subcommand nobody
// thought to add to it, so the roster here is DERIVED from `parse`'s own source.

/// Every subcommand word `parse` accepts: each `"<word>" => Sub::…` arm between
/// `let sub = match first.as_str() {` and the `other =>` arm, read from this module's own source
/// text. A new arm is a name this test already knows; the `-h | --help | help` arm maps to no `Sub`
/// and is not one.
fn accepted_subcommands() -> Vec<&'static str> {
    const SOURCE: &str = include_str!("secrets.rs");
    let start = SOURCE
        .find("let sub = match first.as_str() {")
        .expect("`parse`'s subcommand match moved — re-key this derivation");
    let mut words = Vec::new();
    for line in SOURCE[start..].lines().skip(1) {
        let line = line.trim_start();
        if line.starts_with("other =>") {
            return words;
        }
        if let Some(rest) = line.strip_prefix('"')
            && let Some((word, tail)) = rest.split_once('"')
            && tail.trim_start().starts_with("=> Sub::")
        {
            words.push(word);
        }
    }
    panic!("`parse`'s subcommand match has no `other =>` arm any more — re-key this derivation");
}

/// The subcommand words a USAGE text lists: the first token of each line in its `subcommands:`
/// section that is indented by exactly two spaces (a description line is indented further, an
/// option line belongs to the section after `options:`).
fn listed_subcommands(usage: &str) -> Vec<&str> {
    let section = usage
        .split("\nsubcommands:\n")
        .nth(1)
        .and_then(|after| after.split("\noptions:\n").next())
        .expect("USAGE has a `subcommands:` section followed by an `options:` one");
    section
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("  ")?;
            if rest.starts_with(' ') {
                return None;
            }
            rest.split_whitespace().next()
        })
        .collect()
}

/// The accepted subcommands a USAGE text does not list.
fn missing_from(usage: &str, accepted: &[&'static str]) -> Vec<&'static str> {
    let listed = listed_subcommands(usage);
    accepted.iter().copied().filter(|word| !listed.contains(word)).collect()
}

/// **`--help` lists every subcommand `parse` accepts — and nothing `parse` would refuse.** Both
/// directions, so the roster can neither lose a verb silently nor keep naming one that is gone.
#[test]
fn usage_lists_every_subcommand_parse_accepts_and_no_other() {
    let accepted = accepted_subcommands();
    // The derivation must have read the real match, or every assertion below is about nothing.
    assert!(
        accepted.contains(&"list") && accepted.contains(&"move-venue-config"),
        "derived {accepted:?} from `parse` — the marker lines moved"
    );
    assert_eq!(
        missing_from(USAGE, &accepted),
        Vec::<&str>::new(),
        "`vike-cli secrets --help` does not list a subcommand `parse` accepts"
    );
    for word in listed_subcommands(USAGE) {
        assert!(accepted.contains(&word), "USAGE lists `{word}`, which `parse` does not accept");
    }
}

/// The negative control for the test above: with the `move-venue-config` entry cut out of a copy of
/// USAGE, the very same check names it. Without this an empty result could be a derivation that
/// stopped looking, which is the shape of pass this repository calls an assertion that cannot fail
/// for its stated reason.
#[test]
fn the_usage_check_names_a_subcommand_that_is_cut() {
    let accepted = accepted_subcommands();
    let doctored = USAGE.replacen("\n  move-venue-config\n", "\n", 1);
    assert_ne!(doctored, USAGE, "the mutation must remove the entry, or it proves nothing");
    assert_eq!(missing_from(&doctored, &accepted), ["move-venue-config"]);
}

/// The missing-subcommand refusal enumerates the same roster as `parse` — it is typed, not derived,
/// so this is what keeps it from drifting.
#[test]
fn the_missing_subcommand_error_lists_the_same_roster() {
    let err = parse_of(&[]).unwrap_err();
    let inside = err
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("no parenthesised roster in {err:?}"));
    let mut named: Vec<&str> = inside.split(" | ").collect();
    let mut accepted = accepted_subcommands();
    named.sort_unstable();
    accepted.sort_unstable();
    assert_eq!(named, accepted, "the error names a different roster than `parse` accepts");
}
