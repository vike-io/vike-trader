use super::*;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

#[test]
fn an_environment_with_none_of_them_starts_normally() {
    assert_eq!(refuse_removed_env(&HashMap::new()), Ok(()));
    assert_eq!(refuse_removed_env(&env(&[("VIKE_RECONCILE", "1")])), Ok(()));
}

#[test]
fn a_set_variable_is_refused_naming_the_key_and_the_replacement_line() {
    let err = refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", "250")])).unwrap_err();
    assert!(err.contains("VIKE_MAX_ORDER_NOTIONAL"), "{err}");
    // The operator's own value, rendered as the command they can paste — there is no settings
    // file to name any more (`docs/decisions/0086`).
    assert!(err.contains("vike-cli config set policy.max_notional_per_order 250"), "{err}");
    assert!(!err.contains(".toml"), "no settings file may be named as a write target: {err}");
    assert!(err.contains("NO LONGER READ"), "{err}");
}

#[test]
fn the_daemon_variable_is_refused_the_same_way() {
    let err =
        refuse_removed_env(&env(&[("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "1000")])).unwrap_err();
    assert!(err.contains("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL"), "{err}");
    assert!(err.contains("policy.max_notional_per_order 1000"), "{err}");
}

/// One pass, every offender — not one restart per variable.
#[test]
fn both_variables_are_reported_together() {
    let err = refuse_removed_env(&env(&[
        ("VIKE_MAX_ORDER_NOTIONAL", "250"),
        ("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "1000"),
    ]))
    .unwrap_err();
    assert!(err.contains("VIKE_MAX_ORDER_NOTIONAL="), "{err}");
    assert!(err.contains("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL="), "{err}");
}

/// The UNREAD-key shape: no replacement key, so no paste-able line — but the refusal still
/// names the variable, the SECTION it used to live in (never a file, since `docs/decisions/0086`
/// leaves no settings file for anything to be read from), and the reason, and it must NOT send
/// the operator to `VIKE_STATE_ROOT`.
#[test]
fn the_unread_state_dir_is_refused_and_offers_no_replacement_line() {
    let err = refuse_removed_env(&env(&[("VIKE_STATE_DIR", "/srv/vike/state")])).unwrap_err();
    assert!(err.contains("VIKE_STATE_DIR"), "{err}");
    assert!(err.contains("NO LONGER READ"), "{err}");
    assert!(err.contains("`config`"), "{err}");
    assert!(!err.contains(".toml"), "no settings file may be named: {err}");
    assert!(err.contains("nothing reads it"), "{err}");
    // `key: None` ⇒ no TOML line at all.
    assert!(!err.contains("state_dir = "), "{err}");
}

/// A blank value configured nothing before, so nobody believes a ceiling is active from one.
#[test]
fn an_empty_or_whitespace_value_is_not_a_belief_worth_refusing() {
    assert_eq!(refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", "")])), Ok(()));
    assert_eq!(refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", "   ")])), Ok(()));
}

/// An unparseable or non-positive value still REFUSES (it is set, and someone believes it) —
/// but the suggested line must be one that actually loads, not an echo of the garbage.
#[test]
fn a_garbage_value_still_refuses_but_suggests_a_usable_line() {
    for bad in ["nope", "0", "-5", "NaN"] {
        let err = refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", bad)])).unwrap_err();
        assert!(err.contains("VIKE_MAX_ORDER_NOTIONAL"), "{bad}: {err}");
        assert!(
            err.contains("policy.max_notional_per_order <a positive number"),
            "{bad} must not be echoed into the suggested line: {err}"
        );
    }
}

/// Every row that names a KEY names one `vike-cli config set` takes — a refusal that sends an
/// operator to a key the writer rejects sends them in a circle.
#[test]
fn every_row_that_names_a_key_names_one_the_writer_accepts() {
    for r in REMOVED_ENV {
        // A per-venue split prints every key it became, and each must be one the writer accepts.
        let also: &[&str] = match r.value {
            ValueMap::KillSwitchEach { also, .. } => also,
            _ => &[],
        };
        for key in r.key.into_iter().chain(also.iter().copied()) {
            if let Some(venue) = key.strip_prefix("policy.venues.") {
                assert!(vike_model::VENUES.contains(&venue), "{}: {key}", r.var);
            } else if let Some(field) = key.strip_prefix("policy.") {
                let patch: crate::PolicyPatch = toml::from_str(&format!("{field} = 123.0"))
                    .unwrap_or_else(|e| {
                        panic!("{} names `{key}`, which the writer rejects: {e}", r.var)
                    });
                crate::Policy::default()
                    .apply(patch, std::path::Path::new("policy.toml"))
                    .expect("123.0 must be an accepted value for the named key");
            } else if let Some(flag) = key.strip_prefix("flags.") {
                assert!(crate::FLAG_REGISTRY.iter().any(|m| m.field == flag), "{}: {key}", r.var);
            } else if let Some(rest) = key.strip_prefix("venue.") {
                let (venue, field) = rest.split_once('.').expect("venue.<venue>.<field>");
                assert!(
                    vike_model::venues::venue_fields::venue_field(venue, field).is_some(),
                    "{}: {key} is not a declared venue field",
                    r.var
                );
            } else {
                panic!("{}: `{key}` is not a shape `vike-cli config set` takes", r.var);
            }
        }
    }
}

/// **A removed variable whose value is itself a secret is refused WITHOUT printing it.**
///
/// The refusal goes to stderr, which is captured by every service manager and every CI log, so
/// echoing the value would turn a startup diagnostic into a credential leak — a worse outcome
/// than the misconfiguration being reported.
#[test]
fn a_secret_valued_variable_is_refused_without_echoing_it() {
    let err = refuse_removed_env(&env(&[("VIKE_SECRETS_PASSPHRASE", "hunter2-correct-horse")]))
        .unwrap_err();
    assert!(err.contains("VIKE_SECRETS_PASSPHRASE"), "{err}");
    assert!(err.contains("NO LONGER READ"), "{err}");
    assert!(
        !err.contains("hunter2-correct-horse"),
        "the refusal must never print the value: {err}"
    );
    // It names where credentials come from, and offers no TOML line to paste (nothing moved).
    assert!(err.contains("settings/secrets.env"), "{err}");
    assert!(!err.contains(" = "), "there is no key to set: {err}");
}

// -- the removed FILE -----------------------------------------------------------------------

/// `<project>/settings` -> the project is its parent, and that is the only place looked at.
#[test]
fn a_project_file_beside_the_settings_directory_is_refused_by_full_path() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    std::fs::create_dir(&settings).unwrap();
    let file = project.path().join(REMOVED_PROJECT_FILE);
    std::fs::write(&file, "[config]\nlog_dir = \"/from/project\"\n").unwrap();

    let err = refuse_removed_project_file(Some(&settings)).unwrap_err();
    assert!(matches!(err, ConfigError::RemovedProjectFile { .. }), "{err}");
    let msg = err.to_string();
    assert!(msg.contains(&file.display().to_string()), "names the exact file: {msg}");
    assert!(msg.contains("NO LONGER READ"), "{msg}");
    // Both sections, so the operator never has to guess which one a key goes under — and
    // neither is named as a FILE to write into, since there is none any more (the message still
    // names the removed `vike.toml` itself, by its full path, which is the file being refused).
    assert!(msg.contains("config set config."), "{msg}");
    assert!(msg.contains("config set preferences."), "{msg}");
    assert!(
        !msg.contains("settings/config.toml") && !msg.contains("settings/preferences.toml"),
        "no settings file may be named as a write target: {msg}"
    );

    // ⚠ THE rule: refuse and instruct, never act on somebody else's file.
    assert!(file.is_file(), "the refusal must not delete, move or rewrite the operator's file");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "[config]\nlog_dir = \"/from/project\"\n",
        "nor edit it"
    );
}

/// Absent is the normal case, and it must cost nothing and say nothing.
#[test]
fn no_project_file_starts_normally() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    std::fs::create_dir(&settings).unwrap();
    assert!(refuse_removed_project_file(Some(&settings)).is_ok());
    // No settings directory at all ⇒ no project ⇒ nothing that could be misleading anybody.
    assert!(refuse_removed_project_file(None).is_ok());
}

/// A directory named `vike.toml` still refuses: `metadata` succeeds, and the operator who made
/// one is at least as confused as the one who wrote a file. Only `NotFound` is absence.
#[test]
fn only_not_found_counts_as_absent() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    std::fs::create_dir(&settings).unwrap();
    std::fs::create_dir(project.path().join(REMOVED_PROJECT_FILE)).unwrap();
    let err = refuse_removed_project_file(Some(&settings)).unwrap_err();
    // `metadata` SUCCEEDED, so this really is the "it is there, delete it" answer.
    assert!(
        matches!(err, ConfigError::RemovedProjectFile { probe: RemovedFileProbe::Present, .. }),
        "{err}"
    );
}

// -- the probe that could not answer -----------------------------------------------------
//
// ⚠ The refusal below is the SAME refusal. Nothing here relaxes it: absence must be
// established, and it was not. What is under test is the CLAIM the message makes.

/// **A stat that failed must not be reported as a file that exists.**
///
/// This is the the CI box defect: under `ProtectHome=yes` the stat of a project root inside `$HOME`
/// returns `EACCES`, and the old single message told the operator a `vike.toml` was present and
/// to delete it — a file that was not there, with none of the causes that produce that errno
/// named. Asserted on the message function directly, with a synthesized error, so the claim is
/// pinned without needing a filesystem that can produce one (the wired end-to-end proof is
/// `a_path_that_cannot_be_probed_refuses_without_claiming_the_file_exists` below).
#[test]
fn an_unprobeable_path_says_so_instead_of_asserting_the_file_is_present() {
    let file = Path::new("/home/the operator/vike-trader-rust").join(REMOVED_PROJECT_FILE);
    let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let errno = e.to_string();
    let msg = removed_project_file_message(&file, &RemovedFileProbe::Unestablished(e));

    // ⚠ THE assertion: it never claims the file is there, and never orders a deletion of it.
    assert!(!msg.contains("is present"), "the message asserts something it does not know: {msg}");
    assert!(msg.contains("could not be STATTED"), "{msg}");
    assert!(msg.contains("NOT the same as absent"), "{msg}");
    assert!(msg.contains("may not exist at all"), "the operator must be told this: {msg}");
    // The errno itself, verbatim — the first clue, and the thing to search for.
    assert!(msg.contains(&errno), "the os error `{errno}` must be quoted: {msg}");
    // …and the causes that actually produce it, in the vocabulary `unwritable_store_root` uses.
    assert!(msg.contains("ProtectHome="), "the measured cause must be named: {msg}");
    assert!(msg.contains("ProtectSystem=strict"), "the sibling cause must be named: {msg}");
    assert!(msg.contains("VIKE_SETTINGS_DIR"), "the way out must be named: {msg}");
    // Still the full path, and still the promise that nothing was touched.
    assert!(msg.contains(&file.display().to_string()), "{msg}");
    assert!(msg.contains("Nothing has been moved or deleted"), "{msg}");
}

/// …and the answer that DID see something is untouched: it still says present, still names both
/// destinations, still orders the deletion. A fix that blurred the two messages into one
/// hedge would have replaced a false claim with no claim.
#[test]
fn a_file_that_was_actually_seen_still_gets_the_delete_it_message() {
    let file = Path::new("/srv/vike-<unit>").join(REMOVED_PROJECT_FILE);
    let msg = removed_project_file_message(&file, &RemovedFileProbe::Present);
    assert!(msg.contains("is present"), "{msg}");
    assert!(msg.contains("NO LONGER READ"), "{msg}");
    assert!(msg.contains(&format!("then delete {}", file.display())), "{msg}");
    assert!(!msg.contains("could not be STATTED"), "the two answers must not blur: {msg}");
}

/// **The wired proof, through the real `std::fs::metadata`.**
///
/// A regular FILE standing where `<project>` should be: every probe below it fails with
/// `ENOTDIR`, which is **uid-independent** — the trick `vike-secrets`' own store tests use,
/// because `chmod 000` proves nothing when the suite runs as root, which CI does.
///
/// ⚠ What it does and does not stand in for. It is a genuine non-`NotFound` stat failure
/// reaching the real function, so it proves the arm is WIRED and that the message never claims
/// presence. It is not `EACCES`, so it does not reproduce `ProtectHome=` itself — that is
/// `a_genuine_eacces_is_reported_the_same_way`'s job when the process is not root, and the
/// the CI box measurement's otherwise. Unix-only: on Windows a path through a regular file resolves
/// to `ERROR_PATH_NOT_FOUND`, which maps to `NotFound` — i.e. Windows answers "absent" and the
/// fixture cannot be built there at all.
#[cfg(unix)]
#[test]
fn a_path_that_cannot_be_probed_refuses_without_claiming_the_file_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    std::fs::write(&project, "a regular file, not a directory").unwrap();
    let settings = project.join("settings");

    let err = refuse_removed_project_file(Some(&settings)).unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::RemovedProjectFile { probe: RemovedFileProbe::Unestablished(_), .. }
        ),
        "a failed stat is not established absence, and not established presence either: {err}"
    );
    let msg = err.to_string();
    assert!(!msg.contains("is present"), "{msg}");
    assert!(msg.contains("could not be STATTED"), "{msg}");
    assert!(msg.contains(&project.join(REMOVED_PROJECT_FILE).display().to_string()), "{msg}");
    // The errno survives as a `source`, so a caller can match on the kind, not the prose.
    let source = std::error::Error::source(&err).expect("the io error must be carried");
    assert!(!source.to_string().is_empty(), "{source}");
}

/// The real thing when the environment allows it: a project directory the process genuinely
/// cannot search, which is `EACCES` — the exact errno `ProtectHome=yes` produces.
///
/// Self-skips as root (and on any filesystem ignoring the mode), where `0o000` denies nothing;
/// the ENOTDIR fixture above is what always runs. Same shape as `vike-data`'s
/// `open_under_an_unwritable_parent_reports_the_sandbox_diagnosis`.
#[cfg(unix)]
#[test]
fn a_genuine_eacces_is_reported_the_same_way() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let settings = project.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    // No `x` bit: nothing inside can be resolved, exactly as under `ProtectHome=yes`.
    std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o000)).unwrap();

    let outcome = refuse_removed_project_file(Some(&settings));

    // Restore first, so a failing assertion cannot leave an unremovable directory behind.
    let _ = std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755));

    match outcome {
        Err(ConfigError::RemovedProjectFile {
            probe: RemovedFileProbe::Unestablished(e), ..
        }) => {
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
        }
        // Root, or a filesystem ignoring the mode: the fixture proved nothing, so say so.
        other => {
            eprintln!("skipped: this process can stat through a 0o000 directory (got {other:?})")
        }
    }
}

/// A settings directory with no parent has no project, so there is nothing to probe. The
/// loader's behaviour there is "skip the check", not "guess a directory".
#[test]
fn a_parentless_settings_directory_is_skipped_rather_than_guessed_at() {
    let root = if cfg!(windows) { Path::new("C:\\") } else { Path::new("/") };
    assert!(refuse_removed_project_file(Some(root)).is_ok());
}

/// Every row that forbids echoing must be one where echoing would actually matter, and every
/// row that permits it must be one where the operator needs the number back. Stated as a test
/// because `echo_value` is a one-word field that a copy-pasted row gets wrong silently.
#[test]
fn only_rows_with_a_key_to_paste_echo_their_value() {
    for r in REMOVED_ENV {
        assert_eq!(
            r.echo_value,
            r.key.is_some() && r.value != ValueMap::Stdin,
            "{}: a row with no key has no line to paste, so it has no reason to echo, and a \
             Stdin row reads its replacement from stdin — the value never touches this line \
             either",
            r.var
        );
    }
}

/// Decision 0095: each retired switch refuses, names what the migration did, and prints the line
/// that goes live ON PURPOSE.
#[test]
fn a_retired_mainnet_switch_refuses_with_the_on_purpose_line() {
    for venue in vike_secrets::live_means_mainnet::SWITCHED_VENUES {
        let var = format!("{}_MAINNET", venue.to_ascii_uppercase());
        let err = refuse_removed_env(&env(&[(&var, "1")])).unwrap_err();
        assert!(err.contains(&var) && err.contains("NO LONGER READ"), "{err}");
        assert!(err.contains("rewrote this venue's `live` lines to `demo`"), "{err}");
        assert!(err.contains("MAINNET on purpose"), "{err}");
        assert!(err.contains(&format!("vike-cli config set policy.venues.{venue} live")), "{err}");
        assert!(err.contains(&format!("then unset {var}")), "{err}");
    }
}

/// The retired switches are exactly the venues the migration rewrites — one list, two consumers.
#[test]
fn the_retired_mainnet_rows_are_exactly_the_migrated_venues() {
    let mut rows: Vec<String> = REMOVED_ENV
        .iter()
        .filter(|r| r.var.ends_with("_MAINNET"))
        .map(|r| r.var.to_string())
        .collect();
    rows.sort();
    let mut want: Vec<String> = vike_secrets::live_means_mainnet::SWITCHED_VENUES
        .iter()
        .map(|v| format!("{}_MAINNET", v.to_ascii_uppercase()))
        .collect();
    want.sort();
    assert_eq!(rows, want);
}

/// Review Focus 5, the refusal half: a proxy URL may carry `user:password@`, so its refusal names
/// the variable and the stdin line and never the value.
#[test]
fn a_set_socks_proxy_refuses_without_printing_its_value() {
    let err =
        refuse_removed_env(&env(&[("POLY_SOCKS_PROXY", "socks5h://u:hunter2@<host>:1080")]))
            .unwrap_err();
    assert!(err.contains("POLY_SOCKS_PROXY is NO LONGER READ"), "{err}");
    assert!(err.contains("vike-cli config set venue.polymarket.socks_proxy -"), "{err}");
    assert!(!err.contains("hunter2") && !err.contains("<host>"), "the value leaked: {err}");
}

/// Decision 0095: every retired Polymarket variable refuses with the paste-ready line its OLD
/// reader's value maps to.
#[test]
fn every_retired_polymarket_variable_refuses_with_its_new_home() {
    for (var, raw, line) in [
        ("POLY_EXEC", "1", "vike-cli config set flags.poly_exec true"),
        ("POLY_RECONCILE", "1  # recon", "vike-cli config set flags.poly_reconcile true"),
        ("POLY_PROXY_ENABLED", "false", "vike-cli config set venue.polymarket.proxy_enabled false"),
        ("POLY_PROXY_ENABLED", "yes", "vike-cli config set venue.polymarket.proxy_enabled true"),
        (
            "POLY_PROXY_HOST",
            "<host>  # tunnel",
            "vike-cli config set venue.polymarket.proxy_host <host>",
        ),
        ("POLY_PROXY_PORT", "11080", "vike-cli config set venue.polymarket.proxy_port 11080"),
        (
            "POLY_WS_PROXY_ENABLED",
            "off",
            "vike-cli config set venue.polymarket.ws_proxy_enabled false",
        ),
        ("POLY_RATE_GATE", "1", "vike-cli config set venue.polymarket.rate_gate 1"),
        (
            "POLY_PRESUBMIT_REGISTER",
            "0",
            "vike-cli config set venue.polymarket.presubmit_register 0",
        ),
        (
            "POLY_EXEC_MARKETS",
            "0xaa 0xbb,0xcc  # updown",
            "vike-cli config set venue.polymarket.exec_markets 0xaa,0xbb,0xcc",
        ),
        (
            "POLY_WS_TOKENS_PER_SOCKET",
            "200",
            "vike-cli config set venue.polymarket.ws_tokens_per_socket 200",
        ),
    ] {
        let err = refuse_removed_env(&env(&[(var, raw)])).unwrap_err();
        assert!(err.contains(&format!("{var} is NO LONGER READ")), "{err}");
        assert!(err.contains(line), "{var}={raw}: expected `{line}` in:\n{err}");
        assert!(err.contains(&format!("then unset {var}")), "{err}");
    }
}

/// A value that configured nothing but the default has no line to paste — the refusal says so
/// rather than printing a command `config set` would refuse.
#[test]
fn a_retired_value_that_only_named_the_default_writes_nothing() {
    let err = refuse_removed_env(&env(&[("POLY_EXEC_MARKETS", "# none yet")])).unwrap_err();
    assert!(err.contains("nothing to write"), "{err}");
    assert!(!err.contains("vike-cli config set venue.polymarket.exec_markets"), "{err}");
}

/// Every printed `venue.*` line is one `vike-cli config set` accepts: for a representative value, the
/// value the refusal prints passes the target field's own grammar.
#[test]
fn every_venue_line_passes_its_fields_grammar() {
    for r in REMOVED_ENV {
        let Some(key) = r.key else { continue };
        let Some(rest) = key.strip_prefix("venue.") else { continue };
        if r.value == ValueMap::Stdin {
            continue;
        }
        let (venue, field) = rest.split_once('.').expect("venue.<venue>.<field>");
        let grammar =
            vike_model::venues::venue_fields::venue_field(venue, field).expect("declared").grammar;
        // A kill switch prints a line only for the `0` it acted on.
        let raw = if matches!(r.value, ValueMap::KillSwitchEach { .. }) { "0" } else { "1" };
        let err = refuse_removed_env(&env(&[(r.var, raw)])).unwrap_err();
        let prefix = format!("vike-cli config set {key} ");
        let value = err
            .lines()
            .find_map(|l| l.trim().strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| panic!("{}: no paste-ready line in:\n{err}", r.var));
        grammar.check(value).unwrap_or_else(|e| panic!("{}: `{value}` — {e}", r.var));
    }
}

// --- an EMPTY value that used to MEAN something (decision 0095's review) ---------------------------
//
// The rule above — a blank line configured nothing, so it starts normally — is true of every row but
// two. The deleted Polymarket resolver read `POLY_SOCKS_PROXY=` as "connect direct" and
// `POLY_WS_PROXY_ENABLED=` as "send the WebSocket lanes direct"; a unit carrying either is an operator
// who chose to bypass the tunnel, and a build that ignored the blank line would silently dial the
// built-in SOCKS proxy instead.

/// An empty (or whitespace-only) `POLY_SOCKS_PROXY` is REFUSED, and the message says what the blank
/// line used to do and gives the exact row that says the same — not a line `config set` would reject.
#[test]
fn an_empty_socks_proxy_is_refused_because_empty_meant_direct() {
    for blank in ["", "   ", "\t"] {
        let err = refuse_removed_env(&env(&[("POLY_SOCKS_PROXY", blank)])).unwrap_err();
        assert!(err.contains("POLY_SOCKS_PROXY is set to an EMPTY value"), "{blank:?}: {err}");
        assert!(err.contains("POLY_SOCKS_PROXY is NO LONGER READ"), "{blank:?}: {err}");
        assert!(err.contains("connect DIRECT on both lanes"), "what the blank line meant: {err}");
        assert!(err.contains("with no error anywhere"), "the silent fallback is named: {err}");
        assert!(
            err.contains("printf direct | vike-cli config set venue.polymarket.socks_proxy -"),
            "the replacement is the `direct` sentinel on STDIN (the field is a secret): {err}"
        );
        assert!(err.contains("then unset POLY_SOCKS_PROXY"), "{err}");
        assert!(!err.contains("nothing to write"), "an empty value HAS a line to write: {err}");
    }
}

/// The same for `POLY_WS_PROXY_ENABLED=`: it forced the WebSocket lanes direct.
#[test]
fn an_empty_ws_proxy_flag_is_refused_because_empty_forced_the_ws_lanes_direct() {
    for blank in ["", "  "] {
        let err = refuse_removed_env(&env(&[("POLY_WS_PROXY_ENABLED", blank)])).unwrap_err();
        assert!(err.contains("POLY_WS_PROXY_ENABLED is set to an EMPTY value"), "{err}");
        assert!(err.contains("send the WebSocket lanes DIRECT"), "{err}");
        assert!(
            err.contains("vike-cli config set venue.polymarket.ws_proxy_enabled false"),
            "{err}"
        );
        assert!(err.contains("then unset POLY_WS_PROXY_ENABLED"), "{err}");
    }
}

/// One pass: an empty exception and an ordinary offender are reported together, and an ordinary row
/// that is EMPTY stays out of the report.
#[test]
fn an_empty_exception_is_reported_beside_the_ordinary_offenders() {
    let err = refuse_removed_env(&env(&[
        ("POLY_SOCKS_PROXY", ""),
        ("POLY_PROXY_HOST", "<host>"),
        ("POLY_RATE_GATE", ""),
    ]))
    .unwrap_err();
    assert!(err.contains("POLY_SOCKS_PROXY is set to an EMPTY value"), "{err}");
    assert!(err.contains("POLY_PROXY_HOST=<host> is set"), "{err}");
    assert!(!err.contains("POLY_RATE_GATE"), "a blank ordinary row is not a belief: {err}");
}

/// **The roster of exceptions is pinned, and every other row still starts on a blank line.** A third
/// exception is a decision about what an old reader did with an empty value, so it must be made on
/// purpose — and a row that silently started refusing blanks would turn a stale empty line in a unit
/// file into an outage.
#[test]
fn every_other_row_still_treats_an_empty_value_as_unset() {
    for r in REMOVED_ENV {
        for blank in ["", "  ", "\t"] {
            let refused = refuse_removed_env(&env(&[(r.var, blank)])).is_err();
            assert_eq!(
                refused,
                r.when_empty.is_some(),
                "{}={blank:?}: refused-when-empty must be exactly `when_empty.is_some()`",
                r.var
            );
        }
    }
    let mut exceptions: Vec<&str> =
        REMOVED_ENV.iter().filter(|r| r.when_empty.is_some()).map(|r| r.var).collect();
    exceptions.sort_unstable();
    assert_eq!(
        exceptions,
        ["POLY_REDEEM_HALT", "POLY_SOCKS_PROXY", "POLY_WS_PROXY_ENABLED"],
        "the rows whose blank value meant something — see the module doc's \"What counts as set\""
    );
}

/// An empty meaning writes a value exactly when there is a row to write it to. A row with a key
/// and no value would be a refusal with no line; a row with a value and no key would print a value
/// with nowhere to put it.
#[test]
fn an_empty_meaning_writes_a_value_exactly_when_the_row_has_a_key() {
    for r in REMOVED_ENV {
        let Some(empty) = r.when_empty else { continue };
        assert_eq!(
            r.key.is_some(),
            !empty.write.is_empty(),
            "{}: `when_empty.write` must be set exactly when the row has a `key`",
            r.var
        );
    }
}

/// The value an empty-refusal tells the operator to write passes the target field's own grammar —
/// the empty twin of `every_venue_line_passes_its_fields_grammar`, which skips the secret row.
#[test]
fn every_empty_meaning_passes_its_fields_grammar() {
    for r in REMOVED_ENV {
        let Some(empty) = r.when_empty else { continue };
        // A key-less row writes nothing — see the test above.
        let Some(key) = r.key else { continue };
        let rest = key.strip_prefix("venue.").expect("a venue row");
        let (venue, field) = rest.split_once('.').expect("venue.<venue>.<field>");
        let grammar =
            vike_model::venues::venue_fields::venue_field(venue, field).expect("declared").grammar;
        grammar.check(empty.write).unwrap_or_else(|e| panic!("{}: `{}` — {e}", r.var, empty.write));
    }
}

// --- the venue toggles (decision 0095) -----------------------------------------------------------

/// Decision 0095: each retired venue toggle refuses startup and prints the line that writes its
/// row. The two flags carry `1` → `true` and anything else → `false` (their environment layer took
/// `1`/`0` only); an exact-`1` venue field carries `1` → `1` and anything else → `0`.
#[test]
fn a_retired_venue_toggle_refuses_with_its_config_set_line() {
    for (var, raw, line) in [
        ("HYPERLIQUID_HIP3", "1", "vike-cli config set flags.hyperliquid_hip3 true"),
        ("HYPERLIQUID_HIP3", "0", "vike-cli config set flags.hyperliquid_hip3 false"),
        ("VIKE_ALLOW_WITHDRAW_KEYS", "1", "vike-cli config set flags.allow_withdraw_keys true"),
        (
            "VIKE_BINANCE_TRADE_LITE_FILL",
            "1",
            "vike-cli config set venue.binance.trade_lite_fill 1",
        ),
        ("VIKE_BYBIT_FAST_EXEC", "true", "vike-cli config set venue.bybit.fast_exec 0"),
        ("VIKE_MARK_STREAMS_ASTER", "1", "vike-cli config set venue.aster.mark_streams 1"),
    ] {
        let err = refuse_removed_env(&env(&[(var, raw)])).unwrap_err();
        assert!(err.contains(&format!("{var} is NO LONGER READ")), "{err}");
        assert!(err.contains(line), "{var}={raw}: expected `{line}` in:\n{err}");
        assert!(err.contains(&format!("then unset {var}")), "{err}");
    }
}

/// The master mark-stream kill became one field per venue: its `0` prints one `… 0` line per
/// mark-stream venue, and any other value — which configured nothing but the defaults — prints
/// nothing to write.
#[test]
fn the_retired_mark_stream_master_prints_one_line_per_venue() {
    let err = refuse_removed_env(&env(&[("VIKE_MARK_STREAMS", "0")])).unwrap_err();
    for venue in ["binance", "bybit", "okx", "hyperliquid", "aster"] {
        let line = format!("vike-cli config set venue.{venue}.mark_streams 0");
        assert!(err.contains(&line), "{line}: {err}");
    }
    assert_eq!(err.matches("vike-cli config set").count(), 5, "one line per venue: {err}");
    let err = refuse_removed_env(&env(&[("VIKE_MARK_STREAMS", "1")])).unwrap_err();
    assert!(err.contains("nothing to write"), "{err}");
    assert!(!err.contains("vike-cli config set"), "{err}");
}

/// **The master kill still wins when both mark-stream variables are set.** The deleted resolver
/// checked `VIKE_MARK_STREAMS=0` FIRST and returned OFF for every venue, an explicit
/// `VIKE_MARK_STREAMS_ASTER=1` included. The refusal prints one block per variable in
/// `REMOVED_ENV`'s order, so the aster row comes before the master's: pasted top to bottom, the
/// last `venue.aster.mark_streams` line written is the master's `0` — the answer the old resolver
/// gave.
#[test]
fn the_master_kill_still_wins_when_both_mark_stream_variables_are_set() {
    let err =
        refuse_removed_env(&env(&[("VIKE_MARK_STREAMS_ASTER", "1"), ("VIKE_MARK_STREAMS", "0")]))
            .unwrap_err();
    let aster: Vec<&str> = err
        .lines()
        .filter_map(|l| l.trim().strip_prefix("vike-cli config set venue.aster.mark_streams "))
        .collect();
    assert_eq!(aster, ["1", "0"], "the per-venue opt-in first, the master kill last:\n{err}");
}

/// **A commented master kill never killed anything, and the refusal says so.** The deleted
/// resolver compared the UNTRIMMED value with `0`, so `VIKE_MARK_STREAMS=0 # x` left every mark
/// stream at its charter default. The refusal shows the value as the process received it, names
/// those defaults, makes writing nothing the way to keep them, and prints the five `0` lines only
/// under "If you MEANT `0`".
#[test]
fn a_commented_master_kill_ran_the_defaults_and_its_rows_are_only_offered() {
    let err = refuse_removed_env(&env(&[("VIKE_MARK_STREAMS", "0 # x")])).unwrap_err();
    assert!(err.contains(r#"VIKE_MARK_STREAMS="0 # x" is set"#), "the value as received: {err}");
    assert!(err.contains(r#"did NOT act on "0 # x""#), "{err}");
    assert!(
        err.contains("(binance on, bybit on, okx on, hyperliquid on, aster off)"),
        "the defaults that ran: {err}"
    );
    assert!(err.contains("To keep that, write nothing."), "{err}");
    let (before, after) = err.split_once("If you MEANT `0`:").expect("the conditional block");
    assert!(!before.contains("vike-cli config set"), "no row line before the condition: {err}");
    assert_eq!(after.matches("vike-cli config set").count(), 5, "one per venue: {err}");
    assert!(!err.contains("set each field instead"), "not the exact `0`'s lead: {err}");
    assert!(err.contains("then unset VIKE_MARK_STREAMS"), "{err}");
}

/// **With its own variable also set, aster is not claimed by a commented master's defaults.** The
/// master never killed anything, so aster ran as `VIKE_MARK_STREAMS_ASTER` set it — ON here — and
/// the master's block names that variable for aster instead of its charter default.
#[test]
fn a_commented_master_names_asters_own_variable_when_it_is_set_too() {
    let err = refuse_removed_env(&env(&[
        ("VIKE_MARK_STREAMS", "0 # x"),
        ("VIKE_MARK_STREAMS_ASTER", "1"),
    ]))
    .unwrap_err();
    assert!(
        err.contains(
            "(binance on, bybit on, okx on, hyperliquid on, aster per `VIKE_MARK_STREAMS_ASTER`)"
        ),
        "{err}"
    );
    assert!(!err.contains("aster off"), "aster ran ON from its own variable: {err}");
    assert!(
        err.contains("Set it instead:\n\n    vike-cli config set venue.aster.mark_streams 1\n"),
        "aster's own block is the ordinary one: {err}"
    );
}

/// **A padded or commented `1` left an exact-match toggle at its default (OFF).** The old readers
/// of these three compared the UNTRIMMED value with `1`, so `" 1"` and `"1 # x"` never turned
/// anything on: the row that would is offered only under "If you MEANT `1`".
#[test]
fn a_padded_or_commented_one_left_an_exact_match_toggle_off() {
    for (var, raw, key) in [
        ("VIKE_BINANCE_TRADE_LITE_FILL", "1 # x", "venue.binance.trade_lite_fill"),
        ("VIKE_BINANCE_TRADE_LITE_FILL", " 1", "venue.binance.trade_lite_fill"),
        ("VIKE_BYBIT_FAST_EXEC", "1 # x", "venue.bybit.fast_exec"),
        ("VIKE_MARK_STREAMS_ASTER", "1 # x", "venue.aster.mark_streams"),
    ] {
        let err = refuse_removed_env(&env(&[(var, raw)])).unwrap_err();
        assert!(err.contains(&format!("{var}={raw:?} is set")), "the value as received: {err}");
        assert!(err.contains(&format!("did NOT act on {raw:?}")), "{err}");
        assert!(err.contains("the process that ran took the default (off)"), "{var}: {err}");
        assert!(err.contains("To keep that, write nothing."), "{err}");
        let (before, after) = err.split_once("If you MEANT `1`:").expect("the conditional block");
        assert!(!before.contains("vike-cli config set"), "no row line before the condition: {err}");
        assert!(after.contains(&format!("vike-cli config set {key} 1")), "{err}");
        assert!(err.contains(&format!("then unset {var}")), "{err}");
    }
}

/// **An exact spelling keeps the ordinary message** — the value the old reader compared against,
/// or one whose first token is not `1`/`0` (it meant the default either way). A Polymarket
/// `ExactOne` row keeps its first-token mapping: those readers took the first token, so a
/// commented `1` DID act there.
#[test]
fn an_exact_spelling_keeps_the_ordinary_message() {
    for (var, raw, line) in [
        (
            "VIKE_BINANCE_TRADE_LITE_FILL",
            "0",
            "vike-cli config set venue.binance.trade_lite_fill 0",
        ),
        ("VIKE_BYBIT_FAST_EXEC", "1", "vike-cli config set venue.bybit.fast_exec 1"),
        ("VIKE_BYBIT_FAST_EXEC", "true", "vike-cli config set venue.bybit.fast_exec 0"),
        ("VIKE_MARK_STREAMS_ASTER", "0", "vike-cli config set venue.aster.mark_streams 0"),
        ("POLY_RATE_GATE", "1 # x", "vike-cli config set venue.polymarket.rate_gate 1"),
    ] {
        let err = refuse_removed_env(&env(&[(var, raw)])).unwrap_err();
        assert!(err.contains(&format!("{var}={raw} is set")), "{err}");
        assert!(err.contains(&format!("Set it instead:\n\n    {line}\n")), "{var}={raw}: {err}");
        assert!(!err.contains("If you MEANT"), "{err}");
    }
}

/// **A blank retired venue toggle starts normally — its old reader read a blank as unset.** Each
/// deleted reader, and what `VAR=` did there: `HYPERLIQUID_HIP3` and `VIKE_ALLOW_WITHDRAW_KEYS`
/// went through `Flags::apply_env`, whose `layers::get` skips an empty value, and their remaining
/// readers (the instrument loader's own read, the withdraw gate's process-environment sweep) took
/// only the exact `"1"`; `VIKE_BINANCE_TRADE_LITE_FILL` and `VIKE_BYBIT_FAST_EXEC` took only the
/// exact `"1"`; `VIKE_MARK_STREAMS` turned off only on the exact `"0"`, and
/// `VIKE_MARK_STREAMS_ASTER` read anything but `"1"`/`"0"` as its default. So a blank line never
/// said anything the default did not, and none of the six joins `when_empty`'s roster
/// (`every_other_row_still_treats_an_empty_value_as_unset` holds that roster).
#[test]
fn a_blank_retired_venue_toggle_starts_normally_because_blank_was_unset() {
    for var in [
        "HYPERLIQUID_HIP3",
        "VIKE_ALLOW_WITHDRAW_KEYS",
        "VIKE_BINANCE_TRADE_LITE_FILL",
        "VIKE_BYBIT_FAST_EXEC",
        "VIKE_MARK_STREAMS",
        "VIKE_MARK_STREAMS_ASTER",
    ] {
        let row = REMOVED_ENV.iter().find(|r| r.var == var).unwrap_or_else(|| panic!("{var}"));
        assert_eq!(row.when_empty, None, "{var}: a blank value meant nothing but the default");
        assert_eq!(refuse_removed_env(&env(&[(var, "")])), Ok(()), "{var}");
    }
}

// --- tools, smokes and code nothing starts (decision 0095) ------------------------------------

/// Decision 0095, spec PR 5: the tool, smoke and unstarted-code variables refuse too, each saying
/// what replaced it — a command-line flag, a credential-store row, a smoke's constant, or a
/// parameter of code no composition root starts. None prints a `config set` line: the five poller
/// flags keep their `flags.*` rows, but nothing reads those rows, so pointing at one would
/// confirm something false.
#[test]
fn the_pr5_variables_refuse_and_say_what_replaced_them() {
    for (var, needle) in [
        ("CTRADER_REDIRECT_URI", "--redirect-uri"),
        ("CTRADER_SCOPE", "--scope"),
        ("CTRADER_TOKEN_FILE", "--token-file"),
        ("CTRADER_CLIENT_ID", "vike-cli secrets set CTRADER_CLIENT_ID"),
        ("CTRADER_CLIENT_SECRET", "vike-cli secrets set CTRADER_CLIENT_SECRET"),
        ("POLY_EGRESS_PROBE_URL", "DEFAULT_EGRESS_PROBE"),
        ("POLY_EXPECT_EGRESS_COUNTRY", "DUBLIN_EGRESS_COUNTRY"),
        ("POLY_CHAIN_WATCH", "nothing starts"),
        ("POLY_CHAIN_RPC_URL", "nothing starts"),
        ("POLY_CHAIN_MAX_SPAN", "nothing starts"),
        ("POLY_CHAIN_PROXY", "nothing starts"),
        ("POLY_AUTO_REDEEM", "nothing starts"),
        ("POLY_REDEEM_HALT", "nothing starts"),
        ("POLY_HEARTBEAT", "nothing starts"),
        ("VIKE_PM_RESOLVE", "nothing starts"),
        ("VIKE_HL_OUTCOME", "nothing starts"),
        ("VIKE_RECORD_DVOL_CADENCE_MS", "nothing starts"),
    ] {
        let err = refuse_removed_env(&env(&[(var, "x")])).unwrap_err();
        assert!(err.contains(&format!("{var} is NO LONGER READ")), "{err}");
        assert!(err.contains(needle), "{var} must say `{needle}`: {err}");
        assert!(!err.contains("vike-cli config set"), "{var} has no settings row: {err}");
        // A row with no key AND no file names no file: its `why` carries the whole answer. The
        // `None if file.is_empty()` arm is what keeps the credential-file sentence out of it.
        assert!(!err.contains("<project>/settings/"), "{var} names no file: {err}");
        assert!(err.contains(&format!("then unset {var}")), "{err}");
    }
}

/// **`POLY_REDEEM_HALT` names the kill switch that stays.** Unsetting the variable releases nothing
/// (nothing starts the poller it halted), and the poller's runtime switch is not a variable: it is
/// the halt FILE its caller hands `AutoRedeemPoller::spawn`, checked every tick. A refusal that
/// told an operator to unset a kill switch and stopped there would read as "your halt is gone".
#[test]
fn the_redeem_halt_refusal_names_the_halt_file_that_stays() {
    for value in ["1", ""] {
        let err = refuse_removed_env(&env(&[("POLY_REDEEM_HALT", value)])).unwrap_err();
        assert!(err.contains("halt FILE"), "{value:?}: {err}");
        assert!(err.contains("AutoRedeemPoller::spawn"), "{value:?}: {err}");
    }
}

/// **The app pair's two refusals read in ONE order.** Each ends with the shared tail "then unset
/// …", so the `why` may not also say "unset first, then write" — an operator reading both would
/// be told to unset twice, in two positions. The store write is said to come AFTER that unset.
#[test]
fn the_app_pair_refusal_orders_its_steps_once() {
    for var in ["CTRADER_CLIENT_ID", "CTRADER_CLIENT_SECRET"] {
        let err = refuse_removed_env(&env(&[(var, "x")])).unwrap_err();
        assert_eq!(err.matches("unset").count(), 2, "{var}: {err}");
        assert!(err.contains("after the unset below"), "{var}: {err}");
        assert!(err.contains(&format!("vike-cli secrets set {var}")), "{var}: {err}");
    }
}

/// A secret-bearing retired variable is refused without printing its value.
#[test]
fn a_retired_secret_bearing_variable_is_never_echoed() {
    for var in ["CTRADER_CLIENT_SECRET", "POLY_CHAIN_RPC_URL"] {
        let err = refuse_removed_env(&env(&[(var, "hunter2-value")])).unwrap_err();
        assert!(err.contains(&format!("{var} is NO LONGER READ")), "{err}");
        assert!(!err.contains("hunter2-value"), "{var}: {err}");
    }
}

/// **An empty `POLY_REDEEM_HALT` refuses too: its old readers tested PRESENCE.** The poller's
/// kill switch and `Flags::apply_env` both halted on the variable existing at all, so
/// `POLY_REDEEM_HALT=` halted exactly as `=1` did. A build that skipped the blank line would drop
/// a halt with no error anywhere — the day something starts the poller, it would run. The
/// refusal says what the blank meant and prints no row, because nothing reads one.
#[test]
fn an_empty_redeem_halt_is_refused_because_presence_halted() {
    for blank in ["", "  "] {
        let err = refuse_removed_env(&env(&[("POLY_REDEEM_HALT", blank)])).unwrap_err();
        assert!(err.contains("POLY_REDEEM_HALT is set to an EMPTY value"), "{blank:?}: {err}");
        assert!(err.contains("POLY_REDEEM_HALT is NO LONGER READ"), "{err}");
        assert!(err.contains("HALTED the auto-redeem poller"), "what the blank meant: {err}");
        assert!(err.contains("nothing starts"), "{err}");
        assert!(!err.contains("vike-cli config set"), "there is no row to write: {err}");
        assert!(err.contains("then unset POLY_REDEEM_HALT"), "{err}");
    }
}

// --- the HALT sentinel's path override (decision 0099) ----------------------------------------

/// Decision 0099: `VIKE_HALT_FILE` refuses startup, and the refusal says where the one sentinel
/// is. A build that merely stopped reading it would send an operator who still has it in a unit to
/// `touch` the path they wrote there — a file nothing watches, which is a dead kill switch that
/// reads exactly like an armed one.
#[test]
fn a_set_halt_file_refuses_startup_and_names_the_one_sentinel() {
    let err =
        refuse_removed_env(&env(&[("VIKE_HALT_FILE", "/srv/vike/elsewhere/HALT")])).unwrap_err();
    assert!(err.contains("VIKE_HALT_FILE is NO LONGER READ"), "{err}");
    assert!(err.contains("<project>/settings/state/HALT"), "the file to touch now: {err}");
    assert!(err.contains("nothing watches"), "why a stale value is a dead switch: {err}");
    assert!(err.contains("then unset VIKE_HALT_FILE"), "{err}");
    // A row with no key to paste does not echo its value (`only_rows_with_a_key_to_paste_echo_their_value`),
    // and no settings home takes its place: the sentinel's location is not configurable at all.
    assert!(!err.contains("/srv/vike/elsewhere/HALT"), "{err}");
    assert!(!err.contains("vike-cli config set"), "{err}");
    assert!(!err.contains("<project>/settings/ is"), "no file is named as a home: {err}");
}

/// The old resolver took a blank value as UNSET and fell through to `<project>/settings/state/HALT`
/// (its `resolve_halt_path` trimmed the value and filtered the empty string), so a blank line says
/// nothing the default does not — it starts, and the roster of blank-refusing rows is unchanged
/// (`every_other_row_still_treats_an_empty_value_as_unset` holds that roster).
#[test]
fn a_blank_halt_file_starts_normally_because_blank_was_unset() {
    let row = REMOVED_ENV
        .iter()
        .find(|r| r.var == "VIKE_HALT_FILE")
        .expect("VIKE_HALT_FILE is a retired variable");
    assert_eq!(row.when_empty, None);
    for blank in ["", "  ", "\t"] {
        assert_eq!(refuse_removed_env(&env(&[("VIKE_HALT_FILE", blank)])), Ok(()), "{blank:?}");
    }
}

// --- the row constructors `REMOVED_ENV` is spelled with -----------------------------------------
//
// Each pinned against the full struct literal a row spelled before the constructors existed, so a
// constructor that quietly changed a default would change every row built with it and fail here
// first, by name.

#[test]
fn moved_builds_the_literal_of_a_row_whose_value_moved() {
    assert_eq!(
        RemovedSetting::moved(
            "var_x",
            "policy.toml",
            "policy.x",
            ValueMap::PositiveNumber,
            "Phase X",
            "why x",
        ),
        RemovedSetting {
            var: "var_x",
            file: "policy.toml",
            key: Some("policy.x"),
            value: ValueMap::PositiveNumber,
            removed_in: "Phase X",
            why: "why x",
            echo_value: false,
            when_empty: None,
        }
    );
}

#[test]
fn dropped_builds_the_literal_of_a_row_with_nothing_to_paste() {
    assert_eq!(
        RemovedSetting::dropped("var_y", "config.toml", "Phase Y", "why y"),
        RemovedSetting {
            var: "var_y",
            file: "config.toml",
            key: None,
            value: ValueMap::Verbatim,
            removed_in: "Phase Y",
            why: "why y",
            echo_value: false,
            when_empty: None,
        }
    );
}

#[test]
fn echoed_sets_echo_value_and_nothing_else() {
    assert_eq!(
        RemovedSetting::moved("var_x", "f", "k", ValueMap::Switch, "r", "w").echoed(),
        RemovedSetting {
            var: "var_x",
            file: "f",
            key: Some("k"),
            value: ValueMap::Switch,
            removed_in: "r",
            why: "w",
            echo_value: true,
            when_empty: None,
        }
    );
}

#[test]
fn empty_means_sets_when_empty_and_nothing_else() {
    let meaning = EmptyMeaning { meant: "connect DIRECT", write: "direct" };
    let expected = RemovedSetting {
        var: "var_x",
        file: "f",
        key: Some("k"),
        value: ValueMap::Stdin,
        removed_in: "r",
        why: "w",
        echo_value: false,
        when_empty: Some(EmptyMeaning { meant: "connect DIRECT", write: "direct" }),
    };
    assert_eq!(
        RemovedSetting::moved("var_x", "f", "k", ValueMap::Stdin, "r", "w").empty_means(meaning),
        expected
    );
    // The two builders touch disjoint fields, so a row may chain them in either order.
    assert_eq!(
        RemovedSetting::moved("var_x", "f", "k", ValueMap::Stdin, "r", "w")
            .echoed()
            .empty_means(meaning),
        RemovedSetting::moved("var_x", "f", "k", ValueMap::Stdin, "r", "w")
            .empty_means(meaning)
            .echoed(),
    );
    assert_eq!(
        RemovedSetting::dropped("var_z", "", "r", "w").empty_means(meaning),
        RemovedSetting {
            var: "var_z",
            file: "",
            key: None,
            value: ValueMap::Verbatim,
            removed_in: "r",
            why: "w",
            echo_value: false,
            when_empty: Some(EmptyMeaning { meant: "connect DIRECT", write: "direct" }),
        }
    );
}
