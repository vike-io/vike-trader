//! **`AccountEdit::SetTier` — the account table's fifth act**, and every test here is about a way
//! that one arm could be wrong while every other test in this crate stayed green.
//!
//! `account.tier` was write-once before it: a row is minted either by the migration reading a
//! credential key NAME or by `AccountEdit::Create`, so an operator who picked the wrong `--tier`
//! on that create had no way back that did not go through `Remove` — which is refused the moment
//! the row owns a credential. The cure and the hazard are the same fact: the keys DO NOT MOVE.
//! `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §5 is the design.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_row_with_no_keys_moves_and_changes_nothing_else`] | the premise: the cure for a row added at the wrong tier, and a move that took the label, the book or the id with it |
//! | [`a_row_whose_keys_spell_another_tier_is_refused_by_key_name`] | ⚠ a move that silently CREATES a second account the next time one of those keys is written |
//! | [`the_paper_tier_pins_a_row_through_its_sim_key_token`] | ⚠ THE trap: `paper`'s key token is `SIM`, so a guard written in the other direction (`_PAPER_`) matches nothing and permits every move on and off `paper` while looking like it works |
//! | [`a_hand_mapped_family_pins_its_tier_too`] | dukascopy's `DEMO1` reaches no tier grammar, so a classifier that consulted only the key grammar would let that row move freely |
//! | [`an_unknown_tier_is_refused_and_names_the_vocabulary`] | the retired `sim` spelling silently stored, and a refusal naming nothing an operator can act on |
//! | [`the_unknown_tier_refusal_echoes_no_word`] | a pasted credential quoted back into the terminal by the refusal meant to protect it |
//! | [`the_tier_guard_runs_before_the_row_lookup`] | ⚠ a typo in a flag reported to the operator as a refusal about their CREDENTIALS |
//! | [`moving_an_unlabelled_row_onto_a_tier_that_has_one_is_refused`] | ⚠ the plant no index can refuse: two unlabelled rows at one `(venue, tier)` make the NEXT credential write for that venue fail as ambiguous |
//! | [`moving_onto_a_tier_where_the_label_is_taken_is_refused`] | two rows of one `(venue, tier)` answering one `policy.accounts.<venue>.<LABEL>` address |
//! | [`moving_a_row_to_the_tier_it_already_carries_changes_nothing_and_says_so`] | a re-run reported as a change, or refused as a conflict with itself |
//! | [`a_move_writes_no_credential_row`] | a writer that "tidied" a credential row or the file store in passing — the two artifacts this verb may never touch |
//!
//! ⚠ **Every value here is FICTIONAL**, the rule `tests/account_lifecycle.rs`' module doc records
//! for this whole directory: these files are published to the public mirror like any other source.

use std::path::PathBuf;

use vike_secrets::{AccountEdit, DbErrorKind};

mod support;
use support::Fixture;

// ---------------------------------------------------------------------------------------------
// The premise
// ---------------------------------------------------------------------------------------------

/// **The case the verb exists for**: a row that owns no credential yet — the state an
/// `account add --tier live` typo leaves — moves, and the move is the TIER and nothing else.
///
/// The "nothing else" half is the one worth asserting: `account.tier` is half of
/// `UNIQUE (venue, tier, label)` and the row crosses that index, so an implementation that
/// re-created the row rather than updating it would satisfy "the tier moved" and change the id
/// underneath every runbook, GUI cell and wire client that remembered it.
#[test]
fn a_row_with_no_keys_moves_and_changes_nothing_else() {
    let fx = Fixture::migrated();
    let made = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "paper", label: Some("ALT") })
        .expect("a labelled paper row");
    let before = made.after.expect("a create returns its row");
    assert!(before.venue_account_id.is_none());

    let w = fx
        .edit(AccountEdit::SetTier { id: before.id, tier: "live" })
        .expect("a row with no credential keys moves freely");

    assert_eq!(w.verb, "set-tier");
    assert!(w.changed);
    assert_eq!(w.before.as_ref().expect("the row as found").tier, "paper");
    let after = w.after.expect("a move returns the row");
    assert_eq!(after.tier, "live");
    assert_eq!(after.id, before.id, "the id is the IDENTITY and may not move with the tier");
    assert_eq!(after.label.as_deref(), Some("ALT"));
    assert_eq!(after.venue, "binance");
    assert_eq!(after.venue_account_id, None, "the book is not this verb's column");
    assert_eq!(after.active, before.active);
    assert!(w.keys.is_empty());

    // ...and exactly one row still answers for it — no second row was left at the old tier.
    let rows = fx.accounts();
    assert_eq!(
        rows.iter().filter(|a| a.id == before.id).count(),
        1,
        "one row, moved — not a copy: {rows:?}"
    );
    assert!(
        !rows.iter().any(|a| a.venue == "binance" && a.tier == "paper"),
        "nothing may be left behind at the old tier: {rows:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The refusal the verb is shaped by
// ---------------------------------------------------------------------------------------------

/// ⚠ **The refusal the whole variant is built around.** A key NAME is never rewritten — nothing in
/// this workspace rewrites a credential store wholesale — so `BINANCE_LIVE_API_KEY` keeps spelling
/// `LIVE` after the row moves, and `AccountResolver` would read `live` out of it again and CREATE
/// A SECOND ACCOUNT the next time that key is written. Exactly `AccountKeysPinTheLabel`'s
/// mechanism, one column along.
///
/// The refusal NAMES the keys, because "this account owns credentials" is not actionable and "these
/// three names are what pins it" is.
#[test]
fn a_row_whose_keys_spell_another_tier_is_refused_by_key_name() {
    let fx = Fixture::migrated();
    let id = fx.id_of("binance", "live");

    let err = fx
        .edit(AccountEdit::SetTier { id, tier: "demo" })
        .expect_err("a row whose keys spell `live` may not be moved to `demo`");
    match &err.kind {
        DbErrorKind::AccountKeysPinTheTier { id: got, tier, keys } => {
            assert_eq!(*got, id);
            assert_eq!(tier, "live", "the refusal names the tier the KEYS spell");
            assert_eq!(keys, &["BINANCE_LIVE_API_KEY".to_string()]);
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("NOTHING WAS WRITTEN"), "{msg}");
    assert!(msg.contains("BINANCE_LIVE_API_KEY"), "the message names the key: {msg}");
    assert!(msg.contains("SECOND ACCOUNT"), "the message says what would go wrong: {msg}");
    // A refusal on this surface may name a credential KEY and never a VALUE — the store's own
    // contract, and `support::fake_value` is what a leak would print.
    assert!(!msg.contains("value-for-"), "a refusal may never carry a credential value: {msg}");

    assert_eq!(fx.row(id).expect("the row is still there").tier, "live", "nothing was written");
}

/// ⚠ **THE TRAP THIS FILE EXISTS FOR.** `account.tier` speaks `ACCOUNT_TIERS`
/// (`paper` | `demo` | `live`) and a credential key name speaks `CREDENTIAL_TIERS`
/// (`SIM` | `DEMO` | `LIVE`). The two are different alphabets, so a guard comparing them as they
/// stand can never fire — and the tempting repair, rendering the account tier DOWN into a key
/// token by uppercasing it, is wrong on exactly one tier: `paper`'s token is `SIM`, so a `_PAPER_`
/// needle matches nothing and EVERY move on and off `paper` is silently permitted by a guard that
/// looks like it works.
///
/// So this test is written to fail under that implementation specifically: the key it plants
/// contains the substring `SIM` and does not contain `PAPER` anywhere, which is asserted here
/// rather than left for a reader to notice — an `_PAPER_` guard passes every other test in this
/// file and fails this one.
///
/// Both directions, because `paper` is the destination in one and the origin in the other.
#[test]
fn the_paper_tier_pins_a_row_through_its_sim_key_token() {
    let fx = SimFixture::migrated();

    let key = SIM_KEY;
    assert!(key.contains("SIM"), "the plant must carry the credential TOKEN: {key}");
    assert!(
        !key.to_ascii_uppercase().contains("PAPER"),
        "the plant must NOT carry the account WORD — that is what makes this a kill proof: {key}"
    );

    // Direction 1: the row is AT `paper` and its key spells `paper` through `SIM`.
    let paper = fx.id_of("okx", "paper");
    let err = fx.edit(AccountEdit::SetTier { id: paper, tier: "live" }).expect_err(
        "a SIM-keyed row is pinned to `paper` exactly as a LIVE-keyed row is to `live`",
    );
    match &err.kind {
        DbErrorKind::AccountKeysPinTheTier { tier, keys, .. } => {
            assert_eq!(tier, "paper", "the token SIM maps UP onto the account tier `paper`");
            assert_eq!(keys, &[key.to_string()]);
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert_eq!(fx.row(paper).expect("still there").tier, "paper", "nothing was written");

    // Direction 2: `paper` as the DESTINATION of a row whose keys spell something else.
    let live = fx.id_of("okx", "live");
    let err = fx
        .edit(AccountEdit::SetTier { id: live, tier: "paper" })
        .expect_err("a LIVE-keyed row may not be moved onto `paper` either");
    assert!(
        matches!(&err.kind, DbErrorKind::AccountKeysPinTheTier { tier, .. } if tier == "live"),
        "wrong refusal: {:?}",
        err.kind
    );
}

/// ⚠ **The families whose store token is no credential tier at all still pin their row**, and this
/// is the test that fails an implementation asking only the key GRAMMAR.
///
/// `vike_model::account_keys::account_ref_from_key` answers `None` for `DUKASCOPY_DEMO1_LOGIN` —
/// `DEMO1` is outside `CREDENTIAL_TIERS`, and that function's own doc names this family as one of
/// the two genuinely non-conforming ones. `vike_secrets`' `HAND_MAPPED_ACCOUNTS` is what classifies
/// it, to `demo`, and it is consulted FIRST for the reason that table states: a grammar-first order
/// would read `DUKASCOPY_DEMO1_` as spelling `DEMO` by prefix.
///
/// Without it this row moves freely, and dukascopy is the one venue where a tier and a label
/// together select a LEGAL ENTITY.
#[test]
fn a_hand_mapped_family_pins_its_tier_too() {
    let fx = Fixture::migrated();
    let id = fx.id_of("dukascopy", "demo");

    let err = fx
        .edit(AccountEdit::SetTier { id, tier: "live" })
        .expect_err("DUKASCOPY_DEMO1_* keys pin this row to `demo` through the hand map");
    match &err.kind {
        DbErrorKind::AccountKeysPinTheTier { tier, keys, .. } => {
            assert_eq!(tier, "demo");
            assert!(
                keys.iter().any(|k| k == "DUKASCOPY_DEMO1_LOGIN"),
                "the refusal names the hand-mapped keys: {keys:?}"
            );
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert_eq!(fx.row(id).expect("still there").tier, "demo");
}

// ---------------------------------------------------------------------------------------------
// The vocabulary, and the order the guards run in
// ---------------------------------------------------------------------------------------------

/// **A word outside `ACCOUNT_TIERS` is refused, the message NAMES the vocabulary, and nothing is
/// written.**
///
/// ⚠ **The retired `sim` spelling is in the list and is refused with the rest.**
/// `vike_secrets::account_tier_named` accepts it as an INPUT word — a credential key an operator
/// typed, a dotted settings key on disk — but nothing in this workspace WRITES it any more and a
/// migrated store's own `CHECK (tier IN ('paper','demo','live'))` would refuse the statement.
/// Storing `paper` for it silently would be this verb normalizing where `Create` beside it does
/// not. `PAPER` is in the list for the twin reason: the vocabulary is the three exact lowercase
/// words, and an uppercase spelling is a word the `CHECK` does not hold either.
#[test]
fn an_unknown_tier_is_refused_and_names_the_vocabulary() {
    let fx = Fixture::migrated();
    let id = fx.id_of("binance", "demo");

    for word in ["sim", "backend", "PAPER", "testnet", ""] {
        let err = fx
            .edit(AccountEdit::SetTier { id, tier: word })
            .expect_err("only `paper`, `demo` and `live` are tiers");
        assert!(
            matches!(err.kind, DbErrorKind::AccountTierUnknown),
            "`{word}` must be refused as an unknown TIER, not as something else: {:?}",
            err.kind
        );
        let msg = err.to_string();
        for tier in vike_secrets::ACCOUNT_TIERS {
            assert!(msg.contains(tier), "the message must name the whole vocabulary: {msg}");
        }
        assert_eq!(fx.row(id).expect("still there").tier, "demo", "nothing was written");
    }
}

/// **The store's refusal does not echo the word it was handed** — `AccountLabelMalformed`'s rule,
/// and for that variant's reason verbatim: on the surfaces that reach an account verb the flag
/// beside this one carries a credential VALUE, so a refusal that quoted its argument would be the
/// one channel here that prints an operator's secret back at them.
///
/// The probe is a token that appears nowhere in the message by construction. A real tier word could
/// not be used — the message names all three deliberately — and neither could `sim`, which the
/// message mentions in order to say it is retired. Choosing one of those and calling the pass a
/// result is exactly the assertion-that-cannot-fail this file's siblings are written against.
#[test]
fn the_unknown_tier_refusal_echoes_no_word() {
    let fx = Fixture::migrated();
    let probe = "ZZQX-not-a-tier-9137";
    let err = fx
        .edit(AccountEdit::SetTier { id: fx.id_of("binance", "demo"), tier: probe })
        .expect_err("refused");
    assert!(matches!(err.kind, DbErrorKind::AccountTierUnknown), "{:?}", err.kind);
    assert!(!err.to_string().contains(probe), "the refusal echoed its argument: {err}");
}

/// **The vocabulary is decided BEFORE the row is looked up**, and this is the check rather than a
/// sentence claiming it: the id below names NO ROW, so a row-first implementation answers
/// `NoSuchAccount` here and this test fails.
///
/// The reason the order matters is not tidiness. The keys guard compares an `ACCOUNT_TIERS` word
/// against the target, so a target outside the vocabulary makes EVERY credential key read as a
/// contradiction — and without this ordering a typo in `--tier` reaches the operator as a refusal
/// about their credentials.
#[test]
fn the_tier_guard_runs_before_the_row_lookup() {
    let fx = Fixture::migrated();
    let err = fx
        .edit(AccountEdit::SetTier { id: 9999, tier: "backend" })
        .expect_err("both facts are wrong; the question is WHICH is reported");
    assert!(
        matches!(err.kind, DbErrorKind::AccountTierUnknown),
        "the vocabulary is decided before the store is consulted: {:?}",
        err.kind
    );
    // ...and the control that makes the assertion above mean something: with a LEGAL tier, the same
    // id answers `NoSuchAccount`. Without this, the test above would pass over an implementation
    // that answered `AccountTierUnknown` for everything.
    let err = fx
        .edit(AccountEdit::SetTier { id: 9999, tier: "demo" })
        .expect_err("id 9999 carries no row");
    assert!(
        matches!(err.kind, DbErrorKind::NoSuchAccount { id: 9999 }),
        "a legal tier must reach the row lookup: {:?}",
        err.kind
    );
}

// ---------------------------------------------------------------------------------------------
// The destination's own guards
// ---------------------------------------------------------------------------------------------

/// ⚠ **The row crosses `UNIQUE (venue, tier, label)`, so the destination tier's guards apply — and
/// the sharpest of them is the one no index can make.** SQLite's NULLs are distinct, so the engine
/// takes a second unlabelled row at one `(venue, tier)` happily; what breaks is `AccountResolver`'s
/// `by_key`, whose `(venue, tier, None, None)` entry then names two rows and makes the NEXT
/// credential key written for that venue refuse as ambiguous. A move that allowed it would be
/// arming a refusal for a write nobody has made yet.
#[test]
fn moving_an_unlabelled_row_onto_a_tier_that_has_one_is_refused() {
    let fx = Fixture::migrated();
    let occupied = fx.id_of("binance", "demo");
    let made = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "paper", label: None })
        .expect("binance has no unlabelled paper row yet");
    let moving = made.after.expect("a create returns its row").id;

    let err = fx
        .edit(AccountEdit::SetTier { id: moving, tier: "demo" })
        .expect_err("two unlabelled binance/demo rows is the AmbiguousAccount plant");
    match &err.kind {
        DbErrorKind::AmbiguousUnlabelledAccount { venue, tier, holder } => {
            assert_eq!(venue, "binance");
            assert_eq!(tier, "demo", "the refusal names the DESTINATION tier");
            assert_eq!(*holder, occupied, "and the row that is already there");
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert_eq!(fx.row(moving).expect("still there").tier, "paper", "nothing was written");
}

/// A label another row of the DESTINATION `(venue, tier)` already carries is refused the same way a
/// create is — the index is the authority and the pre-check exists so the refusal can name the
/// other row.
#[test]
fn moving_onto_a_tier_where_the_label_is_taken_is_refused() {
    let fx = Fixture::migrated();
    let taken = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("ALT") })
        .expect("a labelled live row")
        .after
        .expect("the row")
        .id;
    let moving = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "paper", label: Some("ALT") })
        .expect("the same label at a DIFFERENT tier is a different account")
        .after
        .expect("the row")
        .id;

    let err = fx
        .edit(AccountEdit::SetTier { id: moving, tier: "live" })
        .expect_err("two binance/live rows may not both be ALT");
    match &err.kind {
        DbErrorKind::AccountLabelTaken { venue, tier, label, holder } => {
            assert_eq!((venue.as_str(), tier.as_str(), label.as_str()), ("binance", "live", "ALT"));
            assert_eq!(*holder, taken);
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    assert_eq!(fx.row(moving).expect("still there").tier, "paper");
}

// ---------------------------------------------------------------------------------------------
// Idempotence and the id
// ---------------------------------------------------------------------------------------------

/// Re-running a command is not a mistake — `AccountWrite::changed`'s rule, and refusing the second
/// run would break a script that re-asserts a known state. The no-op path still commits, so the
/// result carries the row and the key names like any other.
#[test]
fn moving_a_row_to_the_tier_it_already_carries_changes_nothing_and_says_so() {
    let fx = Fixture::migrated();
    let id = fx.id_of("binance", "live");

    let w = fx
        .edit(AccountEdit::SetTier { id, tier: "live" })
        .expect("a row already at this tier is not an error — and is not blocked by its own keys");
    assert_eq!(w.verb, "set-tier");
    assert!(!w.changed, "nothing was written");
    assert_eq!(w.before.expect("the row as found").tier, "live");
    assert_eq!(w.after.expect("the row as it stands").tier, "live");
    assert_eq!(w.keys, vec!["BINANCE_LIVE_API_KEY".to_string()]);
}

/// A move must not disturb the `credential` table at all: the rows stay filed against the SAME
/// account id under the SAME names, which is precisely why the keys guard above has to exist.
#[test]
fn a_move_writes_no_credential_row() {
    // ⚠ NAMES and a COUNT, never the map — `the_lifecycle_touches_no_credential_row_and_no_file`'s
    // shape, and for its reason: a failing `assert_eq!` over the map would print credential VALUES
    // into the test output, on a file published to the public mirror.
    let names = |fx: &Fixture| -> Vec<String> {
        let table = vike_secrets::read_table(&fx.db(), vike_secrets::Table::Credential)
            .expect("read the credential table");
        let mut out: Vec<String> = table.keys().map(str::to_string).collect();
        out.sort();
        out
    };

    let fx = Fixture::migrated();
    let before = names(&fx);
    let file_before = std::fs::read_to_string(fx.store()).expect("the file store");

    let made = fx
        .edit(AccountEdit::Create { venue: "bybit", tier: "paper", label: Some("ALT") })
        .expect("a fresh keyless row")
        .after
        .expect("the row")
        .id;
    fx.edit(AccountEdit::SetTier { id: made, tier: "demo" }).expect("moves");

    assert_eq!(names(&fx), before, "a tier move may not touch a credential row");
    assert_eq!(
        std::fs::read_to_string(fx.store()).expect("the file store"),
        file_before,
        "and the credential FILE is byte-identical — nothing here writes, moves or deletes one"
    );
}

// ---------------------------------------------------------------------------------------------
// The SIM fixture
// ---------------------------------------------------------------------------------------------

/// The one credential key in this file that is NOT in `support::FIXTURE_KEYS`. It is here rather
/// than there because that constant is shared with two other test binaries and widening it would
/// change what THEY migrate; a key this file alone needs belongs to this file.
const SIM_KEY: &str = "OKX_SIM_API_KEY";

/// A second store, carrying the `{VENUE}_SIM_*` family `support::Fixture` has no row for — the one
/// shape [`the_paper_tier_pins_a_row_through_its_sim_key_token`] is about.
///
/// Deliberately NOT a second copy of `support::Fixture`: it borrows that type for everything except
/// the key set, by writing its own store text and running the same public `migrate`.
struct SimFixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl SimFixture {
    /// `OKX_SIM_*` (the paper account) and `OKX_LIVE_*` (the live one), classified the way
    /// `vike_bridge_core::credentials::classify_credential_name` classifies them — spelled here
    /// because this crate cannot link the crate that owns that function, the same seam
    /// `support::classify` carries.
    fn migrated() -> SimFixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::write(
            settings.join("secrets.env"),
            format!("{SIM_KEY}=value-for-sim\nOKX_LIVE_API_KEY=value-for-live\n"),
        )
        .expect("write fixture store");

        let fx = SimFixture { _dir: dir, settings };
        let classify = |name: &str| -> vike_secrets::Classification {
            use vike_secrets::{AccountKey, Classification, Placement};
            for (prefix, tier) in [("OKX_SIM_", "paper"), ("OKX_LIVE_", "live")] {
                if let Some(field) = name.strip_prefix(prefix) {
                    return Classification {
                        placement: Placement::Account(AccountKey {
                            venue: "okx".to_string(),
                            tier: tier.to_string(),
                            label: None,
                            discriminator: None,
                        }),
                        field: field.to_string(),
                        secret: true,
                        recognised: true,
                        pending_move: None,
                    };
                }
            }
            Classification::unrecognised(name)
        };
        match vike_secrets::migrate(fx.arg(), support::is_node_key, &classify) {
            Ok(_) => fx,
            Err(e) => panic!("migration refused: {e}"),
        }
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    fn accounts(&self) -> Vec<vike_secrets::Account> {
        match vike_secrets::resolve_accounts_in(&self.settings).expect("the store opened") {
            vike_secrets::Accounts::Known(rows) => rows,
            vike_secrets::Accounts::Unanswerable(why) => {
                panic!("the store could not be asked: {why}")
            }
        }
    }

    fn row(&self, id: i64) -> Option<vike_secrets::Account> {
        self.accounts().into_iter().find(|a| a.id == id)
    }

    fn id_of(&self, venue: &str, tier: &str) -> i64 {
        let rows = self.accounts();
        let hit: Vec<&vike_secrets::Account> =
            rows.iter().filter(|a| a.venue == venue && a.tier == tier).collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{tier} row: {hit:?}");
        hit[0].id
    }

    fn edit(
        &self,
        edit: AccountEdit<'_>,
    ) -> Result<vike_secrets::AccountWrite, vike_secrets::DbError> {
        vike_secrets::edit_account_in(&self.settings, edit)
    }
}
