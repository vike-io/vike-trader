//! `set`, the ONE writer, asserted over the shipped binary: streams, exit rung, bytes on disk.

use super::support::{SetCase, canon, exit_code, rung};
use super::{stderr, stdout};

// ── set — the ONE writer ────────────────────────────────────────────────────────────────────────
//
// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` fixed this
// command's shape before it was built; these cases are that shape asserted over the SHIPPED binary,
// which is the only place several of the properties are visible at all (the exit RUNG, the two
// streams, and the bytes on disk afterwards).
//
// ⚠ Every case drives its own temp settings directory through `$VIKE_SETTINGS_DIR`, so the store
// written is the case's own and never the developer box's — the same isolation the reading cases
// above take through `--file`, but reached the other way round, because `set` also has to resolve
// the change journal that hangs off that same directory.

/// The store every `set` case starts from: comments, a blank line, an unrelated venue, and one key
/// the replace case targets. Every byte of it that a write does not name must survive.
const SET_STORE: &str = "# vike credential store\n\
                         # hand-written, and it stays hand-written\n\
                         \n\
                         BINANCE_LIVE_API_KEY=old-key-value\n\
                         OKX_DEMO_API_PASSPHRASE=\"quoted pass\"\n";

/// **The stdin form: the key is APPENDED, every other byte survives, the value reaches neither
/// stream, and the exit is clean.**
///
/// The byte-for-byte assertion is the point. `vike_secrets::save_credentials_to_store` is the
/// workspace's one upsert precisely because the store is the user's only copy of live venue keys,
/// and a CLI writer is a third surface that property has to hold on — reason 1 in
/// `docs/decisions/0036`.
#[test]
fn set_from_stdin_appends_the_key_and_preserves_every_other_byte() {
    let c = SetCase::new("stdin");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "BYBIT_DEMO_API_KEY"], Some("piped-secret-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let after = c.read_store();
    // The store is the ORIGINAL rows plus exactly one.
    assert_eq!(
        after,
        canon(&format!("{SET_STORE}BYBIT_DEMO_API_KEY=piped-secret-value\n")),
        "a set must append one line and touch nothing else"
    );
    // The trailing newline the shell put on the pipe is NOT part of the credential.
    assert!(after.contains("BYBIT_DEMO_API_KEY=piped-secret-value\n"), "{after}");

    // The report names the key and the file, and says which of the two things happened.
    let text = stdout(&out);
    assert!(text.contains("BYBIT_DEMO_API_KEY"), "{text}");
    assert!(text.contains("appended"), "{text}");
    assert!(text.contains(&c.store().display().to_string()), "{text}");

    // …and the VALUE is on neither stream. This is the assertion the whole command is shaped
    // around: a writer that echoed what it wrote would put the credential in the scrollback of
    // every session that used it.
    assert!(!text.contains("piped-secret-value"), "a VALUE reached stdout: {text}");
    assert!(!stderr(&out).contains("piped-secret-value"), "a VALUE reached stderr");
}

/// **The `--from-env` form**, taking the value out of the map the dispatcher swept — the second and
/// last accepted source, and the one a deploy script uses.
#[test]
fn set_from_env_takes_the_named_variable() {
    let c = SetCase::new("from-env");
    c.store_rows(SET_STORE);

    let out = c.run(
        &["set", "BYBIT_DEMO_API_SECRET", "--from-env", "VIKE_TEST_SECRET_SOURCE"],
        None,
        &[("VIKE_TEST_SECRET_SOURCE", "env-sourced-value")],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert_eq!(
        c.read_store(),
        canon(&format!("{SET_STORE}BYBIT_DEMO_API_SECRET=env-sourced-value\n"))
    );
    assert!(!stdout(&out).contains("env-sourced-value"), "a VALUE reached stdout");
    assert!(!stderr(&out).contains("env-sourced-value"), "a VALUE reached stderr");

    // An UNSET variable is a usage error naming the variable, and writes nothing.
    let out = c.run(&["set", "BYBIT_DEMO_API_KEY", "--from-env", "VIKE_TEST_NOT_SET"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("VIKE_TEST_NOT_SET"), "{}", stderr(&out));
    assert!(!c.read_store().contains("BYBIT_DEMO_API_KEY"), "nothing may be written");
}

/// **A REPLACE changes exactly one line, in place**, and leaves the old value nowhere in the file.
#[test]
fn set_replaces_an_existing_key_in_place_and_says_so() {
    let c = SetCase::new("replace");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("rotated-key-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("replaced"), "{}", stdout(&out));

    let after = c.read_store();
    assert_eq!(
        after,
        canon(&SET_STORE.replace(
            "BINANCE_LIVE_API_KEY=old-key-value",
            "BINANCE_LIVE_API_KEY=rotated-key-value"
        )),
        "a replace changes exactly one row"
    );
    assert!(!after.contains("old-key-value"), "the old value must not survive");
}

/// **A VALUE IN ARGV IS A USAGE ERROR, and the refusal quotes nothing.**
///
/// Reason 2 in `docs/decisions/0036`: a CLI is the surface people SCRIPT, and a value in argv lands
/// in shell history and in `ps` output for every user on the box. The last assertion is why the
/// message is a constant rather than a `format!` — a refusal that echoed the token would write the
/// credential into the very scrollback it exists to keep it out of.
#[test]
fn a_value_in_argv_is_refused_on_the_usage_rung_and_never_echoed() {
    let c = SetCase::new("argv");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY", "sk-live-never-print-me"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("may not be given on the command line"), "{err}");
    assert!(err.contains("shell history"), "the refusal must say WHY: {err}");
    assert!(err.contains("--from-env") && err.contains("stdin"), "both forms: {err}");
    assert!(!err.contains("sk-live-never-print-me"), "the refusal ECHOED the value: {err}");
    assert!(!stdout(&out).contains("sk-live-never-print-me"), "the refusal ECHOED the value");

    // …and the store is untouched.
    assert_eq!(c.read_store(), canon(SET_STORE));
}

/// **An unknown key is refused BY NAME on the usage rung**, with the nearest real names.
///
/// Reason 3 in `docs/decisions/0036`: the store is a flat `KEY=VALUE` file, so a writer that
/// accepted any name would write `BINANCE_LIVE_API_KEY_` as happily as the real key — and the venue
/// would then stay on paper with no error anywhere.
#[test]
fn an_unknown_key_is_refused_by_name_and_writes_nothing() {
    let c = SetCase::new("unknown-key");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY_"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("BINANCE_LIVE_API_KEY_"), "the refusal must name the key: {err}");
    assert!(err.contains("BINANCE_LIVE_API_KEY"), "…and suggest the real one: {err}");
    assert_eq!(c.read_store(), canon(SET_STORE), "a refused key must write nothing");
    assert!(!c.read_store().contains("never-written"));
}

/// **Decision 0095: a venue SETTING's credential-style name is refused, naming the `config set`
/// line** — a credential row under it is read by nothing — and that holds when the store already
/// holds a row under it too.
#[test]
fn a_venue_settings_credential_style_name_is_refused() {
    let c = SetCase::new("stranded-setting");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "POLY_PROXY_HOST"], Some("<host>\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("POLY_PROXY_HOST"), "the refusal names the key: {err}");
    assert!(
        err.contains("vike-cli config set venue.polymarket.proxy_host"),
        "…and the line that writes the setting: {err}"
    );
    assert!(!err.contains("<host>"), "the value was not even read: {err}");
    assert_eq!(c.read_store(), canon(SET_STORE), "a refused key must write nothing");

    // A SECRET setting names the stdin form.
    let out = c.run(&["set", "POLY_SOCKS_PROXY"], Some("socks5h://u:p@h:1\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("venue.polymarket.socks_proxy -"), "{}", stderr(&out));

    // A row the store already holds under such a name is refused too: nothing reads it, so
    // rotating it would write a value nothing loads.
    let held = format!("{SET_STORE}IBKR_DEMO_HOST=<host>\n");
    c.store_rows(&held);
    let out = c.run(&["set", "IBKR_DEMO_HOST"], Some("<host>\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("config set venue.ibkr.demo.host"), "{}", stderr(&out));
    assert!(c.read_store().contains("IBKR_DEMO_HOST=<host>"), "{}", c.read_store());
}

/// **A key this workspace READS is refused WITHOUT being called dead**, over the shipped binary.
///
/// `vike-cli secrets set VIKE_TRADEHUB_OBSERVE_KEY` used to answer "is not a credential key this
/// workspace reads, so setting it would write a line nothing would ever load". Measured on the CI box,
/// 2026-09-07, and false: `crates/vike-tradehub/src/node.rs`'s `start_observe_server` reads
/// that exact name out of the credential map, and the escape-hatch paragraph beneath it pointed at
/// per-bridge config loaders, which a node key does not have. The operator was told a key they had
/// just configured was inert, and given no route at all.
///
/// The REFUSAL is unchanged and deliberately so. ⚠ Its REASON is not, and the old one is stated
/// here only to be retired: this said `set` writes "the enumerable grid and nothing wider", which
/// stopped being true when `settable_outside_the_grid` admitted every name the settings registry
/// proves a reader for. A node key stays refused on its own merits — it is MINTED, and a
/// hand-pasted 256-bit HMAC that is truncated fails as an opaque auth denial — which is the narrower
/// and more durable argument. `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`
/// fences the surface and this test must not be read as reopening it. What is asserted here is that the refusal
/// tells the truth, names a reader, and points at the route that exists today. The unit twin
/// (`the_node_keys_refusal_names_the_command_that_mints_them`) covers the whole set of such names
/// and the property over the registry; this one covers the two things only the real binary shows —
/// the RUNG and the STREAM.
///
/// ⚠ **The ROUTE half of this test INVERTED, and the inversion is the point.** It used to assert the
/// message named `vike-cli secrets path` and did NOT name `vike-cli node setup` — the verb this
/// command carried then — with the reason written beside it:
/// *"a node-key GENERATOR is designed and NOT BUILT, and naming one in a refusal
/// would be this same defect one step on — a route that fails at the terminal instead of a key that
/// loads nothing."* That was right while it was true. The generator now exists, so the editor route
/// is the stale answer and the command is the live one; the test's real invariant — **a refusal
/// names the route that exists TODAY, and never one that does not** — is unchanged, and it is what
/// both versions assert.
#[test]
fn a_key_the_workspace_reads_is_refused_without_being_called_unloadable() {
    let c = SetCase::new("read-elsewhere");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "VIKE_TRADEHUB_OBSERVE_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "the refusal must name the key: {err}");
    assert!(!err.contains("nothing would ever load"), "the measured lie is back: {err}");
    assert!(err.contains("IS read by this workspace"), "{err}");
    // The route that exists today, and which BOX to run it on — a node key is minted on the
    // daemon's box and carried to a client, so a command with no box named is half an instruction.
    assert!(err.contains("backend setup"), "the operator must be given a route: {err}");
    assert!(err.contains("backend connect"), "…including the client's half: {err}");
    // ⚠ WHICH BOX. This asserted the literal "DAEMON's box", which was an answer while there was
    // ONE daemon and stopped being one when `vike-datahub` grew a pair of its own. The message names
    // the SERVICE now, which is the thing an operator has to get right.
    assert!(err.contains("vike-tradehub"), "which box, by service: {err}");
    // …and it must not send anybody to an editor to invent a 256-bit HMAC key, which is exactly the
    // step `backend setup` exists to delete.
    assert!(!err.contains("EDITOR"), "the stale route is back: {err}");
    assert_eq!(c.read_store(), canon(SET_STORE), "a refused key must write nothing");
    assert!(!c.read_store().contains("never-written"));

    // ⚠ THE DATAHUB PAIR, AND THE INVARIANT THIS TEST'S OWN DOC STATES: a refusal names the route
    // that exists TODAY and never one that does not. `vike-cli datahub setup` is built;
    // `datahub connect` is NOT, and an earlier draft of this arm promised it — the same defect the
    // doc above records paying for once already, one service over.
    let out = c.run(&["set", "VIKE_DATAHUB_CONTROL_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("VIKE_DATAHUB_CONTROL_KEY"), "{err}");
    assert!(err.contains("datahub setup"), "the datahub's own minting command: {err}");
    assert!(err.contains("vike-datahub"), "and its box: {err}");
    assert!(
        !err.contains("backend setup"),
        "it must not route a datahub key at the tradehub: {err}"
    );
    assert!(
        !err.contains("datahub connect"),
        "there is no such command — a refusal may not invent one: {err}"
    );
    assert!(!err.contains("EDITOR"), "{err}");
    assert_eq!(c.read_store(), canon(SET_STORE), "a refused key must write nothing");
}

/// **An ABSENT store is refused, and the refusal names the command that creates one.**
///
/// This command upserts and creates nothing: `vike-cli secrets init` is the one creator.
/// The rung is FAILED rather than USAGE — the command line was fine, the box is not configured —
/// and that distinction is the whole reason the ladder exists.
///
/// Named `an_absent_store_is_refused_and_names_the_template_command` until 2026-10-07, when the
/// `secrets template` verb went with the credential FILE store it seeded; the refusal names the one
/// creator that is left.
#[test]
fn an_absent_store_is_refused_and_names_migrate_init() {
    let c = SetCase::new("absent");
    // Deliberately NO store written.

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Failed), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("no credential store"), "{err}");
    assert!(
        err.contains("secrets init"),
        "…and the database-era way to make the EMPTY store, which creates no blank template \
         rows: {err}"
    );
    assert!(err.contains(&c.store().display().to_string()), "…and which store: {err}");
    assert!(!c.store().exists(), "a refused set must CREATE no store");
}

/// **The write is JOURNALLED: one `credential_write` record, carrying the key NAME and no value.**
///
/// The ledger is the durable answer to *when did this credential last change* —
/// `vike_model::change_journal`'s module doc measures the `tracing` alternative as deleted within
/// days and, on the the CI box daemon, never written at all. The value assertion is over the RAW LINE
/// rather than over a field: a record type that grew a value cell would pass a field-level check
/// while doing exactly the thing that must be impossible.
#[test]
fn the_write_is_journalled_once_with_the_key_name_and_no_value() {
    let c = SetCase::new("journal");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "BYBIT_DEMO_API_KEY"], Some("journalled-secret-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let lines = c.journal_lines();
    assert_eq!(lines.len(), 1, "ONE write ⇒ ONE record: {lines:?}");
    let line = &lines[0];
    assert!(line.contains("BYBIT_DEMO_API_KEY"), "the record must carry the key NAME: {line}");
    assert!(line.contains("bybit"), "…and the venue it belongs to: {line}");
    assert!(line.contains("vike.db"), "…and which store — the settings database: {line}");
    assert!(line.contains("vike-cli"), "…and that the CLI was the actor: {line}");
    assert!(!line.contains("journalled-secret-value"), "a VALUE reached the ledger: {line}");

    // A REFUSED set records nothing — the ledger says what happened, not what was attempted.
    let out = c.run(&["set", "NOT_A_REAL_KEY"], Some("x\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert_eq!(c.journal_lines().len(), 1, "a refusal must append no record");
}

/// **No value on stdin is a usage error**, not an empty credential written over a real one.
///
/// An empty value is equivalent to an absent key — the venue stays on paper — so writing one would
/// report success for a change that arms nothing.
#[test]
fn an_empty_value_is_refused_rather_than_written() {
    let c = SetCase::new("empty");
    c.store_rows(SET_STORE);

    // Closed stdin — what a shell hands a process with no pipe.
    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("no value on stdin"), "{}", stderr(&out));

    // …and whitespace is not a value either.
    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("   \n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));

    assert_eq!(c.read_store(), canon(SET_STORE), "the existing credential must survive both");
}

/// **A value that BEGINS WITH A DASH is refused like any other argv value — and not echoed.**
///
/// The sibling case above proves the property on a value starting with a letter, which is the only
/// class it ever tested. A dash-leading token missed the positional arm's guard and fell through to
/// the generic `unknown option '{other}'`, which printed the credential verbatim to STDERR — the
/// stream CI logs and every service manager captures. base64url alphabets contain `-`, so this is
/// an ordinary credential rather than a contrived one, and the refusal was doing the exact damage
/// it exists to prevent.
#[test]
fn a_dash_leading_value_in_argv_is_refused_and_never_echoed() {
    let c = SetCase::new("argv-dash");
    c.store_rows(SET_STORE);

    for value in ["-sk-live-never-print-me", "-----BEGIN-never-print-me"] {
        let out = c.run(&["set", "BINANCE_LIVE_API_KEY", value], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
        assert!(
            !stderr(&out).contains("never-print-me") && !stdout(&out).contains("never-print-me"),
            "the refusal ECHOED the value: {} / {}",
            stderr(&out),
            stdout(&out)
        );
    }
    assert_eq!(c.read_store(), canon(SET_STORE), "a refused value must write nothing");
}

/// **`--file` cannot aim a WRITE at a path the operator names.**
///
/// It is an inspection flag — the three reading subcommands keep it — and it used to resolve the
/// same way for `set`, so `set KEY --file ~/.bashrc` appended a live credential to a shell rc file
/// and exited 0, with the change journal recording the write against a "store" named `bashrc`.
/// `docs/decisions/0036` fixes this verb as an upsert into the PROJECT's store; a scripted run that
/// must aim elsewhere moves the whole settings directory with `$VIKE_SETTINGS_DIR`.
#[test]
fn set_refuses_to_write_into_a_file_named_on_the_command_line() {
    let c = SetCase::new("file-flag");
    c.store_rows(SET_STORE);
    // An ordinary, non-credential file that happens to exist — the class this defect reached.
    let bystander = c.dir().join("bashrc");
    let bystander_text = "export PATH=/usr/local/bin\n";
    std::fs::write(&bystander, bystander_text).unwrap();

    let out = c.run(
        &["set", "BINANCE_LIVE_API_KEY", "--file", &bystander.display().to_string()],
        Some("sk-written-here\n"),
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("--file was removed"), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&bystander).unwrap(),
        bystander_text,
        "a file named on the command line must not be written"
    );
    assert_eq!(c.read_store(), canon(SET_STORE), "…and neither is the real store");
    assert!(c.journal_lines().is_empty(), "a refusal records nothing");
}

/// **A MULTI-LINE `--from-env` value is refused, and nothing is written.**
///
/// The blocker this case exists for: the value was taken verbatim, the credential FILE store's writer
/// quoted it (a newline is whitespace) and joined with a newline, so the value's own break became a
/// physical line break — and the file reader then read the first half as a SILENTLY TRUNCATED
/// credential and the second half as a WHOLE NEW `KEY=VALUE` for a venue the operator never
/// configured. Exit 0, "appended", `Outcome::Applied` in the ledger. `--from-env` is the
/// CI/deploy-script form and a multi-line secret is the ordinary shape of a Vault- or
/// Actions-injected variable.
///
/// The `secrets list` assertion is the end-to-end half: it is what showed the injected key as a
/// third credential and a third ACCOUNT when this was reproduced.
#[test]
fn a_multiline_env_value_cannot_inject_a_second_key() {
    let c = SetCase::new("from-env-multiline");
    c.store_rows(SET_STORE);

    for raw in ["abc\nOKX_LIVE_API_SECRET=injected-by-a-newline", "trailing-newline\n"] {
        let out = c.run(
            &["set", "BYBIT_DEMO_API_KEY", "--from-env", "VIKE_TEST_MULTILINE"],
            None,
            &[("VIKE_TEST_MULTILINE", raw)],
        );
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("more than one line"), "{err}");
        assert!(err.contains("VIKE_TEST_MULTILINE"), "the refusal must name the VARIABLE: {err}");
        assert!(!err.contains("injected-by-a-newline"), "the refusal ECHOED the value: {err}");
    }

    assert_eq!(c.read_store(), canon(SET_STORE), "a refused value must write nothing at all");
    assert!(c.journal_lines().is_empty(), "…and record nothing");

    // …and the store still holds exactly what it held: no third key, no third account.
    let out = c.run(&["list"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(!stdout(&out).contains("OKX_LIVE_API_SECRET"), "a key was injected: {}", stdout(&out));
}

/// **A DUPLICATED key is rotated on EVERY line, because the reader is LAST-wins.**
///
/// The writer replaced the FIRST matching line and preserved the rest verbatim, while
/// the file reader inserted per line — so the value every loader in the workspace read
/// was the stale one further down the file. The command printed "replaced", exited 0 and journalled
/// `Applied` for a rotation that changed nothing that is read; the daemon kept signing with the old
/// key. Duplicates arrive from ordinary hand-editing and from `secrets template >>` (the append typo
/// of the documented `>` form), which duplicates the entire grid.
#[test]
fn a_duplicated_key_is_rotated_on_every_line_the_reader_might_return() {
    let c = SetCase::new("duplicate-key");
    let store = "# hand-edited twice\n\
                 BINANCE_LIVE_API_KEY=first-old-value\n\
                 OKX_DEMO_API_PASSPHRASE=\"quoted pass\"\n\
                 BINANCE_LIVE_API_KEY=second-old-value\n";
    c.store_rows(store);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY"], Some("rotated-new-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("replaced"), "{}", stdout(&out));

    let text = c.read_store();
    assert!(!text.contains("first-old-value"), "the first occurrence must be rotated: {text}");
    assert!(
        !text.contains("second-old-value"),
        "the LAST occurrence is the one the reader returns, so it must be rotated too: {text}"
    );
    assert!(text.contains("OKX_DEMO_API_PASSPHRASE=quoted pass"), "unnamed keys survive");
    // The carry is LAST-wins, so the store held ONE row for the duplicated name; it is rotated.
    let expected =
        "BINANCE_LIVE_API_KEY=rotated-new-value\nOKX_DEMO_API_PASSPHRASE=\"quoted pass\"\n";
    assert_eq!(text, canon(expected));

    // The end-to-end half: `list` reads the store through the same parser every loader does, so
    // what it returns is what the box would sign with.
    let out = c.run(&["list"], None, &[]);
    assert!(!stdout(&out).contains("second-old-value"), "{}", stdout(&out));
}

/// **A LABELLED ACCOUNT is WRITTEN — under its own name, and the DEFAULT account's key is left
/// exactly as it was.** Over the shipped binary, because the defect this replaces was a two-command
/// sequence an operator performed, not a sentence.
///
/// `KEY__LABEL` names a second account — a name `vike_model::accounts::account_keys` parses,
/// `vike_bridge_core::credentials::load_credentials_for_account` reads, and `secrets list` prints.
/// `set` REFUSED it (the grid is a fixed enumeration; a label is unbounded) and sent the operator to
/// an editor — which, on a migrated box, edits a file `docs/decisions/0054`'s credential half means
/// no reader opens. So a credential the operator could SEE listed had no writer anywhere.
///
/// ⚠ **The hazard that refusal was written about is NOT the unboundedness** and does not go with
/// it: the suggestion list scored the UNLABELLED base as the nearest name and offered it first —
/// real, settable, and a DIFFERENT ACCOUNT — so the operator's obvious next command overwrote the
/// credential their primary account signs with, exit 0, "replaced". Writing the labelled name is
/// what ends that: the key typed is the key written, and this test asserts the base is untouched
/// byte for byte, which is the property the old refusal was only ever a proxy for.
#[test]
fn a_labelled_account_is_written_and_the_base_key_is_untouched() {
    let c = SetCase::new("labelled-account");
    c.store_rows(SET_STORE);

    let out = c.run(&["set", "BINANCE_LIVE_API_KEY__ALT"], Some("alt-account-key\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    // ⚠ THE HALF THAT MATTERS, and it is asserted as an EXACT append rather than a `contains`: the
    // store is the original text plus exactly one line, so `BINANCE_LIVE_API_KEY=old-key-value` —
    // the DEFAULT account's, the one the old suggestion invited an operator to clobber — is still
    // byte-for-byte what it was.
    assert_eq!(
        c.read_store(),
        canon(&format!("{SET_STORE}BINANCE_LIVE_API_KEY__ALT=alt-account-key\n")),
        "the labelled key is appended and nothing else moves"
    );
    // …and the value never reached a stream.
    assert!(!stderr(&out).contains("alt-account-key"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("alt-account-key"), "{}", stdout(&out));
}

// ── the CLOSED list outside the grid ─────────────────────────────────────────────────────────────

/// **Every name of `vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID` is written into a store
/// that does NOT already hold it** — the gap the list closes: before it, the pager token, the
/// collector key and their siblings reached `set` only by ROTATION, so a box with a database and
/// no copy had no writer at all. Each write is journalled by NAME, and no value reaches either
/// stream or the ledger.
#[test]
fn every_listed_name_is_appended_into_a_store_that_never_held_it() {
    let c = SetCase::new("closed-list");
    c.store_rows(SET_STORE);
    let list = vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID;
    for (i, (name, _)) in list.iter().enumerate() {
        let value = format!("closed-list-value-{i}");
        let out = c.run(&["set", name], Some(&format!("{value}\n")), &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{name}: {}", stderr(&out));
        assert!(stdout(&out).contains("appended"), "{name}: {}", stdout(&out));
        assert!(!stdout(&out).contains(&value) && !stderr(&out).contains(&value), "{name}: VALUE");
        assert!(c.read_store().contains(&format!("{name}={value}\n")), "{name} did not land");
    }
    let listed = c.run(&["list"], None, &[]);
    for (name, _) in list {
        assert!(stdout(&listed).contains(name), "`list` must show {name}");
    }
    let writes: Vec<String> =
        c.journal_lines().into_iter().filter(|l| l.contains("credential_write")).collect();
    assert_eq!(writes.len(), list.len(), "one record per write");
    for (i, ((name, _), line)) in list.iter().zip(&writes).enumerate() {
        assert!(line.contains(name), "the record must name {name}");
        assert!(!line.contains(&format!("closed-list-value-{i}")), "a VALUE reached the ledger");
    }
}

/// **The Studio chat pane's two provider keys are written into an EXISTING store that never held
/// them** — the false refusal this pins closed. `vike-cli secrets set ANTHROPIC_API_KEY` and
/// `CEREBRAS_API_KEY` were refused as "not read by the workspace" while the desktop's chat pane read
/// both out of the credential map. The value travels on stdin (never argv), each write APPENDS one
/// line and moves nothing else, and neither stream, the listing's values, nor the ledger carries it.
#[test]
fn the_studio_chat_keys_are_appended_into_a_store_that_never_held_them() {
    let c = SetCase::new("studio-chat-keys");
    c.store_rows(SET_STORE);
    let anthropic = concat!("ANTHROPIC", "_API_KEY");
    let cerebras = concat!("CEREBRAS", "_API_KEY");
    let mut expected = SET_STORE.to_string();
    for (name, value) in [(anthropic, "sk-ant-studio-value-1"), (cerebras, "csk-studio-value-2")] {
        let out = c.run(&["set", name], Some(&format!("{value}\n")), &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{name}: {}", stderr(&out));
        assert!(stdout(&out).contains("appended"), "{name}: {}", stdout(&out));
        assert!(stdout(&out).contains(name), "the report names the key: {}", stdout(&out));
        assert!(!stdout(&out).contains(value), "{name}: a VALUE reached stdout");
        assert!(!stderr(&out).contains(value), "{name}: a VALUE reached stderr");
        expected.push_str(&format!("{name}={value}\n"));
        assert_eq!(c.read_store(), canon(&expected), "{name}: one line, nothing else");
    }
    let listed = c.run(&["list"], None, &[]);
    assert!(stdout(&listed).contains(anthropic) && stdout(&listed).contains(cerebras));
    for value in ["sk-ant-studio-value-1", "csk-studio-value-2"] {
        assert!(!stdout(&listed).contains(value), "`list` printed a VALUE");
        assert!(c.journal_lines().iter().all(|l| !l.contains(value)), "a VALUE reached the ledger");
    }
}

/// **A registry name OUTSIDE the closed list is still refused, BY NAME, and writes nothing** — a
/// SETTING read off the process environment, and a typo of a listed name, which is offered the
/// real spelling.
#[test]
fn a_name_outside_the_closed_list_is_still_refused_by_name() {
    let c = SetCase::new("closed-list-out");
    c.store_rows(SET_STORE);
    let compute = format!("VIKE_{}", "STUDIO_COMPUTE_KEY");
    let out = c.run(&["set", &compute], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&compute) && stderr(&out).contains("SETTING"),
        "{}",
        stderr(&out)
    );

    let typo = format!("VIKE_{}", "ALERT_TELEGRAM_TOKN");
    let out = c.run(&["set", &typo], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(&typo), "{err}");
    assert!(err.contains(&format!("did you mean: VIKE_{}", "ALERT_TELEGRAM_TOKEN")), "{err}");

    assert_eq!(c.read_store(), canon(SET_STORE), "a refused key must write nothing");
    assert!(
        c.journal_lines().iter().all(|l| !l.contains("credential_write")),
        "a refusal journalled"
    );
}
