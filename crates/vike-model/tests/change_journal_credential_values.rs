//! **A `credential_write` record cannot carry a credential VALUE, and the impossibility is
//! STRUCTURAL rather than remembered.**
//!
//! The rule this file gates is the one that made `vike_config::Policy` unable to implement
//! `EnvOverride`: when a property must hold for every future author, the cure is that there is no
//! code path, not that everybody remembers. `vike_model::change_journal::Change::credential_write`
//! takes NO old/new/value parameter, and
//! `vike_model::change_journal::CredentialTarget`'s fields are private with no value among them —
//! so recording one is not a call-site mistake somebody could make in a diff nobody is reading. It
//! is a change to a type's shape and a constructor's signature, which a reviewer sees.
//!
//! # Why a SOURCE gate and not just a behavioural test
//!
//! A behavioural test can only assert about the values a record DOES carry; it cannot assert about
//! a parameter that does not exist. The thing being defended is an ABSENCE, and the only way to
//! gate an absence is to read the declaration. So this file does both: [`the_credential_constructor_takes_no_value_parameter`]
//! and [`the_credential_target_declares_no_value_field`] read the real source, and
//! [`a_credential_record_records_names_and_a_count`] proves the record is still USEFUL — because a
//! record that carried nothing would satisfy the absence trivially and answer no question.
//!
//! # ⚠ The vacuity trap this file is built to avoid
//!
//! A gate whose guard keys on the thing under test SKIPS rather than FAILS under mutation: "if we
//! can find `credential_write`, check it" quietly passes the day the parser stops finding it, and
//! stops the day somebody renames the function. So every source assertion here is paired with a
//! WITNESS on an INDEPENDENT precondition — the SAME parser run against
//! `Change::set_setting`, which legitimately DOES take `old` and `new`, and against
//! `SettingTarget`, which legitimately DOES declare them as public fields. If the parser breaks,
//! the witness goes red first and names the parser rather than the rule.

use std::path::{Path, PathBuf};

use vike_model::change_journal::{
    Actor, Change, ChangeJournal, MAX_CREDENTIAL_KEYS, Outcome, Proc, Target,
};

/// Names that must never appear as a parameter of, or a field reachable from, a credential record.
///
/// Deliberately a wider net than "value": the point is that a credential's SECRET half has many
/// spellings and the gate must not depend on picking the one a future author happens to use.
/// `key`/`keys` is emphatically NOT on the list — a key NAME is what this record exists to carry
/// (`vike-cli secrets list` prints names by an explicit decision in the root `CLAUDE.md`).
const VALUE_WORDS: [&str; 8] =
    ["old", "new", "value", "values", "secret", "secrets", "password", "token"];

fn source_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("change_journal.rs")
}

fn source() -> String {
    let p = source_path();
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The PARAMETER NAMES of `fn <name>(` in `src`, as declared.
///
/// A line scan over rustfmt's output rather than a parse: this workspace's gates are text-only by
/// convention (`crates/vike-ops/tests/layer_gate.rs`,
/// `crates/vike-boot/tests/dependency_floor.rs`), and the alternative is a `syn` dependency in the
/// crate every binary in this workspace links. `None` when the function is not found at all, which
/// the callers turn into a FAILURE rather than a skip.
fn parameter_names(src: &str, name: &str) -> Option<Vec<String>> {
    let at = src.find(&format!("pub fn {name}("))?;
    let open = src[at..].find('(')? + at;
    let close = src[open..].find(')')? + open;
    Some(
        src[open + 1..close]
            .split(',')
            .filter_map(|p| p.split_once(':'))
            .map(|(n, _)| n.trim().trim_start_matches("mut ").to_string())
            .filter(|n| !n.is_empty())
            .collect(),
    )
}

/// The FIELD DECLARATIONS of `pub struct <name> { .. }` in `src`, as `(is_pub, field_name)`.
///
/// Doc comments, attributes and blank lines are skipped; a field is a line holding `name: Type,`.
/// `None` when the struct is not found — again a failure at the call site, never a skip.
fn field_declarations(src: &str, name: &str) -> Option<Vec<(bool, String)>> {
    let at = src.find(&format!("pub struct {name} {{"))?;
    let open = src[at..].find('{')? + at;
    let close = src[open..].find("\n}")? + open;
    Some(
        src[open + 1..close]
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'))
            .filter_map(|l| {
                let is_pub = l.starts_with("pub ");
                let body = l.strip_prefix("pub ").unwrap_or(l);
                body.split_once(':').map(|(n, _)| (is_pub, n.trim().to_string()))
            })
            .filter(|(_, n)| !n.is_empty())
            .collect(),
    )
}

/// **THE RULE.** The credential constructor accepts no parameter that could be a credential value.
#[test]
fn the_credential_constructor_takes_no_value_parameter() {
    let src = source();
    let params = parameter_names(&src, "credential_write").unwrap_or_else(|| {
        panic!(
            "`pub fn credential_write(` was not found in {}. This gate defends an ABSENCE, so a \
             parser that finds nothing would otherwise pass forever — if the constructor was \
             renamed, re-point this test; if it was deleted, delete this file with it",
            source_path().display()
        )
    });
    assert!(!params.is_empty(), "the parameter scan returned nothing: {params:?}");

    let offenders: Vec<&String> =
        params.iter().filter(|p| VALUE_WORDS.contains(&p.as_str())).collect();
    assert!(
        offenders.is_empty(),
        "\n`Change::credential_write` grew the parameter(s) {offenders:?}.\n\n\
         A credential VALUE must be structurally impossible to journal, not merely omitted by \
         convention — the same discipline that makes `vike_config::Policy` unable to implement \
         `EnvOverride`. The change journal lives in `<project>/settings/state/changes`, which is \
         plaintext, is not `chmod 600`, and is exactly the file an operator copies into an \
         incident report.\n\n\
         What to do instead: record the key NAME. `vike-cli secrets list` prints names by an \
         explicit decision in the root CLAUDE.md, names are what makes \"when did I change the okx \
         passphrase\" answerable, and `count` already says how many were written.\n\n\
         declared parameters: {params:?}\n"
    );
    // Non-vacuity, on an INDEPENDENT precondition: the same parser must still find the value
    // parameters on the constructor that legitimately HAS them. A parser that stopped matching
    // reddens HERE, naming itself, instead of letting the assertion above pass on an empty list.
    let setting = parameter_names(&src, "set_setting")
        .expect("`Change::set_setting` must parse — it is this parser's witness");
    assert!(
        setting.iter().any(|p| p == "old") && setting.iter().any(|p| p == "new"),
        "the parameter scanner no longer finds `old`/`new` on `Change::set_setting`, where they \
         genuinely are. The scanner is broken, so the rule above is being checked against nothing. \
         got: {setting:?}"
    );
}

/// …and the same for the TYPE, because a constructor that took no value would still be defeated by
/// a public field somebody could assign after the fact.
#[test]
fn the_credential_target_declares_no_value_field() {
    let src = source();
    let fields = field_declarations(&src, "CredentialTarget").unwrap_or_else(|| {
        panic!(
            "`pub struct CredentialTarget {{` was not found in {}. See the sibling test: this gate \
             defends an ABSENCE and must fail rather than skip",
            source_path().display()
        )
    });
    assert!(!fields.is_empty(), "the field scan returned nothing: {fields:?}");

    let offenders: Vec<&(bool, String)> =
        fields.iter().filter(|(_, n)| VALUE_WORDS.contains(&n.as_str())).collect();
    assert!(offenders.is_empty(), "`CredentialTarget` declares value field(s) {offenders:?}");

    // Every field PRIVATE — the half that makes the constructor the only way in. A `pub` field is a
    // second constructor wearing a different syntax (`CredentialTarget { .. }` at any call site).
    let public: Vec<&String> = fields.iter().filter(|(p, _)| *p).map(|(_, n)| n).collect();
    assert!(
        public.is_empty(),
        "`CredentialTarget`'s field(s) {public:?} are `pub`. Struct-literal construction is a \
         second constructor, and it takes whatever fields exist — which is precisely the door \
         `Change::credential_write`'s signature closes. Add an accessor method instead.\n\
         declared fields: {fields:?}"
    );

    // Non-vacuity again, on an independent precondition: `SettingTarget` legitimately declares
    // PUBLIC `old` and `new` fields, so the same scanner must find both.
    let setting = field_declarations(&src, "SettingTarget")
        .expect("`SettingTarget` must parse — it is this parser's witness");
    assert!(
        setting.contains(&(true, "old".to_string()))
            && setting.contains(&(true, "new".to_string())),
        "the field scanner no longer finds the public `old`/`new` on `SettingTarget`, where they \
         genuinely are — so both assertions above are being checked against nothing. got: {setting:?}"
    );
}

/// The other half: the record is still WORTH writing. An absence satisfied by recording nothing
/// would pass both tests above and answer no question at all.
#[test]
fn a_credential_record_records_names_and_a_count() {
    let keys = ["OKX_LIVE_API_KEY", "OKX_LIVE_API_SECRET", "OKX_LIVE_API_PASSPHRASE"];
    let change =
        Change::credential_write(Outcome::Applied, Actor::Gui, "secrets.env", "okx", "LIVE", &keys);
    let Target::Credential(target) = change.target() else { panic!("a credential target") };
    assert_eq!(target.keys(), keys, "every NAME is recorded — that is the answerable part");
    assert_eq!(target.count(), keys.len(), "…and the count matches");
    assert_eq!(target.venue(), "okx");
    assert_eq!(target.tier(), "LIVE");
    assert_eq!(target.store(), "secrets.env");

    // The rendered LINE is where a leak would actually land, so assert on the bytes too: the names
    // are there, and nothing that looks like a secret can be, because none was ever accepted.
    let j = ChangeJournal::new(PathBuf::from("unused"), Proc::new("vike-test", 1, "0.1.0"));
    let line = j.render(1_787_356_800_000, &change).expect("render");
    for k in keys {
        assert!(line.contains(k), "the key name {k} must be in the line: {line}");
    }
    assert!(line.contains(r#""count":3"#), "{line}");

    // Over the cap the NAMES are trimmed and the COUNT is not, so a capped record admits the trim
    // instead of silently claiming the cap was the whole write.
    let many: Vec<String> =
        (0..MAX_CREDENTIAL_KEYS * 3).map(|i| format!("V{i}_LIVE_API_KEY")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let change = Change::credential_write(
        Outcome::Applied,
        Actor::Gui,
        "secrets.env",
        "multi",
        "LIVE",
        &refs,
    );
    let Target::Credential(target) = change.target() else { panic!("a credential target") };
    assert_eq!(target.keys().len(), MAX_CREDENTIAL_KEYS);
    assert_eq!(target.count(), MAX_CREDENTIAL_KEYS * 3);
}

/// The scanners' own unit tests, against the shapes rustfmt actually produces — including the
/// multi-line parameter list this file reads and the doc-commented field block.
#[test]
fn the_scanners_read_the_shapes_that_occur() {
    let src = "\
impl X {
    /// docs
    pub fn credential_write(
        outcome: Outcome,
        actor: Actor,
        keys: &[&str],
    ) -> Self {
    }
    pub fn set_setting(a: u8, old: Option<&str>, new: &str) -> Self {}
}

/// docs
#[derive(Debug)]
pub struct CredentialTarget {
    /// the store
    store: String,
    #[serde(skip)]
    keys: Vec<String>,
}
";
    assert_eq!(
        parameter_names(src, "credential_write").unwrap(),
        ["outcome", "actor", "keys"],
        "a multi-line parameter list parses"
    );
    assert_eq!(
        parameter_names(src, "set_setting").unwrap(),
        ["a", "old", "new"],
        "…and a single-line one"
    );
    assert_eq!(
        parameter_names(src, "nope"),
        None,
        "a missing function is None, never an empty list"
    );

    assert_eq!(
        field_declarations(src, "CredentialTarget").unwrap(),
        [(false, "store".to_string()), (false, "keys".to_string())],
        "doc comments and attributes are skipped; privacy is reported"
    );
    assert_eq!(field_declarations(src, "Nope"), None);

    // …and the scanners must actually SEE a violation when one is planted — the direction that
    // matters, and the one a scanner that silently matched nothing would fail.
    let planted = "impl X { pub fn credential_write(keys: &[&str], new: &str) -> Self {} }";
    let params = parameter_names(planted, "credential_write").unwrap();
    assert!(
        params.iter().any(|p| VALUE_WORDS.contains(&p.as_str())),
        "a planted value parameter must be seen: {params:?}"
    );
    let planted = "pub struct CredentialTarget {\n    pub new: String,\n}\n";
    let fields = field_declarations(planted, "CredentialTarget").unwrap();
    assert_eq!(fields, [(true, "new".to_string())], "a planted public value field must be seen");
}
