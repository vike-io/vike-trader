//! The gate on `vike_tradehub_core::book_identity` — the per-venue answer to "which venue BOOK does this
//! account actually trade", which is the whole input to the shared-book warning.
//!
//! Three things it has to prove, and they fail in different directions:
//!
//! 1. **COMPLETENESS** over `vike_model::VENUES`, the repo's standard shape for a per-venue table
//!    (`vike_venues::venue_caps`, `vike_venues::fees`, `vike_venues::venue_tif`): a new bridge crate
//!    reddens this until somebody has READ that venue's credential loader and said whether anything
//!    in the store names the account. A missing row would otherwise behave exactly like a row that
//!    says "cannot tell", and the two are not the same claim.
//! 2. **The determinable venues actually resolve**, from the real key names, through the real
//!    `account_var` label grammar — including the EVM derivation, which is the only path that can
//!    catch an agent key configured beside the master whose own key is also present.
//! 3. **The undeterminable venues resolve to `None` even with a full credential set present.** That
//!    is the "warn nothing where you cannot tell" contract, and it is what stops the rule
//!    manufacturing a pair out of two unknowns.
//!
//! Moved from `crates/vike-tradehub-core/tests/` when the venue-adapter contract finished
//! (docs/decisions/0096): it drives `vike-tradehub-core`'s public fold with real venue ids, which only a
//! crate holding the registry can — `vike-tradehub` since the 2026-09-29 amendment. Default build
//! only: `vike-tradehub-core`'s registry carried ibkr, fxcm and polymarket `FeatureAbsent` in every build
//! and these assertions were written against that; each venue's feature-on half is its own
//! `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`, `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` or
//! `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`. That crate-level `#![cfg]` also makes this its
//! own test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

#[path = "support/account_row.rs"]
mod account_row;

use std::collections::BTreeMap;
use std::path::PathBuf;

use vike_bridge_core::venue_adapter::BookIdentity;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::trade::adapters::book_identity::{
    book_identity_for, confirmation_for_account, effective_book, normalize_book,
};
use vike_model::AccountTier;
use vike_model::VENUES;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{AccountKeys, Accounts, NoAccountTable};

use account_row::acct;

fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

/// The book `effective_book` derives for an account of `venue` at `mode` holding `pairs`.
fn book_of(venue: &str, mode: AccountTier, pairs: &[(&str, &str)]) -> Option<String> {
    let held = fields(pairs);
    effective_book(REGISTRY, venue, mode, AccountCredentials::new(Some(1), Some(&held), None))
}

/// **THE COMPLETENESS TEST.** Every roster venue is classified. That each is classified exactly
/// once, and that no row names a venue the roster does not carry, is the registry's own partition
/// test (`crates/vike-gatewayhub/tests/trade_roster/registry_shape/registry.rs`'s `the_registry_partitions_the_roster`):
/// each row's answer is its bridge's `VenueDeclaration::book_identity` since the venue start
/// contract deleted vike-tradehub-core's `BOOK_IDENTITY` table (docs/decisions/0096).
#[test]
fn every_roster_venue_has_exactly_one_row() {
    let mut missing = Vec::new();
    for venue in VENUES {
        if book_identity_for(REGISTRY, venue).is_none() {
            missing.push(*venue);
        }
    }
    assert!(
        missing.is_empty(),
        "these venues declare no book identity: {missing:?}. Read that venue's own credential \
         loader and say whether anything in the store NAMES the account it trades — an address, a \
         login, a numeric account id. `BookIdentity::Undeterminable` (with the reason) is a \
         legitimate answer for a key/secret venue; a MISSING row is not, because it behaves like \
         one while claiming nothing."
    );
}

/// Every row says something usable: a `Named` row can compose at least one key, and an
/// `Undeterminable` row carries the reason that makes it a CLASSIFICATION rather than a gap.
#[test]
fn every_row_is_substantive() {
    for venue in VENUES {
        let Some(kind) = book_identity_for(REGISTRY, venue) else { continue };
        match kind {
            BookIdentity::Named { name_fields, evm_key_fields } => {
                assert!(
                    !name_fields.is_empty() || !evm_key_fields.is_empty(),
                    "{venue}: a Named row needs at least one field to read"
                );
                for f in name_fields.iter().chain(evm_key_fields.iter()) {
                    assert!(
                        !f.is_empty() && !f.starts_with('_'),
                        "{venue}: {f:?} is not a credential FIELD word"
                    );
                }
            }
            BookIdentity::Undeterminable { why } => {
                assert!(
                    why.len() > 30,
                    "{venue}: an Undeterminable row must say WHY the store cannot name the \
                     account, in a sentence somebody can check against that venue's loader — got \
                     {why:?}"
                );
            }
        }
    }
}

/// **The determinable venues, from their REAL key names.** One case per `Named` row, so a row whose
/// prefix or tier token was copied wrong reads no key and fails here rather than silently
/// downgrading a determinable venue to "cannot tell".
#[test]
fn every_named_row_resolves_from_its_own_keys() {
    // (venue, mode, the FIELD that names the book, its value, the expected answer)
    let cases: &[(&str, AccountTier, &str, &str, &str)] = &[
        ("hyperliquid", AccountTier::Demo, "ACCOUNT_ADDRESS", "0xMaStEr", "0xmaster"),
        ("hyperliquid", AccountTier::Live, "ACCOUNT_ADDRESS", "0xAbC", "0xabc"),
        ("aster", AccountTier::Demo, "USER", "0xUSER", "0xuser"),
        ("aster", AccountTier::Live, "USER", "0xUSER", "0xuser"),
        ("oanda", AccountTier::Demo, "ACCOUNT_ID", "101-004-1", "101-004-1"),
        ("oanda", AccountTier::Live, "ACCOUNT_ID", "001-004-9", "001-004-9"),
        ("alpaca", AccountTier::Demo, "ACCOUNT_ID", "acct-1", "acct-1"),
        ("ctrader", AccountTier::Demo, "ACCOUNT_ID", "12345", "12345"),
        ("ig", AccountTier::Demo, "IDENTIFIER", "myuser", "myuser"),
    ];
    let mut seen: Vec<&str> = Vec::new();
    for (venue, mode, field, value, expected) in cases {
        seen.push(venue);
        assert_eq!(
            book_of(venue, *mode, &[(field, value)]).as_deref(),
            Some(*expected),
            "{venue}: the {field} field must name the book"
        );
        // …and an account holding nothing has no book: the no-fallback rule every credential
        // loader in this workspace obeys, now that the start hands a bridge ONE account's fields.
        assert_eq!(
            effective_book(REGISTRY, venue, *mode, AccountCredentials::none()),
            None,
            "{venue}: an account with no credentials names no book"
        );
    }

    // …and every `Named` row is covered by at least one case above, so a row added with no case
    // fails here rather than being asserted about by nobody.
    for venue in VENUES {
        if matches!(book_identity_for(REGISTRY, venue), Some(BookIdentity::Named { .. })) {
            assert!(seen.contains(venue), "no resolution case for the Named row {venue:?}");
        }
    }
}

/// **The EVM fallback**: a hyperliquid account whose key IS the account (no `_ACCOUNT_ADDRESS`)
/// resolves to the address derived from the key — the same derivation its own signer performs.
///
/// This path is the one that can catch the shape the whole warning exists for: an AGENT wallet
/// configured under one label with its master's address, beside the MASTER's own key under another.
#[test]
fn an_evm_key_derives_the_address_when_no_explicit_one_is_written() {
    // The canonical EIP-712 "Cow" key (keccak256("cow")) and its known EOA — the same vector
    // `crates/bridges/aster/src/signing.rs`'s tests use, so the expected address is not this file's
    // own invention.
    const COW_KEY: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";
    const COW_ADDR: &str = "0xcd2a3d9f938e13cd947ec05abc7fe734df8dd826";

    assert_eq!(
        book_of("hyperliquid", AccountTier::Live, &[("PRIVATE_KEY", COW_KEY)]).as_deref(),
        Some(COW_ADDR)
    );

    // …and an EXPLICIT address WINS over the derivation, because it is the venue's own statement of
    // which account this is. That is the agent-wallet case: the key is the agent, the address is the
    // master, and the master is the book.
    assert_eq!(
        book_of(
            "hyperliquid",
            AccountTier::Live,
            &[("PRIVATE_KEY", COW_KEY), ("ACCOUNT_ADDRESS", "0xMASTER")]
        )
        .as_deref(),
        Some("0xmaster"),
        "the explicit master address is the book; the agent key's own address is not"
    );

    // A key this cannot parse is not a finding — the start is about to refuse it anyway, and an
    // identity derived from nothing is worse than no identity.
    assert_eq!(book_of("hyperliquid", AccountTier::Live, &[("PRIVATE_KEY", "not-a-key")]), None);
}

/// **The undeterminable venues answer `None` with a FULL credential set present** — which is the
/// contract, not a gap: nothing in a key/secret store names the account, so there is no finding to
/// make and none is made.
#[test]
fn an_undeterminable_venue_answers_none_even_fully_credentialled() {
    // The generic HMAC set (plus OKX's passphrase) and a login pair: nothing in any of them names an
    // account.
    let held: &[&str] = &["API_KEY", "API_SECRET", "API_PASSPHRASE", "LOGIN", "PASSWORD"];
    let all: Vec<(&str, &str)> = held.iter().map(|f| (*f, "x")).collect();
    for venue in VENUES {
        if matches!(book_identity_for(REGISTRY, venue), Some(BookIdentity::Undeterminable { .. })) {
            for mode in [AccountTier::Demo, AccountTier::Live] {
                assert_eq!(
                    book_of(venue, mode, &all),
                    None,
                    "{venue} is classified Undeterminable and must answer None"
                );
            }
        }
    }
}

/// A PAPER account trades no venue book, so it has none to share — the shortest arm of the
/// resolution and the one that keeps a paper box out of the report entirely.
#[test]
fn a_paper_account_has_no_book() {
    assert_eq!(book_of("hyperliquid", AccountTier::Paper, &[("ACCOUNT_ADDRESS", "0xabc")]), None);
}

/// A venue the roster does not carry has no loader to read and no row to consult.
#[test]
fn a_non_roster_venue_has_no_row_and_no_book() {
    assert!(book_identity_for(REGISTRY, "sim").is_none());
    assert_eq!(book_of("sim", AccountTier::Live, &[("ACCOUNT_ADDRESS", "0xabc")]), None);
}

/// Normalization is the PRODUCER's job, and this pins which end owns it —
/// `vike_config::shared_books` compares text and does no folding of its own.
#[test]
fn the_book_is_normalized_here_not_in_the_rule() {
    assert_eq!(normalize_book("  0xAbC \n"), "0xabc");
    assert_eq!(normalize_book("DUQ186573"), "duq186573");
}

/// Only the RESOLVED tier's ACCOUNT is read. A store holding a demo account and a live account of
/// one venue describes two books at two endpoints, and each start reads its own account's fields:
/// `book_of_account` at `demo` never answers with the live account's address.
#[test]
fn only_the_resolved_tiers_account_is_read() {
    let policy = vike_tradehub_core::AdapterPolicy::default()
        .with_credentials(
            "hyperliquid",
            &AccountLabel::Default,
            AccountTier::Demo,
            &[("ACCOUNT_ADDRESS", "0xdemo")],
        )
        .with_credentials(
            "hyperliquid",
            &AccountLabel::Default,
            AccountTier::Live,
            &[("ACCOUNT_ADDRESS", "0xlive")],
        );
    let book = |mode| {
        vike_gatewayhub::trade::adapters::book_identity::book_of_account(
            REGISTRY,
            "hyperliquid",
            &AccountLabel::Default,
            mode,
            &policy.accounts,
        )
    };
    assert_eq!(book(AccountTier::Demo).as_deref(), Some("0xdemo"));
    assert_eq!(book(AccountTier::Live).as_deref(), Some("0xlive"));
}

// ── THE RECORD A START PARKS WHEN THE VENUE NAMES A BOOK ─────────────────────────────────────
//
// `confirmation_for_account` is the ADDRESS half of the handshake fold — the generic twin of what
// the dukascopy start has built for one venue since 2026-09-15 (`vike_tradehub_core`'s dukascopy arm then,
// `crates/bridges/dukascopy/src/adapter.rs` since the venue-adapter contract), now reachable by any
// venue whose start performs a real handshake (today: hyperliquid's `userRole` probe).
//
// The DISPOSITION half is `vike_secrets::account_confirmation::verdict` and is tested in that crate.
// Nothing here re-tests it; what these tests hold is that the record ADDRESSES the row
// `vike-cli secrets confirm` will resolve, and answers `None` rather than parking something nothing
// can ever fold.

fn keys_for(rows: &[(i64, &[&str])]) -> BTreeMap<i64, AccountKeys> {
    rows.iter()
        .map(|(id, prefixes)| {
            (
                *id,
                AccountKeys {
                    prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
                    names: Vec::new(),
                },
            )
        })
        .collect()
}

/// The the CI box shape, measured 2026-09-19: one unlabelled `(hyperliquid, live)` row owning
/// `HYPERLIQUID_LIVE_`, with no book recorded yet.
fn prod2_shape(book: Option<&str>) -> AccountDirectory {
    AccountDirectory::from_rows(
        Accounts::Known(vec![acct(11, "hyperliquid", "live", None, book)]),
        Some(keys_for(&[(11, &["HYPERLIQUID_LIVE_"])])),
    )
}

/// **THE RECORD, ADDRESSED THE WAY THE FOLD LOOKS IT UP.** `vike-cli secrets confirm` resolves a
/// parked record by `prefixes.contains(&rec.key_prefix)` over the row's own key prefixes, so the
/// prefix here is READ FROM THE ROW rather than composed from the venue and tier — a composed
/// `HYPERLIQUID_LIVE_` would be a second derivation free to disagree with the store's.
#[test]
fn the_parked_record_carries_the_rows_own_prefix_and_the_row_as_evidence() {
    let d = prod2_shape(None);
    let r = confirmation_for_account(
        "hyperliquid",
        &AccountLabel::Default,
        AccountTier::Live,
        "0xMaStEr",
        &d,
        1_700_000_000_000,
    )
    .expect("one row, one prefix");
    assert_eq!(r.venue, "hyperliquid");
    assert_eq!(r.key_prefix, "HYPERLIQUID_LIVE_", "the address the fold matches on");
    assert_eq!(r.observed_row, Some(11), "the row rides along as EVIDENCE, never as the address");
    assert_eq!(r.observed_book, None, "not yet known — and that is not `no book`");
    assert_eq!(r.at_ms, 1_700_000_000_000, "the HANDSHAKE's instant, not the fold's");
    // ⚠ NORMALIZED, for the reason `normalize_book` states: the answer this parks becomes the book
    // the fold writes, and the shared-book report compares it against a derivation that is
    // normalized. Two spellings of one address would report one account as two.
    assert_eq!(r.handshake_account_id, "0xmaster");
}

/// **WHAT THE STORE HELD AT THE HANDSHAKE RIDES ALONG**, so the fold can see whether the row moved
/// between the two moments — and so the start's own log can state a verdict without a second read.
#[test]
fn a_row_that_already_names_a_book_carries_it_as_observed() {
    let d = prod2_shape(Some("0xABC"));
    let r = confirmation_for_account(
        "hyperliquid",
        &AccountLabel::Default,
        AccountTier::Live,
        "0xabc",
        &d,
        1,
    )
    .expect("one row, one prefix");
    assert_eq!(r.observed_book.as_deref(), Some("0xabc"), "normalized on BOTH sides");
    // …and the shared verdict then reads `Confirms` rather than manufacturing a disagreement out of
    // one address spelled two ways, which would fire on every boot.
    assert_eq!(
        vike_secrets::account_confirmation::verdict(
            &r.handshake_account_id,
            r.observed_book.as_deref()
        ),
        vike_secrets::account_confirmation::Verdict::Confirms
    );
}

/// **⚠ A LABELLED ACCOUNT IS ADDRESSED BY `(tier, label)`, BECAUSE IT HAS NO PREFIX TO BE ADDRESSED
/// BY.**
///
/// `vike_secrets::read_account_keys` derives a prefix by stripping a credential's `field` from the
/// END of its NAME; for a labelled key the `field` is not a suffix (`__LABEL` sits after it), so a
/// labelled account's row carries an EMPTY prefix list. This was a dead end until 2026-09-19 — a
/// record parked for such an account matched no row in `secrets confirm`'s lookup and would be KEPT
/// forever — and `confirmation_for_account` answered `None` rather than park one.
///
/// It now carries the store's OWN answer for a labelled account instead: `UNIQUE (venue, tier,
/// label)`, the index that constrains nothing while every label is NULL and bites the moment one is
/// not. Both halves come off the ROW, never from the caller's `label`/`mode`, so the start and the
/// fold cannot address different rows.
#[test]
fn a_labelled_account_is_addressed_by_its_tier_and_label() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let d = AccountDirectory::from_rows(
        Accounts::Known(vec![acct(12, "hyperliquid", "live", Some("ALT"), None)]),
        // The row is real and the fold can see it — it simply owns no PREFIX, which is exactly what
        // the store answers for a labelled account.
        Some(keys_for(&[(12, &[])])),
    );
    let r = confirmation_for_account("hyperliquid", &alt, AccountTier::Live, "0xAbC", &d, 9)
        .expect("a labelled account is addressable now");
    assert_eq!(r.label.as_deref(), Some("ALT"), "…by the label the ROW carries");
    assert_eq!(
        r.tier,
        Some(AccountTier::Live),
        "…and the tier, because the label alone is not unique"
    );
    assert_eq!(r.key_prefix, "", "no prefix exists for one — the empty string IS the shape");
    assert_eq!(r.observed_row, Some(12), "the row still rides along as evidence");
    assert_eq!(r.handshake_account_id, "0xabc", "normalized, like every other producer");
}

/// **AN UNLABELLED ACCOUNT KEEPS THE PREFIX ADDRESS AND GAINS NOTHING**, which is what makes a
/// record written by this build and one written before labels were addressable the SAME entry
/// rather than two.
///
/// ⚠ The tier is deliberately left absent even though it is known: the prefix already encodes it
/// (`HYPERLIQUID_LIVE_`), and filling it in would change the record's `address()` so the same
/// account would merge to two entries across an upgrade.
#[test]
fn an_unlabelled_account_carries_no_label_address_at_all() {
    let d = prod2_shape(None);
    let r = confirmation_for_account(
        "hyperliquid",
        &AccountLabel::Default,
        AccountTier::Live,
        "0xabc",
        &d,
        9,
    )
    .expect("one row, one prefix");
    assert_eq!(r.key_prefix, "HYPERLIQUID_LIVE_");
    assert_eq!(r.label, None, "an unlabelled account is addressed by its prefix alone");
    assert_eq!(r.tier, None, "…and carries no tier, so an upgrade does not split its entry in two");
}

/// **A LABELLED ROW WITH NO PREFIX IS NOW ADDRESSABLE, BUT AN UNLABELLED ONE WITH NO PREFIX IS STILL
/// NOT.** The two look identical in the key table and are different accounts: a row can outlive the
/// last credential that created it (`vike_secrets::AccountKeys::prefixes` says so), and for an
/// unlabelled one there is then nothing left that names it.
#[test]
fn an_unlabelled_row_that_owns_no_prefix_still_parks_nothing() {
    let d = AccountDirectory::from_rows(
        Accounts::Known(vec![acct(11, "hyperliquid", "live", None, None)]),
        Some(keys_for(&[(11, &[])])),
    );
    assert_eq!(
        confirmation_for_account(
            "hyperliquid",
            &AccountLabel::Default,
            AccountTier::Live,
            "0xa",
            &d,
            1
        ),
        None
    );
}

/// **NO SINGLE ROW ⇒ NOTHING TO CONFIRM**, in every shape that produces one: a store nobody read, a
/// box with no `account` table, an account whose only row is RETIRED, and two rows that both match.
///
/// They all arrive at the join as one `None`, and each is a real box. The last is the dukascopy
/// shape wearing another venue: picking either row would file one account's handshake against
/// another's.
#[test]
fn every_shape_with_no_single_row_parks_nothing() {
    let ask = |d: &AccountDirectory| {
        confirmation_for_account(
            "hyperliquid",
            &AccountLabel::Default,
            AccountTier::Live,
            "0xabc",
            d,
            1,
        )
    };
    assert_eq!(ask(&AccountDirectory::unread()), None, "nobody read the store");

    let file_store = AccountDirectory::from_rows(
        Accounts::Unanswerable(NoAccountTable::NoStore { db: PathBuf::from("/x/db/vike.db") }),
        None,
    );
    assert_eq!(ask(&file_store), None, "a box with no settings database carries no account table");

    let mut retired = acct(11, "hyperliquid", "live", None, None);
    retired.active = false;
    let inactive = AccountDirectory::from_rows(
        Accounts::Known(vec![retired]),
        Some(keys_for(&[(11, &["HYPERLIQUID_LIVE_"])])),
    );
    assert_eq!(ask(&inactive), None, "a retired account is not confirmed back into use");

    let two = AccountDirectory::from_rows(
        Accounts::Known(vec![
            acct(11, "hyperliquid", "live", None, None),
            acct(13, "hyperliquid", "live", None, None),
        ]),
        Some(keys_for(&[(11, &["HYPERLIQUID_LIVE_"]), (13, &["HYPERLIQUID_LIVE2_"])])),
    );
    assert_eq!(ask(&two), None, "two rows match and neither may be picked");
}

/// **THE TIER BINDS**, so a LIVE start never parks a confirmation against the DEMO row — which on
/// the measured the CI box store is a real pair of rows (10 demo, 11 live) differing in nothing else.
/// Getting this wrong would stamp one account's handshake onto the other's row.
#[test]
fn a_live_start_does_not_confirm_the_demo_row() {
    let d = AccountDirectory::from_rows(
        Accounts::Known(vec![
            acct(10, "hyperliquid", "demo", None, None),
            acct(11, "hyperliquid", "live", None, None),
        ]),
        Some(keys_for(&[(10, &["HYPERLIQUID_DEMO_"]), (11, &["HYPERLIQUID_LIVE_"])])),
    );
    let live = confirmation_for_account(
        "hyperliquid",
        &AccountLabel::Default,
        AccountTier::Live,
        "0xa",
        &d,
        1,
    )
    .expect("the live row");
    assert_eq!(live.observed_row, Some(11));
    assert_eq!(live.key_prefix, "HYPERLIQUID_LIVE_");

    let demo = confirmation_for_account(
        "hyperliquid",
        &AccountLabel::Default,
        AccountTier::Demo,
        "0xb",
        &d,
        1,
    )
    .expect("the demo row");
    assert_eq!(demo.observed_row, Some(10));
    assert_eq!(demo.key_prefix, "HYPERLIQUID_DEMO_");
}

/// **A PAPER ACCOUNT CONFIRMS NOTHING.** It touches no venue book, so there is no handshake to
/// record — the same first line `effective_book` and `recorded_book` both have.
#[test]
fn a_paper_account_parks_nothing() {
    let d = prod2_shape(None);
    assert_eq!(
        confirmation_for_account(
            "hyperliquid",
            &AccountLabel::Default,
            AccountTier::Paper,
            "0xabc",
            &d,
            1
        ),
        None
    );
}
