//! Every SHIPPED daemon profile — the runbooks under `docs/ops/` — must load through the daemon's
//! OWN parser.
//!
//! # Why this exists — a copy-paste path no compiler ever saw
//!
//! `docs/ops/tradehub-binance-live.toml` and its bybit/okx siblings each carry a mount an operator
//! can start a daemon from — they are not illustrations, they are exact bytes a `DaemonProfile`
//! parses. Nothing parsed them in CI, and `deny_unknown_fields` on [`DaemonProfile`] makes one
//! renamed key a FATAL parse error rather than an ignored one — so a field rename lands as a broken
//! runbook with every gate green. Not hypothetical: all three profiles say `symbol = "…"`, a
//! spelling `crates/vike-tradehub/src/config.rs`'s `DaemonProfile` only gained in #1184 — one commit
//! before #1195 shipped the profiles — so any vike-tradehub binary older than that (a deployed
//! release, a stale checkout) refuses every one of them at the first line: `unknown field
//! `symbol``. Whether the docs or the struct moves next, this file makes the disagreement a red PR
//! instead of an operator discovery.
//!
//! ⚠ **The TEMPLATE half of this file is GONE, and the history is worth carrying.** Until decision
//! 0086 ("settings live only in the database") this gate ALSO covered `settings/tradehub.example.toml`
//! — the fifth member of the `settings/` template family, and the file every launcher's `--config`
//! named once copied — plus its own third rule (every COMMENTED key in it a key the daemon accepts,
//! proven by uncommenting the lot). 0086 verdict 1 forbids reading a profile TOML from any binary, so
//! that template — and the `--config <path>` argument it fed — are retired
//! (`crates/vike-cli/src/cmd/config_profile_bootstrap.rs`'s `config bootstrap-daemon` is the writer
//! that replaces it, building a `StoredProfile` directly from argv). The runbooks below are
//! unaffected: they are `docs/ops/` REFERENCE material for the fields a bootstrap invocation's
//! arguments should carry, never a file any binary reads.
//!
//! # What is asserted, and why it is the strongest set that holds in CI
//!
//! For EVERY `docs/ops/tradehub-*-live.toml` (discovered by listing the directory, so a fourth
//! venue's runbook joins the gate the moment the file exists — [`KNOWN_PROFILES`] is only the
//! floor that keeps the listing from going silently vacuous):
//!
//!   1. [`DaemonProfile::from_toml_str`] succeeds — the same parse-then-validate entry that would
//!      back a `--proves <file>`-shaped comparison, i.e. the strongest check this file's bytes admit;
//!   2. [`DaemonProfile::validate_for_live`] succeeds — these are LIVE runbooks and that check is
//!      the daemon's live gate, and it is PURE (the [`LIVE_WIRED_VENUES`] allow-list,
//!      `vike_tradehub::wired_markets::WIRED_MARKETS` routing, venue symbol shape, `seed_cash` finiteness — no
//!      credentials, no network, no env), so it holds on the credential-free CI runners.
//!
//! Per the repo's mutation-test-every-gate rule, [`the_parser_this_gate_relies_on_can_refuse`]
//! proves the rejection path the gate exists to catch is reachable through this exact entry point:
//! a planted unknown TOP-LEVEL key must fail the parse of each real profile, by name.

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

/// (1)+(2): every shipped runbook parses through the daemon's real entry and passes the pure live
/// gate — i.e. the profile actually works against THIS tree's `DaemonProfile`.
#[test]
fn every_shipped_daemon_profile_parses_and_passes_the_live_gate() {
    for (path, text) in runbook_profiles() {
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

/// The mutation self-test: `deny_unknown_fields` rejection — the exact drift shape this gate
/// exists for — is reachable through the same entry the assertions above use. A planted unknown
/// top-level key (prepended, so it cannot land inside the trailing `[daemon]` table and test the
/// wrong struct) must fail each REAL profile's parse, and the error must name the key.
#[test]
fn the_parser_this_gate_relies_on_can_refuse() {
    for (path, text) in runbook_profiles() {
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
