//! `account copy-credentials`, asserted over the shipped binary: a second account row of the same
//! venue and tier ends up holding the first row's fields, as every reader sees them; no VALUE
//! reaches stdout, stderr or the ledger; every refusal writes and journals nothing; a field the
//! destination already holds is refused by NAME until `--replace`, which replaces exactly those
//! fields.

use std::collections::BTreeMap;
use std::process::Output;

use super::support::{PlantedAccount, SetCase, carries_no_value, exit_code, rung};
use super::{stderr, stdout};

const KEY: &str = "bybit-demo-key-AAAA1111";
const SECRET: &str = "bybit-demo-secret-BBBB2222";
const LIVE_KEY: &str = "bybit-live-key-CCCC3333";
const OLD_INVERSE_KEY: &str = "old-inv-key-DDDD4444";
const INVERSE_SECRET: &str = "inv-own-secret-EEEE5555";

/// The the CI box shape: bybit's DEMO row holding a key pair, and a keyless `INV` twin of it, beside an
/// okx demo row and a bybit LIVE row, so the venue and tier refusals have a row to be refused over.
const ACCOUNTS: &[PlantedAccount<'static>] = &[
    ("bybit", "demo", Some("DEMO"), &[("API_KEY", KEY), ("API_SECRET", SECRET)]),
    ("bybit", "demo", Some("INV"), &[]),
    ("okx", "demo", None, &[]),
    ("bybit", "live", Some("LIVEROW"), &[("API_KEY", LIVE_KEY)]),
];

/// Every value any fixture here holds — what no stream and no ledger line may carry.
fn values() -> BTreeMap<String, String> {
    [
        ("KEY", KEY),
        ("SECRET", SECRET),
        ("LIVE_KEY", LIVE_KEY),
        ("OLD_INV_KEY", OLD_INVERSE_KEY),
        ("INV_SECRET", INVERSE_SECRET),
    ]
    .iter()
    .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
    .collect()
}

/// A project holding [`ACCOUNTS`]; the ids in the order given (DEMO, INV, okx, live).
fn seeded(tag: &str) -> (SetCase, [String; 4]) {
    let c = SetCase::new(tag);
    let ids = c.seed_accounts(ACCOUNTS);
    let ids = [ids[0], ids[1], ids[2], ids[3]].map(|id| id.to_string());
    (c, ids)
}

/// One account's live fields with their values, through the store's own reader — for COMPARING,
/// never for printing.
fn fields_of(c: &SetCase, id: &str) -> BTreeMap<String, String> {
    let id: i64 = id.parse().unwrap();
    let book = c.credential_store().credential_registry().expect("the store reads");
    book.account(id).map_or_else(BTreeMap::new, |f| {
        f.fields().map(|n| (n.to_string(), f.get(n).unwrap_or_default().to_string())).collect()
    })
}

fn copy(c: &SetCase, from: &str, to: &str, extra: &[&str]) -> Output {
    let mut argv = vec!["account", "copy-credentials", "--from-id", from, "--to-id", to];
    argv.extend_from_slice(extra);
    c.run(&argv, None, &[])
}

/// Both streams of `o`, as one text.
fn streams(o: &Output) -> String {
    format!("{}\n{}", stdout(o), stderr(o))
}

/// **The copy: INV ends up holding exactly DEMO's two fields, DEMO is untouched, the streams carry
/// field NAMES only, and the ledger carries ONE `credential_write` of row names.**
#[test]
fn the_copy_lands_both_fields_and_no_value_reaches_a_stream_or_the_ledger() {
    let (c, ids) = seeded("copy-creds");
    let (demo, inverse) = (ids[0].as_str(), ids[1].as_str());
    let before = fields_of(&c, demo);
    assert_eq!(before.len(), 2, "the fixture's source holds a pair");

    let out = copy(&c, demo, inverse, &["--confirm", inverse]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", streams(&out));

    assert_eq!(fields_of(&c, inverse), before, "the destination holds the source's fields");
    assert_eq!(fields_of(&c, demo), before, "the source is read, never written");
    let text = streams(&out);
    for field in ["API_KEY", "API_SECRET"] {
        assert!(text.contains(field), "the copy names each field it wrote ({field}): {text}");
    }
    carries_no_value(&text, &values(), "the copy's output");

    let writes: Vec<String> =
        c.journal_lines().into_iter().filter(|l| l.contains("credential_write")).collect();
    assert_eq!(writes.len(), 1, "ONE writer call ⇒ ONE record: {writes:?}");
    let line = &writes[0];
    for field in ["API_KEY", "API_SECRET"] {
        let row = format!("bybit#{inverse}.{field}");
        assert!(line.contains(&row), "the record names the destination row {row}: {line}");
    }
    assert!(line.contains("\"demo\"") && line.contains("vike-cli"), "{line}");
    carries_no_value(line, &values(), "the ledger record");
}

/// **`--dry-run` names every field and what WOULD happen, and writes and journals nothing.**
#[test]
fn a_rehearsal_names_the_fields_and_writes_nothing() {
    let (c, ids) = seeded("copy-creds-dry");
    let (demo, inverse) = (ids[0].as_str(), ids[1].as_str());
    let out = copy(&c, demo, inverse, &["--confirm", inverse, "--dry-run"]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", streams(&out));
    let text = stdout(&out);
    assert!(text.contains("API_KEY  would be added"), "{text}");
    assert!(text.contains("DRY RUN"), "{text}");
    assert!(fields_of(&c, inverse).is_empty(), "a rehearsal writes nothing");
    assert!(c.journal_lines().is_empty(), "…and records nothing");
    carries_no_value(&streams(&out), &values(), "the rehearsal's output");
}

/// **A demo key never lands on a live row, nor the reverse; two venues are refused too** — each
/// before any value is read, writing and journalling nothing.
#[test]
fn a_demo_key_never_lands_on_a_live_row_nor_the_reverse() {
    let (c, ids) = seeded("copy-creds-tier");
    let (demo, okx, live) = (ids[0].as_str(), ids[2].as_str(), ids[3].as_str());
    let live_before = fields_of(&c, live);
    for (from, to) in [(demo, live), (live, demo)] {
        let out = copy(&c, from, to, &["--confirm", to]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", streams(&out));
        assert!(stderr(&out).contains("demo key must not land on a live row"), "{}", stderr(&out));
        carries_no_value(&streams(&out), &values(), "the tier refusal");
    }
    let out = copy(&c, demo, okx, &["--confirm", okx]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", streams(&out));
    assert!(stderr(&out).contains("sign only on"), "{}", stderr(&out));

    assert_eq!(fields_of(&c, live), live_before, "the live row is untouched");
    assert!(fields_of(&c, okx).is_empty(), "the okx row is untouched");
    assert_eq!(fields_of(&c, demo).len(), 2, "the demo row is untouched");
    assert!(c.journal_lines().is_empty(), "a refusal records nothing");
}

/// **Every other refusal**: no source id, the same id twice, a row that does not exist (either
/// side), a missing or mismatched confirm, a source holding no field, `--id` beside the verb, and
/// the verb's own flags on another action. Each writes nothing, journals nothing and quotes no value.
#[test]
fn every_refusal_writes_nothing_and_quotes_no_value() {
    let (c, ids) = seeded("copy-creds-refusals");
    let (demo, inverse, okx) = (ids[0].as_str(), ids[1].as_str(), ids[2].as_str());
    let usage = rung(vike_cli::exit::Exit::Usage);
    let failed = rung(vike_cli::exit::Exit::Failed);
    let verb = ["account", "copy-credentials"];
    let cases: Vec<(Vec<&str>, i32, &str)> = vec![
        ([&verb[..], &["--to-id", inverse, "--confirm", inverse]].concat(), usage, "--from-id N"),
        (
            [&verb[..], &["--from-id", demo, "--to-id", demo, "--confirm", demo]].concat(),
            usage,
            "both",
        ),
        (
            [&verb[..], &["--from-id", "999", "--to-id", inverse, "--confirm", inverse]].concat(),
            usage,
            "--from-id 999",
        ),
        (
            [&verb[..], &["--from-id", demo, "--to-id", "999", "--confirm", "999"]].concat(),
            usage,
            "--to-id 999",
        ),
        ([&verb[..], &["--from-id", demo, "--to-id", inverse]].concat(), usage, "--confirm"),
        (
            [&verb[..], &["--from-id", demo, "--to-id", inverse, "--confirm", demo]].concat(),
            usage,
            "confirm mismatch",
        ),
        (
            [&verb[..], &["--from-id", inverse, "--to-id", demo, "--confirm", demo]].concat(),
            failed,
            "nothing to copy",
        ),
        (
            [
                &verb[..],
                &["--id", demo, "--from-id", demo, "--to-id", inverse, "--confirm", inverse],
            ]
            .concat(),
            usage,
            "does not take --id",
        ),
        (vec!["account", "activate", "--id", okx, "--replace"], usage, "copy-credentials"),
        (
            vec!["account", "rename", "--id", okx, "--label", "X", "--from-id", demo],
            usage,
            "copy-credentials",
        ),
    ];
    for (argv, rung_wanted, needle) in cases {
        let out = c.run(&argv, None, &[]);
        assert_eq!(exit_code(&out), rung_wanted, "{argv:?}: {}", streams(&out));
        assert!(stderr(&out).contains(needle), "{argv:?} must say {needle:?}: {}", stderr(&out));
        carries_no_value(&streams(&out), &values(), "a refusal");
    }
    assert!(fields_of(&c, inverse).is_empty(), "no refusal wrote the destination");
    assert_eq!(fields_of(&c, demo).len(), 2, "…or the source");
    assert!(c.journal_lines().is_empty(), "a refusal records nothing");
}

/// **A field the destination already holds is refused BY NAME without `--replace`; with it, only
/// the source's fields are written and the destination's other field is untouched.**
#[test]
fn a_field_the_destination_holds_is_refused_without_replace_and_replaced_alone_with_it() {
    let c = SetCase::new("copy-creds-replace");
    let ids = c.seed_accounts(&[
        ("bybit", "demo", Some("DEMO"), &[("API_KEY", KEY)]),
        (
            "bybit",
            "demo",
            Some("INV"),
            &[("API_KEY", OLD_INVERSE_KEY), ("API_SECRET", INVERSE_SECRET)],
        ),
    ]);
    let (demo, inverse) = (ids[0].to_string(), ids[1].to_string());
    let (demo, inverse) = (demo.as_str(), inverse.as_str());

    let refused = copy(&c, demo, inverse, &["--confirm", inverse]);
    assert_eq!(exit_code(&refused), rung(vike_cli::exit::Exit::Failed), "{}", streams(&refused));
    assert!(stderr(&refused).contains("already holds API_KEY"), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("--replace"), "{}", stderr(&refused));
    assert!(stdout(&refused).contains("refused without --replace"), "{}", stdout(&refused));
    carries_no_value(&streams(&refused), &values(), "the held-field refusal");
    assert_eq!(fields_of(&c, inverse)["API_KEY"], OLD_INVERSE_KEY, "nothing was replaced");
    assert!(c.journal_lines().is_empty(), "a refusal records nothing");

    let replaced = copy(&c, demo, inverse, &["--confirm", inverse, "--replace"]);
    assert_eq!(exit_code(&replaced), rung(vike_cli::exit::Exit::Ok), "{}", streams(&replaced));
    assert!(stdout(&replaced).contains("API_KEY  REPLACED"), "{}", stdout(&replaced));
    carries_no_value(&streams(&replaced), &values(), "the replace's output");
    let after = fields_of(&c, inverse);
    assert_eq!(after["API_KEY"], KEY, "the held field takes the source's value");
    assert_eq!(after["API_SECRET"], INVERSE_SECRET, "the destination's other field is untouched");
    let lines = c.journal_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    carries_no_value(&lines[0], &values(), "the ledger record");
}
