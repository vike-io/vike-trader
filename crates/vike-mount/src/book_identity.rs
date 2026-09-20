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
//!
//! ## ⚠ …but an `Undeterminable` ROW is NOT the last word on that venue any more
//!
//! This table answers OFFLINE, out of the credential store, and that is the whole of its scope. It
//! stopped being the whole of the ANSWER on 2026-09-19: [`book_of_account`] reads
//! [`recorded_book`] — `vike_secrets::Account::venue_account_id` — FIRST, and only falls through to
//! [`effective_book`]. So a venue whose row here says *nothing in the store names the book* can
//! still contribute a `vike_config::ArmedBook` and still be reported in a shared-book finding, as
//! soon as something has told the store what the venue answered.
//!
//! Two things do that telling: `vike-cli secrets set-book`, and a mount whose own handshake named
//! the account ([`confirmation_for_account`] → `vike_model::account_confirmation::park` →
//! `vike-cli secrets confirm`). Neither consults this table at all.
//!
//! **So do not read an `Undeterminable` row as "this venue can never report a shared book."** It
//! means "the store's KEYS do not name it", which is a claim about the credential file and not
//! about the venue. `vike_mount::shared_books_for`'s own doc states the widened rule; this
//! paragraph exists because a reader who got here first would otherwise conclude the opposite.

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
    // ⚠ **CORRECTED. This row was `Named { name_suffixes: &["USER"] }` and its comment said "`USER`
    // is the login, which on FXCM is the account". The second half is false, and MEASURED false in
    // this tree's own shim.**
    //
    // `crates/bridges/fxcm/src/shim/fcshim.cpp`'s `firstAccount` reads an ACCOUNTS TABLE off the
    // login rules and returns the first row that is kind 32/36 and whose margin-call flag is `N`.
    // A table, not a field — so one login carries several accounts, and `USER` names the login that
    // reaches them rather than the book any order lands on.
    //
    // ⚠ **And the account is not fixed even for the session.** `fc_place`, `fc_account` and
    // `fc_base_unit_size` each call `firstAccount` afresh, so the selection is re-made per FFI round
    // trip, against a predicate that includes *not in margin call*. A login with two eligible
    // accounts therefore moves its order flow to the second one the moment the first enters a margin
    // call — silently, and at the worst possible moment, since a margin call on the first account is
    // exactly when its flow should not be quietly continue elsewhere. That is an ORDER-ROUTING defect
    // rather than a naming one, it is recorded at `crates/bridges/fxcm/CLAUDE.md`, and it is not
    // this table's to fix; what this table must stop doing is asserting a book it cannot know.
    //
    // So: `Undeterminable`. The safe direction for a COLLAPSE error — two accounts of one login
    // would have compared equal here and been reported as a shared book they may not share.
    (
        "fxcm",
        BookIdentity::Undeterminable {
            why: "the store holds a ForexConnect LOGIN (`FXCM_{TIER}_USER`), and one login reaches \
                  an ACCOUNTS TABLE rather than one account — the shim picks the first row that is \
                  kind 32/36 and not in margin call, afresh on every FFI call. So nothing in the \
                  store names the book, and the book is not fixed for the session either",
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
            why: "the store holds a client id/secret pair; deribit's subaccount is selected by the \
                  credential and named nowhere in the store",
        },
    ),
    // ⚠ dukascopy: the row STAYS `Undeterminable` and its REASON is rewritten — the correction
    // `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §9 records as owed.
    //
    // What the old reason said, and why both halves were wrong: *"no labelled dukascopy account can
    // arm at all (`arm_addresses_accounts`), so two of them can never both be active"* rested on an
    // arming REFUSAL rather than on determinability, and that refusal is gone — the venue joined
    // `arm_addresses_accounts` on 2026-09-15 and `crate::dukascopy` addresses its accounts through
    // the settings database's `account` rows. And *"unknowable"* was never true of the venue: the
    // JForex sidecar's ready handshake carries an account outright
    // (`crates/bridges/dukascopy/src/proto.rs`'s `Envelope::Ready`), and on 2026-09-14 two live
    // logins answered with two different accounts and two separate balances.
    //
    // What IS true, and is the whole of the reason now: `effective_book` reads the credential STORE
    // and nothing else, and dukascopy's store holds a LOGIN rather than an account number. So this
    // classification means "cannot be determined OFFLINE", which is the enum's own scope — and this
    // venue is the cleanest example of the third state `BookIdentity` has no variant for: the venue
    // tells us at connect. `vike_secrets::Account::venue_account_id` is where that answer lands, and
    // `vike-cli secrets set-book` is the only writer of it today.
    (
        "dukascopy",
        BookIdentity::Undeterminable {
            why: "the store holds a JForex LOGIN, not an account number, so nothing in it names \
                  the book; the venue answers at the sidecar's ready handshake instead, and \
                  `account.venue_account_id` is where that answer is kept",
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

/// **The book the STORE has RECORDED for one account** — `vike_secrets::Account::venue_account_id`,
/// read out of the snapshot the composition root already took
/// ([`crate::MountPolicy::accounts`]).
///
/// This is the half of the question [`effective_book`] structurally cannot answer. That function
/// reads the CREDENTIAL store and derives, and for the five venues whose row here is
/// [`BookIdentity::Undeterminable`] there is nothing in it to derive FROM — an HMAC key names no
/// account and a login names no account — so it answers `None` and no shared book of theirs can
/// ever be reported. The fact may nonetheless be sitting in the `account` table, put there by
/// `vike-cli secrets set-book` or, later, by a mount's own handshake. **Reading it here is what
/// makes a recorded book DO something**: until this existed, `set-book` wrote a column that changed
/// no decision anywhere in the mount.
///
/// # The join is `(venue, tier, label)`, and every term is load-bearing
///
/// * **`venue`** — obvious, and the only term the row states in the shape the caller holds it.
/// * **`tier`** — the same discipline [`effective_book`] states for its own key reads: a store
///   holding both a demo and a live account of one venue holds TWO books at two different
///   endpoints, and a report pairing one account's live book with another's demo book would be a
///   finding about two ledgers that cannot possibly share anything. `mode` is the account's
///   RESOLVED tier, and [`VenueMode::as_str`] is spelling-identical to
///   `vike_secrets::schema::ACCOUNT_TIERS` — `"demo"` and `"live"` — so this is a comparison rather
///   than a translation.
/// * **`label`** — `vike_model::account_keys::AccountLabel::text` is `None` for the default account
///   and `account.label` is `NULL` for it too, so the two spellings compare equal with no special
///   case. ⚠ That is **not** the same as saying a `NULL` label matches anything: it matches the
///   DEFAULT account and nothing else, which is why a labelled account can never be handed the
///   default account's book.
///
/// # ⚠ More than one candidate answers `None`, and that is the dukascopy case
///
/// Two rows of one `(venue, tier, label)` is a real shape — dukascopy's `(dukascopy, demo, NULL)`
/// pair, which is the whole reason `vike_secrets::Account::id` is the identity and the label is not.
/// Under the default label BOTH rows match, and there is no honest way to pick one here: the fact
/// that separates them is the credential-key OWNER PREFIX, which [`crate::dukascopy::resolve_account`]
/// reads and this general resolver deliberately does not restate. So an ambiguous join answers "not
/// determinable", exactly as an absent column does, and the caller falls through to the offline
/// derivation. Returning the first row's book would file one broker's account number against the
/// other broker's account.
///
/// Likewise a row whose column is still `None` contributes nothing — *not yet known* is not *this
/// account has no book* ([`vike_secrets::Account::venue_account_id`] states that rule at the column
/// itself).
#[must_use]
pub fn recorded_book(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    directory: &crate::AccountDirectory,
) -> Option<String> {
    let row = recorded_row(venue, label, mode, directory)?;
    // ⚠ NORMALIZED, and it is not cosmetic. `vike_secrets::normalized_venue_account_id` preserves
    // CASE (it refuses invisibles and bounds the length, nothing more), so a store holding `0xAbC`
    // and a derivation answering `0xabc` are the same account spelled twice — and
    // `vike_config::shared_books` is a plain string comparison, by design, because this function and
    // [`effective_book`] both normalize at the producer.
    row.venue_account_id.as_deref().map(normalize_book)
}

/// **THE book resolution the mount reports on: what was RECORDED, else what can be DERIVED.**
///
/// [`recorded_book`] first, [`effective_book`] second. The order is the point and it is not a
/// preference between two guesses:
///
/// * a recorded id is what the VENUE said (a handshake) or what an OPERATOR read off the venue's own
///   page (`vike-cli secrets set-book`) — a statement about the account;
/// * a derivation is an INFERENCE from the signing material, and on the one venue where the two can
///   disagree it is the inference that is wrong. A hyperliquid agent key with no
///   `_ACCOUNT_ADDRESS` beside it derives the AGENT's address, which is not the book it trades —
///   the pitfall `vike_hyperliquid::user_role`'s module doc carries in the venue's own words.
///
/// They do not disagree silently for long: `vike_secrets::set_venue_account_id` REFUSES to write a
/// row that already names a different book (`DbErrorKind::BookAlreadyKnown`), so the case where a
/// recorded value and a derivation differ is a finding the writer raises, not a value this reader is
/// quietly picking between.
#[must_use]
pub fn book_of_account(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    vars: &HashMap<String, String>,
    directory: &crate::AccountDirectory,
) -> Option<String> {
    recorded_book(venue, label, mode, directory)
        .or_else(|| effective_book(venue, label, mode, vars))
}

/// **THE JOIN** — the one `account` row that IS account `label` of `venue` at the tier it resolved
/// to, or `None` when no single row can be said to be it.
///
/// Factored out of [`recorded_book`] because two callers need different halves of the same answer:
/// that function wants the BOOK the row records, while [`classify_observed_book`] wants the row's
/// `id` so an operator can be handed a `vike-cli secrets set-book --id N` line rather than a
/// description of one. One join, so the line an operator is told to run cannot address a different
/// row from the one the mount read.
///
/// The three terms, and why each is load-bearing, are argued at [`recorded_book`]. The short form:
/// the tier keeps a live book from pairing with a demo one, `account.label IS NULL` means the
/// DEFAULT account and not *any* account, and **more than one candidate answers `None` rather than
/// picking one** — dukascopy's `(dukascopy, demo, NULL)` pair, where the discriminating fact is the
/// credential-key owner prefix this general resolver deliberately does not restate.
fn recorded_row<'a>(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    directory: &'a crate::AccountDirectory,
) -> Option<&'a vike_bridge_core::credentials::Account> {
    // A paper account touches no venue book — the same first line [`effective_book`] has, and for
    // the same reason. A recorded column is a fact about the account, not about this mount, so it
    // outlives a demotion to paper and must not be reported as a book being traded.
    if mode == VenueMode::Paper {
        return None;
    }
    let rows = directory.rows()?.ok()?.known()?;
    let tier = mode.as_str();
    let mut matched = rows.iter().filter(|r| {
        r.active && r.venue == venue && r.tier == tier && r.label.as_deref() == label.text()
    });
    let row = matched.next()?;
    if matched.next().is_some() {
        return None;
    }
    Some(row)
}

/// **The record a mount PARKS when a venue's own handshake named the book it trades** — the
/// generic twin of what `crate::dukascopy`'s `confirmation_for` builds for the one venue that had
/// this wired first.
///
/// `None` means *this account cannot be addressed by a parked record*, which is not the same as
/// *nothing was learned*; the caller still reports what the venue said. There are two such cases
/// and they are different problems:
///
/// * **no single `account` row** — [`recorded_row`]'s answer, argued there. Nothing to confirm.
/// * **the row owns no credential-key OWNER PREFIX**, which is the address
///   `vike_model::account_confirmation::ConfirmationRecord` carries. See the ⚠ below.
///
/// # ⚠ TWO address shapes, and the ROW decides which
///
/// `vike_model::account_confirmation::ConfirmationRecord` can be addressed either way and this
/// function picks by `account.label`:
///
/// * **unlabelled** — the credential-key OWNER PREFIX (`HYPERLIQUID_LIVE_`), which is the only thing
///   separating two rows that share `(venue, tier, label)`. That is dukascopy's pair, and it is the
///   case prefix addressing exists for. A row with NO prefix has no address — an account row can
///   outlive the last credential that created it — so nothing is parked;
/// * **labelled** — `(tier, label)`, the store's own `UNIQUE (venue, tier, label)` index.
///
/// The second shape exists because a labelled account has no prefix AT ALL, which was a declared
/// dead end until 2026-09-19: `vike_secrets::read_account_keys` derives a prefix by stripping a
/// credential's `field` from the END of its NAME, and for a labelled key the `field` is not a suffix
/// because the `__LABEL` sits after it — so the row's prefix list is EMPTY and a prefix-addressed
/// record could match no row. `Classification::owner_prefix` states that `None` and calls it
/// survivable, which it is for the MIGRATION (the resolver's `(venue, tier, label)` lookup answers)
/// and was NOT for a parked record until the record learned to carry that same tuple.
///
/// # What the record says, and what it does NOT decide
///
/// It carries the venue's answer and what the store held AT THE HANDSHAKE (`observed_book`), plus
/// the row id as EVIDENCE. Whether that combination folds, confirms or alarms is
/// `vike_model::account_confirmation::verdict`'s to say — computed here for the LOG and again by
/// the fold over the store as it stands then, because the two moments are genuinely different and a
/// record that baked in a verdict would be asserting one about a store it can no longer see.
/// # ⚠ `bound_tier` IS THE TIER THE MOUNT BOUND, NEVER THE CEILING — and the wrong one is in scope
///
/// The parameter is named for the trap. `crate::make_engine_for_account` binds BOTH of these near
/// the top, they are both `VenueMode`-shaped, and the wrong one is the one whose name reads like an
/// answer:
///
/// ```text
/// let mode = account_ceiling(policy, venue, account);              // the CEILING — NOT this
/// let live_permitted = ceiling_permits_live(mode);
/// let mainnet = cex_mainnet_enabled(venue, vars) && live_permitted;
/// let tier = if mainnet { Environment::Live } else { Environment::Demo };   // the BOUND tier
/// ```
///
/// They differ on the ordinary default box. With `venues.bybit = "live"` and `BYBIT_MAINNET` unset,
/// `mode` is `Live` while the mount loads the **DEMO** key set — so a handshake performed with demo
/// credentials, passed the ceiling, would resolve [`recorded_row`]'s `r.tier == …` filter onto the
/// **LIVE** `account` row and park a demo account's identity against it. `secrets confirm` would
/// then fold it, and the row an order routes on would name the wrong account with nothing anywhere
/// reporting a problem.
///
/// The worked example to copy is `crate::hyperliquid`'s `resolve_hyperliquid_master` call site,
/// which constructs the value inline from the resolution it just performed
/// (`if mainnet { VenueMode::Live } else { VenueMode::Demo }`) rather than reaching for the `mode`
/// sitting in scope.
///
/// ⚠ [`recorded_book`]'s own `mode` has the same requirement and is NOT renamed, because its one
/// caller — [`crate::shared_books_for`] — passes `vike_config::VenueArming::effective`, which
/// `crate::arming`'s `account_arming_raw` derives with `cex_mainnet_enabled(venue, vars) && live`,
/// *the top-of-`make_engine` resolution verbatim*. That path cannot reach for the ceiling by
/// accident; a venue arm calling THIS function can.
#[must_use]
pub fn confirmation_for_account(
    venue: &str,
    label: &AccountLabel,
    bound_tier: VenueMode,
    handshake_account_id: &str,
    directory: &crate::AccountDirectory,
    at_ms: i64,
) -> Option<vike_model::account_confirmation::ConfirmationRecord> {
    let row = recorded_row(venue, label, bound_tier, directory)?;
    // ⚠ **BOTH halves are read from the ROW the join already resolved, never composed here.**
    // `vike-cli secrets confirm` matches an unlabelled record by
    // `prefixes.contains(&rec.key_prefix)` and a labelled one by `(a.tier, a.label)`, over exactly
    // these values — so taking them FROM the row is what makes the mount and the fold incapable of
    // addressing different rows. A composed `HYPERLIQUID_LIVE_`, or a tier taken from `mode` rather
    // than from `row.tier`, would be a second derivation free to disagree with the store's own.
    let (key_prefix, label, tier) = match row.label.as_deref() {
        // A LABELLED account: the row carries no credential-key prefix at all, and `(tier, label)`
        // is the store's own `UNIQUE (venue, tier, label)` address. The empty prefix is the shape,
        // not a missing value — `ConfirmationRecord::key_prefix` says so at the field.
        Some(l) => (String::new(), Some(l.to_string()), Some(row.tier.clone())),
        // UNLABELLED: the prefix is the ONLY thing separating two rows that share
        // `(venue, tier, label)`, which is dukascopy's pair. No prefix means no address — a row can
        // outlive the last credential that created it — so nothing is parked.
        None => (directory.keys()?.ok()??.get(&row.id)?.prefixes.first()?.clone(), None, None),
    };
    Some(vike_model::account_confirmation::ConfirmationRecord {
        venue: venue.to_string(),
        key_prefix,
        label,
        tier,
        handshake_account_id: normalize_book(handshake_account_id),
        observed_row: Some(row.id),
        observed_book: row.venue_account_id.as_deref().map(normalize_book),
        at_ms,
    })
}
