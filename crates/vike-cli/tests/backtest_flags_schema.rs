//! **The CLI's local copies of profile knowledge, held against the real profile loader.**
//!
//! # Why this file exists
//!
//! `crates/vike-cli/Cargo.toml` states this crate's identity in its own dependency comment:
//! *"DataFusion-FREE by construction … No concrete backend, no vike-backtest, no engine crates."*
//! So `crates/vike-cli/src/cmd/backtest.rs` **cannot name `BacktestProfile`**, and every piece of
//! schema knowledge the flag surface needs — which top-level tables exist, which `data.kind`
//! spellings parse, which keys the sugar flags land on — is necessarily a COPY.
//!
//! A copy without a gate rots. `vike-backtest` IS reachable from HERE:
//! `crates/vike-cli/Cargo.toml`'s `[dev-dependencies]` carries
//! `vike-backtest = { path = "../vike-backtest", features = ["hist-replay"] }`, which is what
//! `crates/vike-cli/tests/search_walkforward_cli.rs` already uses for its loopback server. So this
//! file links the REAL `vike_backtest::harness::BacktestProfile` and puts every local copy through
//! it.
//!
//! ⚠ It proves the copies AGREE. It cannot prove the CLI's refusals are complete: `--set` performs
//! no full-key schema check by design (an undeclared `engine.*` key is refused on the far side by
//! `#[serde(deny_unknown_fields)]`, on exit rung 1), and two subtrees — `strategy.params` (an
//! untyped `toml::Value`) and `sweep` (an `Option<toml::Table>`) — accept ANY key silently. That
//! asymmetry is MEASURED by `the_untyped_subtrees_accept_any_key` below rather than assumed away.
//!
//! ⚠ `crates/vike-cli/tests/demo_tape_profile.rs` is the precedent for the shape: a vike-cli test
//! reaching a `pub` CLI item and holding it against another crate's authority.

use vike_backtest::harness::BacktestProfile;
use vike_cli::cmd::backtest::{PROFILE_TOP_LEVEL_KEYS, SUGAR, Shape, Sugar};

/// A profile that parses and validates, as the baseline every case perturbs.
pub const VALID: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
"#;

/// The baseline must actually load, or every assertion below proves nothing.
#[test]
fn the_baseline_profile_loads() {
    BacktestProfile::from_toml_str(VALID).expect("the fixture must be a loadable profile");
}

/// Every key the CLI will accept as a `--set`'s FIRST segment is one `BacktestProfile` declares.
///
/// The failure this catches: a top-level key renamed or removed in vike-backtest while
/// `PROFILE_TOP_LEVEL_KEYS` still names it, so the CLI accepts a `--set` the loader then refuses
/// with a message the operator cannot act on.
///
/// ⚠ It asks only "is this name KNOWN". A wrong-type or missing-inner-field complaint is a
/// different message and is not this gate's business — `unknown field` is the string
/// `deny_unknown_fields` produces and the only one that means what this test means.
#[test]
fn every_cli_top_level_key_is_one_the_profile_declares() {
    for key in PROFILE_TOP_LEVEL_KEYS {
        let text = if key == "name" {
            format!("name = \"x\"\n{VALID}")
        } else if VALID.contains(&format!("[{key}]")) {
            // Already in the baseline, so its name is proven known by `the_baseline_profile_loads`.
            continue;
        } else {
            format!("{VALID}\n[{key}]\n")
        };
        if let Some(msg) = BacktestProfile::from_toml_str(&text).err().map(|e| e.to_string()) {
            assert!(
                !msg.contains("unknown field"),
                "the CLI accepts `--set {key}.…` but BacktestProfile does not declare `{key}`: \
                 {msg}\nRe-key PROFILE_TOP_LEVEL_KEYS in crates/vike-cli/src/cmd/backtest.rs"
            );
        }
    }
}

/// …and the other direction: a name the CLI does NOT list is genuinely refused by the loader, so
/// the local refusal is not stricter than the schema.
#[test]
fn a_key_the_cli_refuses_is_one_the_profile_refuses_too() {
    let text = format!("{VALID}\n[egnine]\n");
    let msg = BacktestProfile::from_toml_str(&text)
        .err()
        .map(|e| e.to_string())
        .expect("an undeclared top-level table must be refused");
    assert!(msg.contains("unknown field"), "expected a deny_unknown_fields refusal: {msg}");
    assert!(!PROFILE_TOP_LEVEL_KEYS.contains(&"egnine"));
}

/// ⚠ THE MEASURED ASYMMETRY, recorded as a test rather than as prose. Spec §5.3 says a `--set`
/// naming an undeclared key is *"a hard load error, not a silent no-op"*. It is — except in the two
/// subtrees whose serde type is untyped, which are exactly the two `--param` targets. If this test
/// ever FAILS, the spec's claim has become true and the `--set` doc comment should say so.
///
/// ⚠ The parameter-search SECTION was renamed by owner ruling R2 (`[sweep]` → `[paramscan]`) and
/// landed as a PERMANENT serde alias in stage 7, so BOTH spellings are fixtures here: an alias that
/// stopped loading would break every profile already on an operator's disk, silently, at parse.
#[test]
fn the_untyped_subtrees_accept_any_key() {
    for text in [
        format!("{VALID}\n[strategy.params]\nthis_is_a_typo = 1\n"),
        format!("{VALID}\n[paramscan]\nthis_is_a_typo = [1, 2]\n"),
        format!("{VALID}\n[sweep]\nthis_is_a_typo = [1, 2]\n"),
    ] {
        let _p = BacktestProfile::from_toml_str(&text).unwrap_or_else(|e| {
            panic!(
                "strategy.params / paramscan are untyped today, so this must LOAD. It did not: \
                 {e}\n\
                 If the schema tightened, update the --set doc comment and this plan's Spec \
                 corrections — do not delete this test.\n\
                 If the `sweep` ALIAS was dropped, put it back: it is permanent, and every profile \
                 written before stage 7 spells the section that way."
            )
        });
    }
}

/// A profile line for a sugar key, at the TYPE its shape produces.
///
/// ⚠ **The `_` arm is `"binance"`, a REAL value only for `data.venue`** — so a `Str`-shaped key
/// whose field is not free-form needs its own arm here, or the gate below measures whether the
/// loader tolerates a nonsense string rather than whether it accepts the values the flag writes.
/// `engine.decide` is the worked example: it is `Option<String>`, so `decide = "binance"` LOADS,
/// and the arm is what makes this test say something about `--decide`.
fn rendered_for(s: &Sugar) -> String {
    match s.shape {
        Shape::Scalar => "1.0".to_string(),
        Shape::Str => match s.key {
            "data.kind" => "\"bar\"".to_string(),
            "data.interval" => "\"1d\"".to_string(),
            "data.from" | "data.to" => "\"0\"".to_string(),
            "strategy.name" => "\"buy_hold\"".to_string(),
            // ⚠ `sequential`, not `simultaneous`. Both are values `decide_mode` resolves, but
            // `simultaneous` is refused by `BacktestProfile::refusals` in three combinations (a
            // tick lane, an armed `[risk]` table, a `data.detail_interval`) — so a baseline that
            // later grows any of the three would make this row fail for a reason that is about the
            // COMBINATION and not about the flag. `sequential` is the absent-key default and can
            // never be refused, which is the property this fixture wants.
            "engine.decide" => "\"sequential\"".to_string(),
            _ => "\"binance\"".to_string(),
        },
        Shape::StrList => "[\"BTCUSDT\"]".to_string(),
    }
}

/// Every key a sugar flag lands on is one the profile loader accepts, at the TYPE the flag's shape
/// produces.
///
/// The failure this catches: `--from` shipping a TOML integer onto a `String` field, or a key
/// renamed in the schema while the flag still writes the old one — both of which produce a
/// confusing far-side type error naming a key the operator never typed.
///
/// ⚠ The fixture is built by REPLACING the baseline's own line for that key, because appending a
/// second `[data]` header or a second `kind =` line is a TOML error rather than a schema one.
#[test]
fn every_sugar_key_is_accepted_by_the_profile_loader_at_the_shape_it_writes() {
    for s in SUGAR {
        let (table, leaf) = s.key.split_once('.').expect("every sugar key is table.leaf");
        let header = format!("[{table}]");
        assert!(VALID.contains(&header), "the baseline must declare {header} to patch it");
        // Drop the baseline's own line for this leaf (if any), then insert ours under the header.
        let stripped: String = VALID
            .lines()
            .filter(|l| !l.trim_start().starts_with(&format!("{leaf} =")))
            .collect::<Vec<_>>()
            .join("\n");
        let patched =
            stripped.replacen(&header, &format!("{header}\n{leaf} = {}", rendered_for(&s)), 1);
        if let Some(msg) = BacktestProfile::from_toml_str(&patched).err().map(|e| e.to_string()) {
            assert!(
                !msg.contains("unknown field") && !msg.contains("invalid type"),
                "`{}` writes {} = {}, which the profile loader refuses: {msg}\n\
                 Re-key SUGAR in crates/vike-cli/src/cmd/backtest.rs",
                s.flag,
                s.key,
                rendered_for(&s)
            );
        }
    }
}

/// ⚠ **WHERE `engine.decide`'s REFUSAL LIVES, measured — and it is the evidence for the CLI
/// forwarding `--decide` unvalidated rather than keeping a roster of its own.**
///
/// `crates/vike-cli/src/cmd/backtest.rs`'s `parse_run_args` checks two rosters (`--kind` and
/// `--interval`) and deliberately not this one, because `sequential | simultaneous` has no
/// `NAMES`-shaped home on the far side: `vike_backtest::harness::profile`'s `decide_mode` MATCHES
/// the two strings and types them again into its own refusal, so a third copy on the client would
/// be the more-than-one-home defect `vike_backtest::data_plan`'s `OnGap::NAMES` exists to cure.
/// (That is exactly why the two `[data]` rosters below ARE copied and gated, and this one is not:
/// they have a source to be held against, and this has none.)
///
/// What this measures is the COST of that choice, so nobody has to guess at it: an unknown spelling
/// is NOT caught at load and NOT caught by `validate` — `BacktestProfile::refusals` calls
/// `decide_mode(...).unwrap_or_default()`, which swallows the error on purpose so the value is
/// reported once rather than twice — and reaches an operator only from the construction site inside
/// `harness::run_backtest`. So a typo costs a spawn or a round trip plus a store open.
///
/// ⚠ If this test ever FAILS, the loader has GAINED a load-time refusal for this key, and the right
/// response is to say so on that `SUGAR` row and on `vike_cli::surface`'s `--decide` row — not to
/// delete the test. A local roster only becomes the better trade if the far side grows a `NAMES`
/// array to hold it against.
#[test]
fn an_unknown_decide_spelling_is_not_refused_at_load_or_by_validate() {
    let patched = VALID.replacen("[engine]", "[engine]\ndecide = \"__not_a_mode__\"", 1);
    let p = BacktestProfile::from_toml_str(&patched).unwrap_or_else(|e| {
        panic!(
            "`engine.decide` is `Option<String>`, so an unknown spelling must still LOAD — the \
             refusal lives at the EngineParams construction site, not here. It did not: {e}\n\
             If the schema tightened, the CLI can now spelling-check --decide locally: see this \
             test's doc before changing either side."
        )
    });
    // …and the value survives to the far side verbatim, which is what "forwarded unvalidated"
    // means: the ENGINE's own sentence is the one an operator reads.
    assert_eq!(p.engine.decide.as_deref(), Some("__not_a_mode__"));

    // The two spellings the flag documents both load and validate, so the flag's own values are
    // never the thing that fails. `sequential` is the absent-key default; `simultaneous` is
    // bar-mode-only and the baseline is bar, declares no `[risk]` and sets no `detail_interval`.
    for mode in ["sequential", "simultaneous"] {
        let text = VALID.replacen("[engine]", &format!("[engine]\ndecide = \"{mode}\""), 1);
        BacktestProfile::from_toml_str(&text)
            .unwrap_or_else(|e| panic!("`--decide {mode}` writes a value the loader refuses: {e}"));
    }
}

/// The two values the CLI IMPLIES must both be ones the schema accepts. `data.kind = "bar"` is a
/// `DataKind` variant; `strategy.name = "rhai"` is a name the registry resolves (and is
/// deliberately NOT in `STRATEGIES`, which is why it cannot be checked against that roster).
#[test]
fn the_implied_defaults_are_values_the_schema_accepts() {
    let stripped: String = VALID
        .lines()
        .filter(|l| !l.trim_start().starts_with("kind =") && !l.trim_start().starts_with("name ="))
        .collect::<Vec<_>>()
        .join("\n");
    let patched = stripped.replacen("[data]", "[data]\nkind = \"bar\"", 1).replacen(
        "[strategy]",
        "[strategy]\nname = \"rhai\"",
        1,
    );
    if let Some(msg) = BacktestProfile::from_toml_str(&patched).err().map(|e| e.to_string()) {
        assert!(
            !msg.contains("unknown field") && !msg.contains("unknown variant"),
            "the CLI implies kind = \"bar\" / name = \"rhai\", which the loader refuses: {msg}"
        );
    }
}

/// **Spec §15.4: `--write-profile` round-trips.** Flags → profile → re-run → the same resolved
/// profile.
///
/// ⚠ It is written against the RESOLVED profile, never against an operator's file text. The whole
/// rewrite chain re-parses to `toml::Value` and re-serializes with `toml::to_string`, which drops
/// comments and normalizes key order — so a byte comparison against an authored file could never
/// pass and would not be measuring the right thing. What must hold is that running FROM the written
/// file produces the document the flags produced, which is what makes a written profile a faithful
/// record of the command line that made it.
///
/// ⚠ **It does NOT gate `build_profile_toml`'s `profile_key_is_set` guard, and the stage-2 plan's
/// Task 10 step 1 claims it does.** That claim cannot hold: pass 1 passes no `--kind`, so its own
/// output already carries `kind = "bar"`, and pass 2 re-applying the same implied value is
/// byte-identical WITH the guard or WITHOUT it. The guard is genuinely gated by
/// `crates/vike-cli/src/cmd/backtest.rs`'s `an_explicit_kind_and_a_files_kind_both_beat_the_implied_default`
/// and `sugar_beats_set_and_implied_never_overwrites` — go there if you are changing it. What THIS
/// test is worth is undiminished: it is the only check that a written profile re-runs to itself and
/// that the written bytes load through the real engine loader.
#[test]
fn a_written_profile_rebuilds_to_itself() {
    let flags = [
        "backtest",
        "run",
        "--venue",
        "binance",
        "--symbol",
        "BTCUSDT,ETHUSDT",
        "--interval",
        "1h",
        "--from",
        "0",
        "--to",
        "100000",
        "--strategy",
        "buy_hold",
        "--cash",
        "1000",
        "--fee",
        "0.001",
        "--set",
        "engine.slippage=0.0005",
        "--set",
        "strategy.params.size=1.0",
    ];
    // ⚠ A self-deleting handle, held for the whole test — NOT a hand-minted
    // `env::temp_dir().join(…)` plus a trailing `remove_dir_all`, which
    // `crates/vike-ops/tests/journal_scratch_gate.rs` refuses by name: cleanup that is not in a
    // `Drop` does not run when the work fails, and this test panics on purpose when it fails.
    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("a.toml");
    let second = dir.path().join("b.toml");

    let write_one = |path: &std::path::Path, argv: &[&str]| {
        let mut full: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        full.push("--write-profile".to_string());
        full.push(path.to_str().expect("utf-8").to_string());
        full.push("--show-effective".to_string());
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_vike-cli"))
            .args(&full)
            .output()
            .expect("run vike-cli");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{full:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    // Pass 1: the flags write a profile.
    write_one(&first, &flags);
    let a = std::fs::read_to_string(&first).expect("first write");

    // Pass 2: that profile, with NO flags, writes again.
    write_one(&second, &["backtest", "run", "--profile", first.to_str().expect("utf-8")]);
    let b = std::fs::read_to_string(&second).expect("second write");

    assert_eq!(a, b, "a written profile must rebuild to itself");

    // …and it is a profile the real loader accepts, which is what makes the round trip mean
    // anything at all.
    BacktestProfile::from_toml_str(&a)
        .expect("the written profile must be one the engine would load");
}

/// ⚠ **The ENGINE-SIDE leg of the two `[data]` value rosters — and it took THREE attempts, each
/// failing for a different reason worth recording.**
///
/// `vike_cli::surface::ROSTERS`' `gap_dispositions` and `universe_modes` rows are hand copies:
/// this crate links no engine crate, so nothing in `src/` can name the types they copy. That is
/// the same argument `data_kinds` and `profile_top_level_keys` above make, and each row states it
/// in its own `admission` and names this test as the residual it leaves.
///
/// # Attempt 1 could not fail for the reason its doc gave
///
/// It drove each CLI member through the profile loader and asserted acceptance, claiming to catch
/// a FOURTH arm added to the engine. A new engine arm breaks no assertion in that loop; it caught
/// only the cheaper half, an arm the engine DROPPED while the CLI still published it.
///
/// # Attempt 2 held a copy against another copy
///
/// It parsed the accepted set out of the engine's own refusal (`… is not one of a | b | c`) and
/// compared THAT to the CLI row. A MUTATION PROOF killed it: a fourth arm planted in
/// `DataCfg::on_gap`'s resolver left this test GREEN, because the refusal sentence was itself a
/// hand-written copy of the arms and did not move with them.
///
/// # What it holds now
///
/// `vike_backtest::data_plan`'s `OnGap::NAMES` and `UniverseMode::NAMES` are the ONE roster: the
/// resolvers WALK them (`OnGap::parse`, not a `match`), the refusals RENDER them
/// (`OnGap::roster`), and `OnGap::name` is an exhaustive `match` on the variant, so a new variant
/// cannot compile without a row. This test compares the CLI's copy to that array — copy against
/// SOURCE — and then drives every member through the real loader, so a roster that has drifted
/// from its own resolver cannot satisfy it either. The mutation that defeated attempt 2 fails here.
///
/// ⚠ `require_coverage = true` is not decoration. `BacktestProfile::refusals` refuses
/// `data.on_gap` outright without it ("the gate it configures is not armed"), so a profile
/// carrying a disposition alone does not load at all and this test would be measuring that
/// refusal instead of the spelling.
#[test]
fn every_data_roster_member_is_a_value_the_profile_loader_resolves() {
    fn profile_with(key: &str, value: &str) -> String {
        VALID.replace(
            "from = \"0\"",
            &format!("from = \"0\"\nrequire_coverage = true\n{key} = \"{value}\""),
        )
    }

    for (id, key, engine) in [
        ("gap_dispositions", "on_gap", &vike_backtest::data_plan::OnGap::NAMES[..]),
        ("universe_modes", "universe", &vike_backtest::data_plan::UniverseMode::NAMES[..]),
    ] {
        let row = vike_cli::surface::ROSTERS
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("the `{id}` roster row exists"));
        assert_eq!(
            row.members,
            engine,
            "`{id}` and the engine disagree about `data.{key}`.\nleft = \
             `vike_cli::surface::ROSTERS`' hand copy, right = the engine's own `NAMES`, which its \
             resolver walks and its refusal renders. A member only on the RIGHT is a spelling the \
             profile accepts and `--{}` refuses as unknown; one only on the LEFT is a value this \
             CLI advertises and the loader rejects. See that row's `admission`.",
            key.replace('_', "-")
        );

        for member in row.members {
            let p = BacktestProfile::from_toml_str(&profile_with(key, member))
                .unwrap_or_else(|e| panic!("`{id}` publishes `{member}`, loader refuses it: {e}"));
            if key == "on_gap" {
                p.data.on_gap().unwrap_or_else(|e| panic!("{id}/{member} resolves: {e}"));
            } else {
                p.data.universe_mode().unwrap_or_else(|e| panic!("{id}/{member} resolves: {e}"));
            }
        }

        assert!(
            BacktestProfile::from_toml_str(&profile_with(key, "__not_a_member__")).is_err(),
            "the loader accepted `data.{key} = \"__not_a_member__\"`, so `{id}` proves nothing"
        );
    }
}
