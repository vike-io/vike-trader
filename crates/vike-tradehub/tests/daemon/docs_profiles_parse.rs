//! Every SHIPPED daemon profile — the runbooks under `docs/ops/` and the `settings/` template —
//! must load through the daemon's OWN parser.
//!
//! # Why this exists — a copy-paste path no compiler ever saw
//!
//! `docs/ops/tradehub-binance-live.toml` and its bybit/okx siblings each open with "Copy to
//! `<project>/settings/tradehub.toml`" — they are not illustrations, they are the exact bytes an
//! operator hands to [`DaemonProfile::load`]. Nothing parsed them in CI, and `deny_unknown_fields`
//! on [`DaemonProfile`] makes one renamed key a FATAL parse error rather than an ignored one — so a
//! field rename lands as a broken runbook with every gate green. Not hypothetical: all three
//! profiles say `symbol = "…"`, a spelling `crates/vike-tradehub/src/config.rs`'s `DaemonProfile`
//! only gained in #1184 — one commit before #1195 shipped the profiles — so any vike-tradehub
//! binary older than that (a deployed release, a stale checkout) refuses every one of them at the
//! first line: `unknown field `symbol``. Whether the docs or the struct moves next, this file makes
//! the disagreement a red PR instead of an operator discovery.
//!
//! # The TEMPLATE is here too, and it is the one an operator reaches first
//!
//! The nine runbooks are a MENU — nine mutually exclusive live mounts, one per venue, each carrying
//! that venue's measured grid. None of them is "the file you start with", and for a container
//! operator none of them is reachable at all: they live in a git checkout the image does not carry.
//! [`TEMPLATE`] is the fifth member of the `settings/` template family and the file every launcher's
//! `--config` actually names once copied, so it is gated by everything below plus one rule of its
//! own ([`every_commented_key_in_the_template_is_one_the_daemon_accepts`]).
//!
//! ⚠ **`crates/vike-cli/tests/settings_examples.rs` does NOT cover it, and cannot.** That gate
//! derives its template list from `vike_config::provenance::setting_keys()` — the four files
//! `vike_config::load` resolves — so a daemon profile is invisible to it in BOTH directions: it
//! neither checks the template nor trips over it. Its central promise
//! (`the_templates_as_shipped_configure_nothing`) is also unavailable here by construction:
//! [`DaemonProfile`] is `deny_unknown_fields` and REQUIRES a mount symbol, so an all-commented
//! daemon profile does not parse. This file is therefore the whole of that template's coverage.
//!
//! # What is asserted, and why it is the strongest set that holds in CI
//!
//! For EVERY `docs/ops/tradehub-*-live.toml` (discovered by listing the directory, so a fourth
//! venue's runbook joins the gate the moment the file exists — [`KNOWN_PROFILES`] is only the
//! floor that keeps the listing from going silently vacuous) AND for [`TEMPLATE`]:
//!
//!   1. [`DaemonProfile::from_toml_str`] succeeds — the same parse-then-validate entry
//!      [`DaemonProfile::load`] wraps, i.e. the operator's own path minus the filesystem read;
//!   2. [`DaemonProfile::validate_for_live`] succeeds — these are LIVE runbooks and that check is
//!      the daemon's live gate, and it is PURE (the [`LIVE_WIRED_VENUES`] allow-list,
//!      `vike_run::WIRED_MARKETS` routing, venue symbol shape, `seed_cash` finiteness — no
//!      credentials, no network, no env), so it holds on the credential-free CI runners.
//!
//! …and for the template alone, a third: every COMMENTED key in it is a key the daemon accepts,
//! proven by uncommenting the lot and putting the result through the same two entries. A template
//! invites exactly that edit, so a line that would be refused once uncommented is a trap the shipped
//! bytes cannot show.
//!
//! Per the repo's mutation-test-every-gate rule, [`the_parser_this_gate_relies_on_can_refuse`]
//! proves the rejection path the gate exists to catch is reachable through this exact entry point:
//! a planted unknown TOP-LEVEL key must fail the parse of each real profile, by name. The
//! uncommenter has its own ([`the_uncommenter_can_actually_fail`]), because a line extractor that
//! silently found nothing would make rule 3 vacuous.

use std::path::{Path, PathBuf};

use vike_tradehub::config::DaemonProfile;

/// The completeness floor: the runbook profiles that MUST exist. Discovery below is by directory
/// listing so new siblings join automatically; this list only stops a rename of the whole family
/// (or of the directory) from turning the gate into a green no-op over zero files.
const KNOWN_PROFILES: &[&str] = &[
    "tradehub-alpaca-live.toml",
    "tradehub-aster-live.toml",
    "tradehub-binance-live.toml",
    "tradehub-bybit-live.toml",
    "tradehub-ctrader-live.toml",
    "tradehub-deribit-live.toml",
    "tradehub-ig-live.toml",
    "tradehub-oanda-live.toml",
    "tradehub-okx-live.toml",
];

/// The shipped daemon-profile TEMPLATE, repo-relative.
///
/// It is committed through `.gitignore`'s `!/settings/*.example.toml` rule — the same one that
/// commits the four `vike_config` templates beside it — and it is the file
/// `deploy/docker/entrypoint.sh --template` emits, so the image and a checkout hand out the same
/// bytes.
const TEMPLATE: &str = "settings/tradehub.example.toml";

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the same idiom as the sibling
/// `live_wired_venues_pin.rs`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `docs/ops/tradehub-*-live.toml`, as `(repo-relative path, contents)`, sorted by path.
fn runbook_profiles() -> Vec<(String, String)> {
    let dir = workspace_root().join("docs").join("ops");
    let mut found: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot list `docs/ops` at {}: {e}", dir.display()))
        .map(|entry| entry.expect("readable docs/ops entry").path())
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.to_string();
            (name.starts_with("tradehub-") && name.ends_with("-live.toml")).then_some((name, path))
        })
        .map(|(name, path)| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read `{}`: {e}", path.display()));
            (format!("docs/ops/{name}"), text)
        })
        .collect();
    found.sort();
    for known in KNOWN_PROFILES {
        assert!(
            found.iter().any(|(path, _)| path == &format!("docs/ops/{known}")),
            "`docs/ops/{known}` is gone — if the runbook family was renamed, move this gate's \
             discovery (and KNOWN_PROFILES) with it rather than letting the listing go vacuous"
        );
    }
    found
}

/// The shipped template, as `(repo-relative path, contents)`.
fn template_profile() -> (String, String) {
    let path = workspace_root().join(TEMPLATE);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the shipped daemon-profile template `{TEMPLATE}` must exist and be readable ({e}). It \
             is committed through `.gitignore`'s `!/settings/*.example.toml` rule, and without it \
             NOTHING in this workspace produces the `<project>/settings/tradehub.toml` that every \
             launcher's `--config` names — which is how a first run reaches `bad profile … No such \
             file or directory`."
        )
    });
    (TEMPLATE.to_string(), text)
}

/// Everything gated here: the nine runbooks and the template, all as `(path, contents)`.
fn shipped_profiles() -> Vec<(String, String)> {
    let mut all = runbook_profiles();
    all.push(template_profile());
    all
}

/// (1)+(2): every shipped profile parses through the daemon's real entry and passes the pure live
/// gate — i.e. the runbook's (and the template's) "copy to `<project>/settings/tradehub.toml`"
/// instruction actually works against THIS tree's `DaemonProfile`.
#[test]
fn every_shipped_daemon_profile_parses_and_passes_the_live_gate() {
    for (path, text) in shipped_profiles() {
        let profile = DaemonProfile::from_toml_str(&text).unwrap_or_else(|e| {
            panic!(
                "`{path}` no longer loads through `DaemonProfile::from_toml_str` — its copy-paste \
                 path is broken: {e}"
            )
        });
        profile.validate_for_live().unwrap_or_else(|e| {
            panic!(
                "`{path}` parses but `DaemonProfile::validate_for_live` refuses it — a shipped \
                 profile must describe a mount the live daemon accepts: {e}"
            )
        });
    }
}

/// (3) — the template's own rule. **Every commented key in it is a key the daemon accepts.**
///
/// A template exists to be edited, and the edit it invites is removing a `#`. So the shipped bytes
/// parsing proves only half of it: a key that was renamed, a value of the wrong type, or a number
/// outside a validated range sits there commented out, green, until an operator uncomments it and
/// the daemon refuses to start. Uncommenting the lot and running the SAME two entries is what makes
/// each of those lines a promise rather than a suggestion — the direction
/// `crates/vike-cli/tests/settings_examples.rs`'s `every_template_line_is_one_the_loader_accepts`
/// covers for the four settings files, reached here through the daemon's parser instead.
///
/// ⚠ The key set is asserted too, not just the parse. An extractor that matched nothing would make
/// this test a second, slower copy of the one above.
#[test]
fn every_commented_key_in_the_template_is_one_the_daemon_accepts() {
    let (path, text) = template_profile();

    let keys = commented_keys(&text);
    assert!(
        keys.len() >= 4,
        "`{path}` has {} commented key line(s); the uncommenter found next to nothing, so this \
         test proves next to nothing. Either the template stopped documenting its optional keys or \
         `commented_keys`' grammar no longer matches how they are written. found: {keys:?}",
        keys.len()
    );

    let live = uncommented(&text);
    let profile = DaemonProfile::from_toml_str(&live).unwrap_or_else(|e| {
        panic!(
            "`{path}` does not parse once its commented keys are uncommented — which is exactly \
             what a template invites somebody to do, and they would meet this as `bad profile`. \
             Fix the offending line or delete it; a key that cannot be enabled must not be listed. \
             keys: {keys:?}, error: {e}"
        )
    });
    profile.validate_for_live().unwrap_or_else(|e| {
        panic!(
            "`{path}` parses uncommented but `DaemonProfile::validate_for_live` refuses the \
             result — a line whose value is outside a validated range is a trap the shipped \
             (commented) bytes cannot show. keys: {keys:?}, error: {e}"
        )
    });
}

/// The mutation self-test: `deny_unknown_fields` rejection — the exact drift shape this gate
/// exists for — is reachable through the same entry the assertions above use. A planted unknown
/// top-level key (prepended, so it cannot land inside the trailing `[daemon]` table and test the
/// wrong struct) must fail each REAL profile's parse, and the error must name the key.
#[test]
fn the_parser_this_gate_relies_on_can_refuse() {
    for (path, text) in shipped_profiles() {
        let planted = format!("docs_profiles_parse_gate_selftest_key = 1\n{text}");
        let err = DaemonProfile::from_toml_str(&planted).expect_err(&format!(
            "an unknown top-level key planted into `{path}` PARSED — `deny_unknown_fields` no \
             longer guards `DaemonProfile`, so this whole gate is vacuous"
        ));
        assert!(
            err.contains("docs_profiles_parse_gate_selftest_key"),
            "the refusal for `{path}` does not name the offending key; an operator debugging a \
             broken profile needs the key name. error: {err}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The uncommenter — the template's third rule rests entirely on these two functions
// ---------------------------------------------------------------------------------------------

/// A commented SETTING line's live form: `# key = value` or `# [table]`, at column 0, exactly one
/// space after the `#`.
///
/// ⚠ Deliberately narrow, and the narrowness is what keeps it honest. The template is mostly PROSE
/// in `#` comments, and a looser rule would uncomment a sentence into the TOML. Three things are
/// required: the `#` sits at column 0 (an indented `#` is a continuation line — the run recipes at
/// the top of the template are indented for exactly this reason), it is followed by exactly one
/// space, and what remains is either a bare-identifier assignment or a lone `[table]` header.
///
/// The `[table]` arm exists because `[daemon]` is a commented key line that carries no `=`; without
/// it, uncommenting `summary_ms` would hoist two `[daemon]` fields to the top level and the test
/// would fail for the extractor's reason rather than the template's.
fn commented_line(line: &str) -> Option<(String, String)> {
    let live = line.strip_prefix("# ")?;
    if live.starts_with(' ') || live.starts_with('\t') {
        return None;
    }
    let trimmed = live.trim_end();
    if let Some(table) = trimmed.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        return is_ident(table).then(|| (format!("[{table}]"), trimmed.to_string()));
    }
    let (lhs, _) = trimmed.split_once('=')?;
    let key = lhs.trim_end();
    is_ident(key).then(|| (key.to_string(), trimmed.to_string()))
}

/// A bare TOML key: `[A-Za-z_][A-Za-z0-9_.-]*`.
fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Every key the template mentions in a commented line, in order.
fn commented_keys(body: &str) -> Vec<String> {
    body.lines().filter_map(commented_line).map(|(key, _)| key).collect()
}

/// The template with every commented setting line uncommented — what an operator gets by stripping
/// the `#`s, which is exactly what a template invites.
fn uncommented(body: &str) -> String {
    body.lines()
        .map(|line| match commented_line(line) {
            Some((_, live)) => live,
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The uncommenter's mutation self-test: it must say YES to the shapes the template uses and NO to
/// the prose it is surrounded by. Without this, rule 3 could pass by finding nothing and
/// uncommenting nothing — the vacuous-gate failure this repository has already had three times.
#[test]
fn the_uncommenter_can_actually_fail() {
    // YES — the two shapes the template actually uses.
    assert_eq!(
        commented_line("# tick_size = 0.01"),
        Some(("tick_size".to_string(), "tick_size = 0.01".to_string()))
    );
    assert_eq!(
        commented_line("# [daemon]"),
        Some(("[daemon]".to_string(), "[daemon]".to_string()))
    );
    // …and an aligned assignment, which is how this template writes its pairs.
    assert_eq!(commented_line("# interval    = \"1m\"").map(|(k, _)| k), Some("interval".into()));

    // NO — prose, in every shape the template's header actually contains.
    assert_eq!(
        commented_line("# So the two keys below are LIVE and every other key is not."),
        None
    );
    assert_eq!(commented_line("#     cp settings/x.toml settings/y.toml"), None, "indented recipe");
    assert_eq!(commented_line("# `deny_unknown_fields` = the trap"), None, "backticked prose");
    // ⚠ A REAL near-miss from the shipped template — a prose line that genuinely contains `=` at
    // column 0 after the `# `. It is rejected because the text left of the `=` is not a bare
    // identifier, which is the whole reason the grammar checks that rather than just splitting.
    assert_eq!(
        commented_line(
            "# LAUNCHER's stop budget in the same breath (`TimeoutStopSec=` in the unit, ...)"
        ),
        None,
        "prose containing an `=` must not be uncommented into the TOML"
    );
    assert_eq!(commented_line("venue = \"binance\""), None, "a LIVE line is not a commented one");
    assert_eq!(commented_line("#no space after the hash = 1"), None);

    // …and the whole-body transform is a no-op on prose while doing its job on a key line.
    let body = "# a sentence\nvenue = \"binance\"\n# qty = 1.0\n";
    assert_eq!(uncommented(body), "# a sentence\nvenue = \"binance\"\nqty = 1.0");
    assert_eq!(commented_keys(body), vec!["qty".to_string()]);
}
