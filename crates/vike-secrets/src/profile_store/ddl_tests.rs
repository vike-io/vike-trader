use super::ddl::ASSET_CLASS_WORDS_PLACEHOLDER;
use super::*;
use std::assert_matches;

/// The vocabulary a production caller hands over (`vike_model::AssetClass::SQL_WORDS`).
///
/// ⚠ **A FIXTURE, never an authority** — these words are copied, not derived, and what is
/// asserted below is the SCHEMA mechanism rather than the vocabulary. (⚠ The reason this used to
/// give — *"this crate is a zero-`vike-*`-dependency leaf and cannot name that enum"* — is false
/// twice over: tier 15's rule is *nothing above rank 10* and the enum is AT rank 10, and
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
/// 2026-09-20) declared the edge as well. [`profile_ddl`]'s doc carries the ruling that
/// settles what to do about it: the parameter stays, on three reasons none of which is a
/// dependency claim.) The
/// cross-crate pin that these words really are the
/// enum's lives in `crates/vike-tradehub/tests/daemon/profile_rows.rs` — ⚠ not, as this used
/// to say, because it is *the one place that can see both crates* (0072 made this crate one
/// too), but because it is the one place that can see the enum and the production SUPPLIER
/// together, which is the drift that pin actually catches.
const WORDS: &[&str] = &[
    "Equity",
    "Etf",
    "CryptoSpot",
    "CryptoPerp",
    "CryptoFuture",
    "Option",
    "Fx",
    "Future",
    "Index",
    "PredictionMarket",
    "Cfd",
];

fn schema() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory store");
    conn.execute_batch(&profile_ddl(WORDS).expect("renders")).expect("schema applies");
    conn.execute("INSERT INTO profile (name, kind, active) VALUES ('p', 'daemon', 0)", [])
        .expect("parent row");
    conn
}

/// **`mount.asset_class` is NOT NULL** — a mount that does not say which product it trades is
/// refused by the schema. `MountRow` cannot express the absence at all (its constructor takes
/// the class), so the database is the only place this refusal is observable.
#[test]
fn a_mount_row_with_no_asset_class_is_refused() {
    let conn = schema();
    let err = conn
        .execute(
            "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol) \
                 VALUES ('p', 0, 0, 'bybit', NULL, 'BTCUSDT')",
            [],
        )
        .expect_err("a NULL asset_class must be refused");
    assert!(
        err.to_string().to_uppercase().contains("NOT NULL"),
        "expected a NOT NULL violation, got: {err}"
    );
}

/// **The `CHECK` refuses a word outside the vocabulary**, however plausible it looks — which is
/// what stops a hand-edited store or a future writer inventing a twelfth product.
#[test]
fn a_word_outside_the_vocabulary_is_refused_by_the_check() {
    let conn = schema();
    for (i, bad) in
        ["cryptospot", "Perp", "Spot", "CRYPTOSPOT", "", "Crypto Spot"].iter().enumerate()
    {
        let err = conn
            .execute(
                "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol) \
                     VALUES ('p', ?1, 0, 'bybit', ?2, 'BTCUSDT')",
                rusqlite::params![i as i64, bad],
            )
            .expect_err("a word outside the vocabulary must be refused");
        assert!(
            err.to_string().to_uppercase().contains("CHECK"),
            "expected a CHECK violation for {bad:?}, got: {err}"
        );
    }
}

/// ...and every word the vocabulary DOES carry is accepted. Without this half the test above
/// would pass over a `CHECK` that refuses everything.
#[test]
fn every_word_in_the_vocabulary_is_accepted() {
    let conn = schema();
    for (i, word) in WORDS.iter().enumerate() {
        conn.execute(
            "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol) \
                 VALUES ('p', ?1, 0, 'bybit', ?2, 'BTCUSDT')",
            rusqlite::params![i as i64, word],
        )
        .unwrap_or_else(|e| panic!("`{word}` is in the vocabulary and must be accepted: {e}"));
    }
}

/// **The template spells no asset-class word of its own.** Rendered with a foreign vocabulary,
/// the `CHECK` carries exactly that vocabulary and none of the real one — which is the property
/// that makes the Rust enum the single declaration rather than one of two lists.
#[test]
fn the_schema_carries_only_the_vocabulary_it_was_given() {
    let ddl = profile_ddl(&["Alpha", "Beta"]).expect("renders");
    assert!(ddl.contains("CHECK (asset_class IN ('Alpha', 'Beta'))"), "{ddl}");
    for word in WORDS {
        assert!(
            !ddl.contains(&format!("'{word}'")),
            "the template carries a hardcoded asset-class word ({word}) — the vocabulary must \
                 arrive from the caller, or there are two lists"
        );
    }
    let real = profile_ddl(WORDS).expect("renders");
    for word in WORDS {
        assert!(real.contains(&format!("'{word}'")), "{word} missing from the rendered CHECK");
    }
    assert!(
        !real.contains(ASSET_CLASS_WORDS_PLACEHOLDER),
        "the placeholder must be fully substituted:\n{real}"
    );
}

/// A vocabulary this module will not render is REFUSED rather than interpolated. The only caller
/// hands over `&'static str`s off a closed enum, so reaching this is a bug in the caller — but
/// the thing standing between caller data and a SQL clause must be a refusal, not an assumption.
#[test]
fn an_unrenderable_vocabulary_is_refused() {
    for bad in [&["Crypto Spot"][..], &["it's"][..], &["a'); DROP TABLE mount;--"][..], &[""][..]] {
        let e =
            profile_ddl(bad).expect_err("a non-identifier word must never be rendered into SQL");
        assert_matches!(e, ProfileError::UnrenderableVocabulary { .. }, "{e}");
    }
    let e =
        profile_ddl(&[]).expect_err("an empty vocabulary would refuse every mount row there is");
    assert!(e.to_string().contains("EMPTY"), "{e}");
}

/// Every table the presence probe knows about is still created by the rendered DDL — the
/// parameterisation must not have dropped one.
#[test]
fn the_rendered_schema_still_creates_every_declared_table() {
    let conn = schema();
    for table in PROFILE_TABLES {
        let found: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |r| r.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(found, 1, "`{table}` is not created by the rendered schema");
    }
}
