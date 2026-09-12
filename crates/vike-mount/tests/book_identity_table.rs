//! The gate on `vike_mount::book_identity` — the per-venue answer to "which venue BOOK does this
//! account actually trade", which is the whole input to the shared-book warning.
//!
//! Three things it has to prove, and they fail in different directions:
//!
//! 1. **COMPLETENESS** over `vike_model::VENUES`, the repo's standard shape for a per-venue table
//!    (`vike_model::venue_caps`, `vike_model::fees`, `vike_bridge_core::tif`): a new bridge crate
//!    reddens this until somebody has READ that venue's credential loader and said whether anything
//!    in the store names the account. A missing row would otherwise behave exactly like a row that
//!    says "cannot tell", and the two are not the same claim.
//! 2. **The determinable venues actually resolve**, from the real key names, through the real
//!    `account_var` label grammar — including the EVM derivation, which is the only path that can
//!    catch an agent key configured beside the master whose own key is also present.
//! 3. **The undeterminable venues resolve to `None` even with a full credential set present.** That
//!    is the "warn nothing where you cannot tell" contract, and it is what stops the rule
//!    manufacturing a pair out of two unknowns.

use std::collections::HashMap;

use vike_config::VenueMode;
use vike_model::VENUES;
use vike_model::account_keys::AccountLabel;
use vike_mount::book_identity::{
    BOOK_IDENTITY, BookIdentity, book_identity_for, effective_book, normalize_book,
};

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// **THE COMPLETENESS TEST.** Every roster venue is classified, exactly once, and no row names a
/// venue the roster does not carry.
#[test]
fn every_roster_venue_has_exactly_one_row() {
    let mut missing = Vec::new();
    for venue in VENUES {
        if book_identity_for(venue).is_none() {
            missing.push(*venue);
        }
    }
    assert!(
        missing.is_empty(),
        "these venues have no `BOOK_IDENTITY` row: {missing:?}. Read that venue's own credential \
         loader and say whether anything in the store NAMES the account it trades — an address, a \
         login, a numeric account id. `BookIdentity::Undeterminable` (with the reason) is a \
         legitimate answer for a key/secret venue; a MISSING row is not, because it behaves like \
         one while claiming nothing."
    );

    let mut ids: Vec<&str> = BOOK_IDENTITY.iter().map(|(v, _)| *v).collect();
    let declared = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), declared, "duplicate venue row in BOOK_IDENTITY");
    for (venue, _) in BOOK_IDENTITY {
        assert!(VENUES.contains(venue), "BOOK_IDENTITY names {venue:?}, which is not a roster id");
    }
    assert_eq!(declared, VENUES.len(), "one row per roster venue, no more");
}

/// Every row says something usable: a `Named` row can compose at least one key, and an
/// `Undeterminable` row carries the reason that makes it a CLASSIFICATION rather than a gap.
#[test]
fn every_row_is_substantive() {
    for (venue, kind) in BOOK_IDENTITY {
        match kind {
            BookIdentity::Named {
                prefix,
                demo_tiers,
                live_tiers,
                name_suffixes,
                evm_key_suffixes,
            } => {
                assert!(!prefix.is_empty(), "{venue}: a Named row needs a key prefix");
                assert!(
                    !demo_tiers.is_empty() || !live_tiers.is_empty(),
                    "{venue}: a Named row that can reach NO tier can never answer"
                );
                assert!(
                    !name_suffixes.is_empty() || !evm_key_suffixes.is_empty(),
                    "{venue}: a Named row needs at least one key to read"
                );
                for t in demo_tiers.iter().chain(live_tiers.iter()) {
                    assert!(
                        !t.is_empty(),
                        "{venue}: an empty tier token composes `PREFIX__SUFFIX`"
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
    // (venue, mode, the key that names the book, its value, the expected answer)
    let cases: &[(&str, VenueMode, &str, &str, &str)] = &[
        (
            "hyperliquid",
            VenueMode::Demo,
            "HYPERLIQUID_DEMO_ACCOUNT_ADDRESS",
            "0xMaStEr",
            "0xmaster",
        ),
        ("hyperliquid", VenueMode::Live, "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS", "0xAbC", "0xabc"),
        ("aster", VenueMode::Demo, "ASTER_TESTNET_USER", "0xUSER", "0xuser"),
        ("aster", VenueMode::Live, "ASTER_LIVE_USER", "0xUSER", "0xuser"),
        ("polymarket", VenueMode::Live, "POLY_LIVE_ADDRESS", "0xFunder", "0xfunder"),
        // …and polymarket's LEGACY live tier, which its own loader still accepts.
        ("polymarket", VenueMode::Live, "POLY_MAINNET_ADDRESS", "0xFunder", "0xfunder"),
        ("oanda", VenueMode::Demo, "OANDA_DEMO_ACCOUNT_ID", "101-004-1", "101-004-1"),
        ("oanda", VenueMode::Live, "OANDA_MAINNET_ACCOUNT_ID", "001-004-9", "001-004-9"),
        ("alpaca", VenueMode::Demo, "ALPACA_SANDBOX_ACCOUNT_ID", "acct-1", "acct-1"),
        ("ibkr", VenueMode::Demo, "IBKR_DEMO_ACCOUNT", "DUQ186573", "duq186573"),
        ("ibkr", VenueMode::Live, "IBKR_LIVE_ACCOUNT", "U13112916", "u13112916"),
        ("ctrader", VenueMode::Demo, "CTRADER_DEMO_ACCOUNT_ID", "12345", "12345"),
        ("ig", VenueMode::Demo, "IG_DEMO_IDENTIFIER", "myuser", "myuser"),
        ("fxcm", VenueMode::Demo, "FXCM_DEMO_USER", "D251112911", "d251112911"),
    ];
    let mut seen: Vec<&str> = Vec::new();
    for (venue, mode, key, value, expected) in cases {
        seen.push(venue);
        let v = vars(&[(key, value)]);
        assert_eq!(
            effective_book(venue, &AccountLabel::Default, *mode, &v).as_deref(),
            Some(*expected),
            "{venue}: {key} must name the book"
        );
        // …and the LABELLED account reads its OWN key, never the default account's — the same
        // no-fallback rule every credential loader in this workspace obeys.
        assert_eq!(
            effective_book(venue, &alt(), *mode, &v),
            None,
            "{venue}: a labelled account must not fall back to the default account's {key}"
        );
        let labelled = vars(&[(&format!("{key}__ALT"), value)]);
        assert_eq!(
            effective_book(venue, &alt(), *mode, &labelled).as_deref(),
            Some(*expected),
            "{venue}: {key}__ALT is the labelled account's own key"
        );
    }

    // …and every `Named` row is covered by at least one case above, so a row added with no case
    // fails here rather than being asserted about by nobody.
    for (venue, kind) in BOOK_IDENTITY {
        if matches!(kind, BookIdentity::Named { .. }) {
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

    let derived = vars(&[("HYPERLIQUID_LIVE_PRIVATE_KEY", COW_KEY)]);
    assert_eq!(
        effective_book("hyperliquid", &AccountLabel::Default, VenueMode::Live, &derived).as_deref(),
        Some(COW_ADDR)
    );

    // …and an EXPLICIT address WINS over the derivation, because it is the venue's own statement of
    // which account this is. That is the agent-wallet case: the key is the agent, the address is the
    // master, and the master is the book.
    let mut agent = derived.clone();
    agent.insert("HYPERLIQUID_LIVE_ACCOUNT_ADDRESS".into(), "0xMASTER".into());
    assert_eq!(
        effective_book("hyperliquid", &AccountLabel::Default, VenueMode::Live, &agent).as_deref(),
        Some("0xmaster"),
        "the explicit master address is the book; the agent key's own address is not"
    );

    // A key this cannot parse is not a finding — the mount is about to refuse it anyway, and an
    // identity derived from nothing is worse than no identity.
    let junk = vars(&[("HYPERLIQUID_LIVE_PRIVATE_KEY", "not-a-key")]);
    assert_eq!(effective_book("hyperliquid", &AccountLabel::Default, VenueMode::Live, &junk), None);
}

/// **The undeterminable venues answer `None` with a FULL credential set present** — which is the
/// contract, not a gap: nothing in a key/secret store names the account, so there is no finding to
/// make and none is made.
#[test]
fn an_undeterminable_venue_answers_none_even_fully_credentialled() {
    let v = vars(&[
        ("BINANCE_DEMO_API_KEY", "k"),
        ("BINANCE_DEMO_API_SECRET", "s"),
        ("BYBIT_DEMO_API_KEY", "k"),
        ("BYBIT_DEMO_API_SECRET", "s"),
        ("OKX_DEMO_API_KEY", "k"),
        ("OKX_DEMO_API_SECRET", "s"),
        ("OKX_DEMO_API_PASSPHRASE", "p"),
        ("DERIBIT_DEMO_API_KEY", "k"),
        ("DERIBIT_DEMO_API_SECRET", "s"),
        ("DUKASCOPY_DEMO1_LOGIN", "login"),
        ("DUKASCOPY_DEMO1_PASSWORD", "pw"),
    ]);
    for (venue, kind) in BOOK_IDENTITY {
        if matches!(kind, BookIdentity::Undeterminable { .. }) {
            for mode in [VenueMode::Demo, VenueMode::Live] {
                assert_eq!(
                    effective_book(venue, &AccountLabel::Default, mode, &v),
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
    let v = vars(&[("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS", "0xabc")]);
    assert_eq!(effective_book("hyperliquid", &AccountLabel::Default, VenueMode::Paper, &v), None);
}

/// A venue the roster does not carry has no loader to read and no row to consult.
#[test]
fn a_non_roster_venue_has_no_row_and_no_book() {
    assert!(book_identity_for("sim").is_none());
    assert_eq!(
        effective_book("sim", &AccountLabel::Default, VenueMode::Live, &HashMap::new()),
        None
    );
}

/// Normalization is the PRODUCER's job, and this pins which end owns it —
/// `vike_config::shared_books` compares text and does no folding of its own.
#[test]
fn the_book_is_normalized_here_not_in_the_rule() {
    assert_eq!(normalize_book("  0xAbC \n"), "0xabc");
    assert_eq!(normalize_book("DUQ186573"), "duq186573");
}

/// Only the RESOLVED tier's keys are read. A store holding both a demo and a live key set for one
/// account describes two books at two endpoints, and reading the union would report a pair between
/// one account's live book and another's demo one.
#[test]
fn only_the_resolved_tiers_keys_are_read() {
    let v = vars(&[
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS", "0xdemo"),
        ("HYPERLIQUID_LIVE_ACCOUNT_ADDRESS", "0xlive"),
    ]);
    assert_eq!(
        effective_book("hyperliquid", &AccountLabel::Default, VenueMode::Demo, &v).as_deref(),
        Some("0xdemo")
    );
    assert_eq!(
        effective_book("hyperliquid", &AccountLabel::Default, VenueMode::Live, &v).as_deref(),
        Some("0xlive")
    );
}

/// Polymarket runs NO testnet, so a demo-resolved polymarket account reads no key at all — which
/// matches its arm, whose `LiveOnlyArm` block leaves it on paper below `live`.
#[test]
fn polymarket_has_no_demo_tier_to_read() {
    let v = vars(&[("POLY_LIVE_ADDRESS", "0xfunder")]);
    assert_eq!(effective_book("polymarket", &AccountLabel::Default, VenueMode::Demo, &v), None);
    assert_eq!(
        effective_book("polymarket", &AccountLabel::Default, VenueMode::Live, &v).as_deref(),
        Some("0xfunder")
    );
}
