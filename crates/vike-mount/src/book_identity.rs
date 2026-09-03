//! **Which venue BOOK does one account actually trade?** — the per-venue capability table behind
//! the shared-book warning `vike_config::shared_books` states the rule for.
//!
//! # Why a table and not a match arm
//!
//! The answer is genuinely different per venue and there is no defensible default. An EIP-712 venue
//! derives a wallet address from the private key in the store, so the answer is computable offline
//! and exact. A key/secret venue's store holds an HMAC key and nothing that NAMES an account — the
//! account is discoverable only by asking the venue. A match arm with a `_ => None` would answer
//! "cannot tell" for a venue somebody forgot, which is indistinguishable from the venues where
//! "cannot tell" is the truth. So this is the repo's standard shape for a per-venue fact
//! (`vike_model::venue_caps`, `vike_model::fees`, `vike_bridge_core::tif`): one row per
//! `vike_model::VENUES` id, each citing the loader it was read from, with a completeness test
//! (`crates/vike-mount/tests/book_identity_table.rs`) that fails the moment the roster grows.
//!
//! # ⚠ What a row may name, and the family this table deliberately declines
//!
//! A [`BookIdentity::Named`] row may only ever name credential keys whose VALUES are safe to put in
//! a log line: an address, a login, a numeric account id. The warning prints the shared book, and a
//! table that fingerprinted the API SECRET to catch "the same credential pasted under two labels"
//! would be a secret-shaped value one careless `{:?}` away from a log.
//!
//! **That is a declared residual, not an oversight**: on binance/bybit/okx/deribit, two labels
//! holding the SAME api key/secret pair are the same book and this table says nothing. It is a
//! narrower error than the one the warning exists for (an agent key signing for a master whose own
//! key is also configured), it is a duplicate CREDENTIAL rather than two accounts, and the
//! credential editor is the place that can see it without a comparison over secret material.
//!
//! # ⚠ Where the book cannot be determined, NOTHING is said
//!
//! [`effective_book`] answers `None` and `vike_mount::make_engine_accounts` builds no
//! `vike_config::ArmedBook` for that account, so no pair can form and no warning is emitted. An
//! unprovable suspicion is not a finding; the alternative — a placeholder value — would compare
//! equal to every other undeterminable account of the venue and manufacture a pair out of two
//! unknowns.

use std::collections::HashMap;

use vike_bridge_core::credentials::account_var;
use vike_config::VenueMode;
use vike_model::account_keys::AccountLabel;

/// **How one venue's effective trading book is identified from the credential store, offline.**
///
/// The two variants are the two honest answers, and there is no third: either the store NAMES the
/// account (directly, or through a key from which the name is derivable), or it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookIdentity {
    /// **The store names the book.** [`Self::Named::name_suffixes`] are the key suffixes tried in
    /// order; the first non-blank value IS the book. [`Self::Named::evm_key_suffixes`] is the
    /// FALLBACK for a venue whose identifier may be left implicit in an EVM private key — the
    /// address is then derived locally (`vike_bridge_core::eth_address_from_private_key`), which is
    /// the same derivation that venue's own signer performs.
    Named {
        /// The `{PREFIX}` of every key this row composes — `"OANDA"`, `"POLY"`, `"HYPERLIQUID"`.
        prefix: &'static str,
        /// The tier tokens tried, IN ORDER, for an account that resolved to [`VenueMode::Demo`].
        /// Empty means the venue has no demo tier at all (polymarket runs no testnet), in which
        /// case a demo-resolved account of it has no book — which is correct, since its arm cannot
        /// mount one either.
        demo_tiers: &'static [&'static str],
        /// The tier tokens tried, IN ORDER, for an account that resolved to [`VenueMode::Live`].
        /// More than one entry is a LEGACY spelling the venue's own loader still accepts
        /// (`vike_bridge_core::credentials::Environment::legacy_str`'s `MAINNET`).
        live_tiers: &'static [&'static str],
        /// Key suffixes whose VALUE is the book, tried in order. Must be safe to log — see the
        /// module doc.
        name_suffixes: &'static [&'static str],
        /// Key suffixes holding an EVM private key the address is derived from when no
        /// [`Self::Named::name_suffixes`] key is present. Empty for every non-EVM venue.
        evm_key_suffixes: &'static [&'static str],
    },
    /// **Nothing in the store names the book**, so only a live call could answer. No warning is
    /// possible and none is emitted; the reason is carried so the row is a CLASSIFICATION rather
    /// than a gap.
    Undeterminable {
        /// Why — cited to the credential shape it was read from.
        why: &'static str,
    },
}

/// **One row per `vike_model::VENUES` id.** A venue missing from here fails
/// `crates/vike-mount/tests/book_identity_table.rs`'s completeness test, so a new bridge cannot
/// ship without an answer.
///
/// ⚠ Every row's key names were read from that venue's OWN loader, and each is cited beside it. The
/// tier tokens are the loader's, not `Environment::as_str()`'s guess: alpaca spells its demo tier
/// `SANDBOX` and aster spells its non-live tier `TESTNET`, and a row that assumed `DEMO` would read
/// no key, answer `None`, and silently downgrade a determinable venue to "cannot tell".
pub const BOOK_IDENTITY: &[(&str, BookIdentity)] = &[
    // ── EIP-712 / EVM venues: the address is derivable locally, which is the whole reason this
    //    table can catch the agent-key-for-a-master shape at all. ─────────────────────────────────
    //
    // `crates/bridges/hyperliquid/src/config.rs`'s `load_for_account`: `_ACCOUNT_ADDRESS` is the
    // MASTER address when the key is an agent wallet, and absent means "the key IS the account".
    // That is exactly this variant's two-step, and hyperliquid is the venue whose SUB-ACCOUNTS made
    // the corrected model necessary — they have their own addresses, so two of them are two books
    // and this row reports nothing about them.
    (
        "hyperliquid",
        BookIdentity::Named {
            prefix: "HYPERLIQUID",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT_ADDRESS"],
            evm_key_suffixes: &["PRIVATE_KEY"],
        },
    ),
    // `crates/bridges/aster/src/signing.rs`'s `load_aster_credentials_for_account`: `USER` is the
    // master address (it goes on the wire as `user=`), `PRIVATE_KEY` is the agent signer and
    // `SIGNER` its optional explicit address. So the BOOK is `USER`, always present (the loader
    // gates on it), and no EVM fallback is needed or wanted — deriving from the agent key would
    // answer with the SIGNER's address, which is not the book.
    (
        "aster",
        BookIdentity::Named {
            prefix: "ASTER",
            demo_tiers: &["TESTNET"],
            live_tiers: &["LIVE"],
            name_suffixes: &["USER"],
            evm_key_suffixes: &[],
        },
    ),
    // `crates/bridges/polymarket/src/config.rs`'s `load_polymarket_creds_for_account`: `ADDRESS` is
    // the funder/maker wallet — the thing that HOLDS the outcome tokens — and `PRIVATE_KEY` is the
    // L1 root that derives the L2 trio. No testnet exists, hence the empty demo tier list; the
    // `MAINNET` live tier is the legacy spelling that loader still accepts.
    (
        "polymarket",
        BookIdentity::Named {
            prefix: "POLY",
            demo_tiers: &[],
            live_tiers: &["LIVE", "MAINNET"],
            name_suffixes: &["ADDRESS"],
            evm_key_suffixes: &["PRIVATE_KEY"],
        },
    ),
    // ── Venues whose store carries a literal account identifier. ─────────────────────────────────
    //
    // `crates/bridges/oanda/src/config.rs`'s `tier_var_names`: the `(api_key, account_id)` pair, and
    // the account id IS the fxTrade account this mount trades.
    (
        "oanda",
        BookIdentity::Named {
            prefix: "OANDA",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE", "MAINNET"],
            name_suffixes: &["ACCOUNT_ID"],
            evm_key_suffixes: &[],
        },
    ),
    // `crates/bridges/alpaca/src/config.rs`'s `load_alpaca_config_for_account`, whose demo tier is
    // spelled `SANDBOX` (`alpaca_tier`) rather than `DEMO`.
    (
        "alpaca",
        BookIdentity::Named {
            prefix: "ALPACA",
            demo_tiers: &["SANDBOX"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT_ID"],
            evm_key_suffixes: &[],
        },
    ),
    // `crates/bridges/vike-ibkr/src/config.rs`'s `load_ibkr_config_for_account`: `_ACCOUNT` selects
    // the `DU…`/`U…` account every order is placed in, which that file's own doc calls out.
    (
        "ibkr",
        BookIdentity::Named {
            prefix: "IBKR",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT"],
            evm_key_suffixes: &[],
        },
    ),
    // `crates/bridges/ctrader/src/config.rs`: `ACCOUNT_ID` is OPTIONAL — when it is absent the
    // account is DISCOVERED at connect. So this row is determinable exactly when the operator wrote
    // it, and `effective_book`'s `None` covers the rest. That is the right shape: a row cannot be
    // "sometimes undeterminable" as a CLASSIFICATION, but a value can be absent.
    (
        "ctrader",
        BookIdentity::Named {
            prefix: "CTRADER",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT_ID"],
            evm_key_suffixes: &[],
        },
    ),
    // `crates/bridges/ig/src/config.rs`'s `ig_env_var_names`: `IDENTIFIER` is the LOGIN. ⚠ It names
    // the login rather than the dealing account, and IG can hold several accounts behind one login
    // — so this row is sound in the direction that matters (two labels with one identifier ARE one
    // login, and this daemon selects that login's default account both times) and silent in the
    // other (two different logins on one underlying account cannot be seen from here). A warning
    // that fires only on a certainty is the contract; the missed case is a miss, not a false
    // report.
    (
        "ig",
        BookIdentity::Named {
            prefix: "IG",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE", "MAINNET"],
            name_suffixes: &["IDENTIFIER"],
            evm_key_suffixes: &[],
        },
    ),
    // `crates/bridges/fxcm/src/config.rs`'s `load_fxcm_tier`: `USER` is the login, which on FXCM is
    // the account. Reached only by an SDK-linked binary, but the table answers the same on every
    // box — a `policy.toml` must be portable between the boxes of one deployment, and so must the
    // report over it.
    (
        "fxcm",
        BookIdentity::Named {
            prefix: "FXCM",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE", "MAINNET"],
            name_suffixes: &["USER"],
            evm_key_suffixes: &[],
        },
    ),
    // ── Key/secret venues: nothing in the store NAMES the account. ───────────────────────────────
    (
        "binance",
        BookIdentity::Undeterminable {
            why: "the store holds an HMAC api key/secret pair and no account identifier; which \
                  account a key belongs to is answerable only by an authenticated call",
        },
    ),
    (
        "bybit",
        BookIdentity::Undeterminable {
            why: "the store holds an HMAC api key/secret pair and no account identifier; a bybit \
                  sub-account is selected BY THE KEY, which names it nowhere offline",
        },
    ),
    (
        "okx",
        BookIdentity::Undeterminable {
            why: "the store holds an api key/secret/passphrase trio and no account identifier; \
                  which sub-account the key belongs to is answerable only by an authenticated call",
        },
    ),
    (
        "deribit",
        BookIdentity::Undeterminable {
            why:
                "the store holds a client id/secret pair; deribit's subaccount is selected by the \
                  credential and named nowhere in the store",
        },
    ),
    // ⚠ dukascopy: `DUKASCOPY_DEMO1_LOGIN` genuinely names the account, and this row is STILL
    // undeterminable — deliberately, because it would be a rule with no reachable input.
    // `vike_mount::arm_addresses_accounts` refuses a labelled dukascopy account outright (its
    // `DEMO1`/`DEMO2` shape bakes an account INDEX into the tier token, a second multi-account
    // spelling `vike_model::account_keys` pins as non-conforming on purpose), so this venue can
    // never have two active accounts and no pair can form whatever this row said. Classifying it
    // `Named` would state a tier grammar that does not exist and invite a reader to trust it.
    (
        "dukascopy",
        BookIdentity::Undeterminable {
            why: "no labelled dukascopy account can arm at all (`arm_addresses_accounts`), so two \
                  of them can never both be active; its DEMO1/DEMO2 keys bake the account index \
                  into the tier token rather than naming an account beside one",
        },
    ),
    // The conservative placeholder a scaffold writes: "cannot tell offline" emits no warning, which
    // is the safe direction. Replace it by READING that venue's own credential loader and saying
    // whether anything in the store NAMES the account — see the rows above, each cited to its loader.
    // vike:new-venue:row ("{venue}", BookIdentity::Undeterminable { why: "TODO(new-venue: {venue})" }),
];

/// The [`BookIdentity`] declared for `venue`, or `None` for a string
/// [`vike_model::VENUES`] does not carry (a test's `"sim"`, a paper id).
///
/// A non-roster venue has no credential loader to read, so there is nothing to classify — and it
/// can only ever be a single default account anyway.
#[must_use]
pub fn book_identity_for(venue: &str) -> Option<BookIdentity> {
    BOOK_IDENTITY.iter().find(|(v, _)| *v == venue).map(|(_, k)| *k)
}

/// **The canonical form every book identity is compared in**: trimmed, lowercased ASCII.
///
/// Both shapes this table produces are case-insensitive identifiers — an EVM address is the same
/// account in any case (hyperliquid's own loader lowercases it for signing) and an IBKR `DUQ186573`
/// is not a different account from `duq186573`. Normalizing HERE, at the producer, is what lets
/// `vike_config::shared_books` be a plain string comparison: a second copy of this knowledge inside
/// the rule would be free to disagree with this one.
#[must_use]
pub fn normalize_book(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// **THE resolution: which book does account `label` of `venue` trade, at the tier it resolved
/// to?** `None` means "not determinable from this store", which is the answer that emits no
/// warning.
///
/// `mode` is the account's RESOLVED tier (`account_arming_under`'s answer), not its stated ceiling,
/// so the keys read are the keys the mount will actually sign with:
///
/// * [`VenueMode::Paper`] answers `None` outright — a paper account touches no venue book, so there
///   is nothing to share and nothing to compare;
/// * [`VenueMode::Demo`] / [`VenueMode::Live`] select that row's own tier tokens, in order.
///
/// ⚠ **Only the resolved tier's keys are read.** A store that holds BOTH a demo and a live key set
/// for one account has two books; comparing the union would report a pair between account A's live
/// book and account B's demo book, which are two different ledgers at two different endpoints.
#[must_use]
pub fn effective_book(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    vars: &HashMap<String, String>,
) -> Option<String> {
    if mode == VenueMode::Paper {
        return None;
    }
    let BookIdentity::Named { prefix, demo_tiers, live_tiers, name_suffixes, evm_key_suffixes } =
        book_identity_for(venue)?
    else {
        return None;
    };
    let tiers = if mode == VenueMode::Live { live_tiers } else { demo_tiers };
    // The NAMED key first, across every tier spelling, because it is the venue's own statement of
    // which account this is. Only then the derivation, which is an inference from a signing key.
    for tier in tiers {
        for suffix in name_suffixes {
            if let Some(v) = account_var(vars, &format!("{prefix}_{tier}_{suffix}"), label) {
                return Some(normalize_book(v));
            }
        }
    }
    for tier in tiers {
        for suffix in evm_key_suffixes {
            let Some(key) = account_var(vars, &format!("{prefix}_{tier}_{suffix}"), label) else {
                continue;
            };
            // A key this cannot parse is not a finding: the mount is about to refuse it anyway (a
            // malformed private key fails at signer construction), and guessing here would be an
            // identity derived from nothing.
            if let Ok(addr) = vike_bridge_core::eth_address_from_private_key(key) {
                return Some(normalize_book(&addr));
            }
        }
    }
    None
}
