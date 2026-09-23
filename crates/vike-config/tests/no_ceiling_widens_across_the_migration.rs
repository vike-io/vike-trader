//! **§5.3's gate — no account's effective ceiling comes out of the schema 2→3 migration HIGHER
//! than it went in.**
//!
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` calls this *"the one gate
//! without which this must not ship"*. Stage 3 folds the `venue_arming` table into a new
//! `account.armed` column, and the fold is the one place in the whole plane where a mistake ARMS
//! SOMETHING NOBODY ARMED — on boxes that hold real venue credentials.
//!
//! # Why the gate lives HERE, in `vike-config`'s tests — the layer ruling
//!
//! The reshape lives in `vike-secrets` (`[package.metadata.vike] layer = 15`). The two things this
//! gate must compare against live in `vike-config` (layer 20): the ORDERING (`VenueMode`'s `Ord`,
//! and `VenueMode::cap`, the named `min` this tree insists on over a bare `.min()`) and the
//! INHERITANCE RULE (`crates/vike-config/src/venue_mode.rs`'s `VenuePolicy::account`). A gate
//! inside `vike-secrets` could not name either, and would have to RE-IMPLEMENT
//! `paper < demo < live` plus the default-inherits / labelled-does-not asymmetry — a second
//! predicate that must agree with the first, which is the defect shape
//! `crates/vike-mount/src/arming.rs`'s `venue_ceiling` was collapsed into one function to escape.
//!
//! `vike-config` already declares `vike-secrets` as a NORMAL dependency (for the boot disclosure),
//! so the edge this gate needs already exists and points the only way it can. Nothing is added to
//! any manifest.
//!
//! ⚠ `crates/vike-config/Cargo.toml` says this crate *"never opens the store, and must not"*. That
//! is a rule about `src/`, and it is not bent here: this is a test target, and
//! `crates/vike-config/tests/mirror.rs` already builds real stores through `vike_secrets::migrate`
//! for the same reason.
//!
//! # What is compared, exactly
//!
//! * **old** — `VenuePolicy::account(venue, label)`, CAPPED by what the account's own credential
//!   set can reach (`VenueMode::cap` against its `tier`). That is the old model's answer for one
//!   account row: the venue's line, folded with the account's own line by the inheritance rule,
//!   and then bounded by the fact that a demo credential set cannot trade live.
//! * **new** — §3.2's `armed ? tier : paper`.
//!
//! ⚠ **`VenuePolicy::get` is NOT used and must not be.** It agrees with `VenuePolicy::account` for
//! the DEFAULT account and differs for every LABELLED one, so a gate written on it cannot see a
//! labelled widening at all. [`the_ruled_fold_widens_a_labelled_account_with_no_line_of_its_own`]
//! proves that by running both.
//!
//! ⚠ The cap is what makes this STRICTLY STRONGER than the spec's literal wording. The spec says
//! `new <= VenuePolicy::account(...)`; capping lowers the RIGHT-hand side of that `<=` — the
//! thing `new` must stay under — so every comparison here is at least as demanding as the one
//! §5.3 asks for, and one of them, the hyperliquid demo row, is genuinely more so.
//!
//! ⚠ **The old ceiling is resolved from the row that WENT IN**, by `account.id`, never from the
//! row the reshape handed back. [`went_in`] carries what recomputing it from the after row cost.
//!
//! # What this gate's SUBJECT is
//!
//! ⚠ **STAGE 3 HAS LANDED, and this section used to describe the handover rather than the state.**
//! `account.armed` exists (`crates/vike-secrets/src/schema.rs`'s `DDL`), the fold is
//! `crates/vike-secrets/src/settings.rs`'s `fold_arming_into_accounts`, and [`SUBJECT`] is no
//! longer a model of it: it READS the column back out of the migrated store, so **every assertion
//! below judges the shipped code**. The tombstone that demanded this swap
//! (`the_shipped_ddl_has_no_armed_column_yet`) is deleted, which is step 3 of the handover it
//! carried; steps 1 and 2 are the fold and this function.
//!
//! The three arms, as `fold_arming_into_accounts`' `account_ceiling` spells them and as
//! `VenuePolicy::account` spells them — the equality this whole file exists to hold:
//!
//! 1. an UNLABELLED account takes the venue arming row (absent ⇒ `paper`);
//! 2. a LABELLED account with a labelled row takes `min(venue row, labelled row)`;
//! 3. a LABELLED account with NO labelled row is armed only where its own tier is already `paper`.
//!
//! ⚠ That third arm read "is never armed" until the Task 4 re-review measured [`SUBJECT`] and
//! found otherwise: a `paper`-tier labelled account resolves to a `Paper` ceiling, so the ceiling
//! EQUALS the tier and it ARMS. The effective behaviour is `paper` either way, so the gate was
//! never wrong and no widening was reachable — but the `armed` COLUMN is written from this
//! sentence, and the two spellings produce a different BIT. (That row's tier was spelled `sim`
//! when this was measured; §4.4's rename is what made the two words one, and the observation is
//! unchanged by it.)
//!
//! Three rejected spellings stay beside it as kill proofs: [`STRUCK`] (§5.2 step 5 as SIGNED),
//! [`RULED`] (§5.2 step 5 as AMENDED, which reads the venue arming row only), and [`UNCAPPED`]
//! (the amended PROSE read literally, which forgets the venue cap). Each widens a legal store, and
//! each has a test proving this gate refuses it — so the shipped fold agreeing is demonstrably the
//! RULE working rather than an accident of the fixtures.
//!
//! ⚠ **`vike-secrets` (layer 15) still cannot call `VenuePolicy::account` (layer 20), so the three
//! arms exist TWICE and this gate is the only thing holding them equal.** That is a declared
//! residual rather than an oversight — it is a gate, not a compiler, and it holds them equal over
//! the four fixture shapes below and nowhere else.
//!
//! ⚠ **What the shipped fold does NOT do, so this file is not read as proving more than it does:**
//! `venue_arming` is not dropped, and `armed` has no consumer on the mount path yet — the ceiling
//! `vike_mount::make_engine` reads is still `VenuePolicy`, built from those same rows. So
//! `new_effective` below is what the column MEANS (§3.2's `armed ? tier : paper`), not yet what
//! the box does. `fold_arming_into_accounts`' own doc carries why the table stays.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use vike_config::{VenueMode, VenuePolicy, apply_rows, load, rows_from_files};
use vike_model::account_keys::AccountLabel;
use vike_secrets::{Account, AccountKey, ArmingRow, Classification, Placement};

// ---------------------------------------------------------------------------------------------
// The vocabulary, derived rather than spelled
// ---------------------------------------------------------------------------------------------

/// One of `VenueMode::ALL`, by its own `as_str` — so this cannot drift from the vocabulary the
/// parser accepts, and a fourth mode would land here rather than in a `match` nobody updated.
///
/// ⚠ **This file carried a `mode_word_of_tier` beside it until stage 4b landed**, because
/// `account.tier`'s word for "no real broker connection" was `sim` while `venue_arming.mode`'s was
/// `paper`, so a fold comparing the two columns raw matched NOTHING while looking like a clean
/// pass. §4.4's rename (ruling 7) makes them one word; the map is DELETED here and in
/// `vike_secrets::settings`' shipped fold together, and the `panic!` below is what now catches a
/// tier this vocabulary does not carry — which is a louder failure than the silent identity arm
/// that map ended in.
fn mode(word: &str) -> VenueMode {
    VenueMode::ALL
        .iter()
        .copied()
        .find(|m| m.as_str() == word)
        .unwrap_or_else(|| panic!("{word:?} is outside `VenueMode::ALL` — an illegal mode word"))
}

/// The policy's view of one account row. Every row a MIGRATION writes carries `label: None`
/// (`vike_secrets::Account`'s own doc), which is `AccountLabel::Default` — but a `__LABEL` key
/// name mints a labelled one through the ordinary venue grammar, which is why this is a
/// conversion rather than a constant.
fn label_of(account: &Account) -> AccountLabel {
    match account.label.as_deref() {
        None => AccountLabel::Default,
        Some(text) => AccountLabel::parse(text).unwrap_or_else(|e| {
            panic!("the store holds the illegal account label {text:?}: {e:?}")
        }),
    }
}

/// **The OLD effective ceiling of one account row** — the venue's line folded with the account's
/// own by `VenuePolicy::account`, then capped by what that account's credential set can reach.
fn old_effective(policy: &VenuePolicy, account: &Account) -> VenueMode {
    policy.account(&account.venue, &label_of(account)).cap(mode(&account.tier))
}

/// **The NEW effective behaviour of one account row** — §3.2's `armed ? tier : paper`, and the
/// whole of it.
fn new_effective(account: &Account, armed: bool) -> VenueMode {
    if armed { mode(&account.tier) } else { VenueMode::Paper }
}

// ---------------------------------------------------------------------------------------------
// The candidate reshapes
// ---------------------------------------------------------------------------------------------

/// A candidate 2→3 fold: every `account` row the reshape writes, paired with the `armed` bit it
/// writes onto it. A real reshape's OUTPUT — which is why the kill proofs can express a reshape
/// that drops a row as well as one that arms the wrong one.
type Reshape = fn(&Fixture) -> Vec<(Account, bool)>;

/// The venue-level `venue_arming` row for a venue, if the store holds one.
fn venue_mode_of(arming: &[ArmingRow], venue: &str) -> Option<String> {
    arming.iter().find(|r| r.venue == venue && r.label.is_none()).map(|r| r.mode.clone())
}

/// **The RULED rule** — §5.2 step 5 as amended on 2026-09-23: *armed for the account whose `tier`
/// equals the venue's old `venue_arming.mode`, false for every other account of that venue.*
///
/// ⚠ **A kill proof, NOT the subject.** It reads the VENUE row and nothing else, so it widens a
/// labelled account that has no `[accounts]` line —
/// [`the_ruled_fold_widens_a_labelled_account_with_no_line_of_its_own`] measures that. [`SUBJECT`]
/// is what stage 3 must implement, and is byte-identical to this on every store whose accounts are
/// all unlabelled.
fn ruled(fx: &Fixture) -> Vec<(Account, bool)> {
    fx.accounts
        .iter()
        .map(|a| {
            let venue_mode = venue_mode_of(&fx.arming, &a.venue);
            let armed = venue_mode.as_deref() == Some(a.tier.as_str());
            (a.clone(), armed)
        })
        .collect()
}

/// [`ruled`], as a [`Reshape`].
const RULED: Reshape = ruled;

/// **The STRUCK wording** — what §5.2 step 5 said as SIGNED, before the 2026-09-23 ruling:
/// *`armed = 1` exactly where the old effective ceiling for that account was above `paper`.*
/// Kept because a gate that has never refused anything is not evidence that nothing is wrong.
fn struck(fx: &Fixture) -> Vec<(Account, bool)> {
    fx.accounts
        .iter()
        .map(|a| (a.clone(), old_effective(&fx.policy, a) > VenueMode::Paper))
        .collect()
}

/// [`struck`], as a [`Reshape`].
const STRUCK: Reshape = struck;

/// **THE SUBJECT — the SHIPPED fold, read back out of the store it wrote.**
///
/// ⚠ **This was a MODEL until stage 3 landed** — `fx.policy.account(venue, label) == tier`,
/// computed here in `vike-config` where `VenuePolicy::account` is nameable. It is now the
/// `account.armed` column that `crates/vike-secrets/src/settings.rs`'s
/// `fold_arming_into_accounts` derived, carried on [`vike_secrets::Account::armed`] and read by
/// [`Fixture::read_back`] through the ordinary `resolve_accounts_in` front door. So every
/// assertion in this file judges the shipped code rather than a description of it, and the swap
/// cost exactly this one function — which is what the tombstone's handover promised.
///
/// The bits it reads cannot widen by construction, and the argument is one line: an armed row
/// comes out at a tier its own ceiling already allowed, and a disarmed one comes out `paper`. What
/// this gate adds is that the CEILING the fold computed is the one `VenuePolicy::account`
/// computes — two spellings of three arms, held equal by [`objections`] over the fixtures below.
///
/// ⚠ **The store is read AFTER `write_settings_in`, and that order is load-bearing.** The fold is
/// derived from the `venue_arming` rows, and `Fixture::build_with` writes those AFTER `migrate`
/// has minted the accounts — so a fold that ran only in the schema reshape would leave every bit
/// at the DDL's `DEFAULT 0` and this gate would read 16 disarmed rows: a NARROWING, which the
/// inequality tolerates and
/// [`the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing`] is what
/// refuses.
fn subject(fx: &Fixture) -> Vec<(Account, bool)> {
    fx.accounts.iter().map(|a| (a.clone(), a.armed)).collect()
}

/// [`subject`], as a [`Reshape`].
const SUBJECT: Reshape = subject;

/// **The spec's amended PROSE, taken literally** — *"read the LABELLED arming row"* — with the
/// venue cap `VenuePolicy::account` applies (`(Some(mode), _) => venue_ceiling.cap(mode)`) left
/// out. A fold written from that sentence rather than from the function arms a labelled account
/// whose own line names a HIGHER tier than its venue's line, which the old model capped on read.
/// Kept as a kill proof so the difference between the cure and luck is executable.
fn uncapped(fx: &Fixture) -> Vec<(Account, bool)> {
    fx.accounts
        .iter()
        .map(|a| {
            let stated = match label_of(a).text() {
                // The labelled row, read WITHOUT the venue cap — the omission under test.
                Some(label) => fx
                    .arming
                    .iter()
                    .find(|r| r.venue == a.venue && r.label.as_deref() == Some(label))
                    .map(|r| r.mode.clone()),
                None => venue_mode_of(&fx.arming, &a.venue),
            };
            (a.clone(), stated.as_deref() == Some(a.tier.as_str()))
        })
        .collect()
}

/// [`uncapped`], as a [`Reshape`].
const UNCAPPED: Reshape = uncapped;

// ---------------------------------------------------------------------------------------------
// The check
// ---------------------------------------------------------------------------------------------

/// **The row as it WENT IN**, by `account.id`.
///
/// ⚠ **This is the whole of the fix for the defect this gate shipped with on `8b3fe904b`, and the
/// defect is worth carrying because it is the easiest one in this file to write again.** The old
/// ceiling was computed from the row the reshape HANDED BACK, which reads `tier` and `label` off
/// the AFTER row — so a test named *"higher than it went in"* never looked at what went in. A
/// reshape rewriting `hyperliquid/demo` as `tier = live, armed = 1` came out `live`, and "old" was
/// recomputed from that same row as `live.cap(live)` = `live`: no change, green, and the gate
/// blessed the exact widening it exists to catch. Latent only because every subject here
/// `.clone()`s its rows — and §5.2 step 5 REWRITES `tier` (`sim` -> `paper`) on every row it
/// copies, so it would have gone live with stage 3.
///
/// `id` is the ruled identity, and the lookup cannot fail once the row-set check below has passed.
fn went_in(fx: &Fixture, id: i64) -> Option<&Account> {
    fx.accounts.iter().find(|a| a.id == id)
}

/// **Every objection to one candidate reshape**, as sentences naming the row.
///
/// Two independent families, and BOTH are needed:
///
/// 1. **The row set is preserved.** `armed NOT NULL DEFAULT 0` makes every row a reshape forgets
///    read `paper`, which is a NARROWING — so a fold that drops accounts passes an
///    inequality-only gate and the gate blesses a reshape that loses them.
///
///    ⚠ The load-bearing part is the KEY, not the multiset: rows are compared by `account.id`,
///    the ruled identity, because dukascopy's two demo books are INDISTINGUISHABLE by
///    `(venue, tier, label)` — both are `(dukascopy, demo, NULL)` — so a check keyed on that tuple
///    could not tell a lost book from a kept one. Ids being unique, a sorted vector and a set
///    would both catch a drop; the sorted vector additionally refuses a reshape that emits one id
///    TWICE, which a set folds away.
/// 2. **No row widens.** `new_effective <= old_effective`, per row, with **old resolved from the
///    BEFORE row** ([`went_in`]) and new from the after row. Equality is the expectation; the
///    assertion is the inequality because a narrowing is safe and a widening is not.
fn objections(fx: &Fixture, after: &[(Account, bool)]) -> Vec<String> {
    let mut found = Vec::new();

    let mut before_ids: Vec<i64> = fx.accounts.iter().map(|a| a.id).collect();
    let mut after_ids: Vec<i64> = after.iter().map(|(a, _)| a.id).collect();
    before_ids.sort_unstable();
    after_ids.sort_unstable();
    if before_ids != after_ids {
        found.push(format!(
            "the reshape did not preserve the account rows: {} went in as ids {before_ids:?} and \
             {} came out as ids {after_ids:?}. A dropped row reads `paper` under \
             `armed NOT NULL DEFAULT 0`, which is a NARROWING — so the inequality below cannot \
             see it and this check is what does.",
            before_ids.len(),
            after_ids.len()
        ));
    }

    for (account, armed) in after {
        // A row with no `went_in` twin is already reported above, by id, with the whole set named.
        let Some(before) = went_in(fx, account.id) else { continue };
        let old = old_effective(&fx.policy, before);
        let new = new_effective(account, *armed);
        if new > old {
            found.push(format!(
                "account {} WIDENS: it went in as {}/{} label {:?}, effective `{old}`, and comes \
                 out as {}/{} label {:?}, effective `{new}` (armed = {armed}). The venue's old \
                 arming row says {:?}.",
                account.id,
                before.venue,
                before.tier,
                before.label,
                account.venue,
                account.tier,
                account.label,
                venue_mode_of(&fx.arming, &before.venue),
            ));
        }
    }

    found
}

/// How each row MOVED, for the equality pin — the named expectation, not the assertion.
#[derive(Debug, PartialEq, Eq)]
enum Move {
    /// The row's effective behaviour is byte-for-byte what it was.
    Unchanged,
    /// The row comes out LOWER than it went in. Safe, and the thing the inequality tolerates.
    Narrowed,
    /// The row comes out HIGHER. [`objections`] is what refuses this; the variant exists so the
    /// equality pin cannot silently read a widening as a narrowing.
    Widened,
}

fn moves(fx: &Fixture, after: &[(Account, bool)]) -> Vec<(i64, Move)> {
    after
        .iter()
        .map(|(a, armed)| {
            let old = went_in(fx, a.id).map_or(VenueMode::Paper, |b| old_effective(&fx.policy, b));
            let new = new_effective(a, *armed);
            let how = match new.cmp(&old) {
                std::cmp::Ordering::Equal => Move::Unchanged,
                std::cmp::Ordering::Less => Move::Narrowed,
                std::cmp::Ordering::Greater => Move::Widened,
            };
            (a.id, how)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// One venue of a fixture store: `(roster venue id, its `[venues]` line, the credential key names
/// that mint its accounts)`.
///
/// ⚠ A three-position tuple rather than a named struct, and the reason is rustfmt rather than
/// taste: `scripts/new_venue.sh` renders a row here from the marker in [`PROD2_SHAPE`], and a
/// `MAX_VENUE_ID_LEN`-length venue id in the named-struct spelling renders a line of exactly
/// `max_width`. One more field, or one longer word, and the scaffold's own output fails
/// `cargo fmt --check` — a failure `crates/vike-ops/tests/new_venue_gate.rs` declares it cannot
/// see (its third residual: line-length reformatting is out of reach entirely). The tuple spelling
/// renders ~35 columns short of the limit, which is slack rather than luck.
type VenueFixture = (&'static str, &'static str, &'static [&'static str]);

/// **The the CI box shape**, as MEASURED on 2026-09-23 and recorded in §5.2 step 5: 16 account rows
/// against 14 venue lines, every venue's accounts at the tier that venue is mounted at, except
/// hyperliquid — mode `live`, holding BOTH a `demo` and a `live` account.
///
/// ⚠ **This is CONSTRUCTED to that measurement, not copied from it.** the CI box's store holds live
/// venue credentials and can never be a repository fixture. What is reproduced is the SHAPE: the
/// roster's own 14 venues (`vike_model::VENUES` has exactly 14, which is where the "14 venue
/// lines" comes from — a declared `[venues]` table mirrors ROSTER-COMPLETE), the two venues that
/// carry more than one account, and the one venue whose mode names only one of its two tiers.
/// The two extra rows are hyperliquid's second tier and dukascopy's second BOOK — the only
/// `(venue, tier)` pair in the migration that yields two accounts
/// (`crates/vike-secrets/src/venue_setting.rs`'s `HAND_MAPPED_ACCOUNTS` carries the ruling).
///
/// ⚠ **This is a per-venue table and a new venue needs a row**, which is why it carries a scaffold
/// marker: the mirror writes one `venue_arming` row per ROSTER venue whether this table names the
/// venue or not, so a roster that grew past this table would leave a venue line with no account
/// under it and the counts in [`the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing`]
/// would stop describing anything. That test's three literals MOVE WITH THIS TABLE — deliberately,
/// because they are a MEASUREMENT of a store and not a property of the code.
const PROD2_SHAPE: &[VenueFixture] = &[
    ("binance", "live", &["BINANCE_LIVE_API_KEY"]),
    ("bybit", "live", &["BYBIT_LIVE_API_KEY"]),
    ("okx", "live", &["OKX_LIVE_API_KEY"]),
    ("deribit", "live", &["DERIBIT_LIVE_API_KEY"]),
    ("oanda", "demo", &["OANDA_DEMO_API_KEY"]),
    ("ig", "demo", &["IG_DEMO_API_KEY"]),
    ("fxcm", "demo", &["FXCM_DEMO_API_KEY"]),
    ("dukascopy", "demo", &["DUKASCOPY_DEMO1_LOGIN", "DUKASCOPY_DEMO2_LOGIN"]),
    ("polymarket", "live", &["POLY_PRIVATE_KEY"]),
    ("ibkr", "demo", &["IBKR_DEMO_API_KEY"]),
    ("ctrader", "demo", &["CTRADER_DEMO_API_KEY"]),
    ("alpaca", "demo", &["ALPACA_SANDBOX_API_KEY"]),
    ("aster", "live", &["ASTER_LIVE_API_KEY"]),
    ("hyperliquid", "live", &["HYPERLIQUID_DEMO_API_KEY", "HYPERLIQUID_LIVE_API_KEY"]),
    // vike:new-venue:row // TODO(new-venue: {venue}): the scaffolded row gives this venue ONE demo
    // vike:new-venue:row // account whose tier equals its line, which is the shape every venue but
    // vike:new-venue:row // hyperliquid has. Replace `demo` and the key name with what the box you
    // vike:new-venue:row // are describing actually holds, and UPDATE THE THREE COUNTS in
    // vike:new-venue:row // `the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing`
    // vike:new-venue:row // — they are a measurement of a store, so they move when the store does.
    // vike:new-venue:row ("{venue}", "demo", &["{VENUE}_DEMO_API_KEY"]),
];

/// A store whose venue line names a LOWER tier than one of its accounts carries — the shape the
/// STRUCK wording arms. No live box holds it today; a box whose operator caps a venue to `demo`
/// while its live keys are still filed does.
const A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE: &[VenueFixture] =
    &[("bybit", "demo", &["BYBIT_DEMO_API_KEY", "BYBIT_LIVE_API_KEY"])];

/// A venue holding a LABELLED second account with no `[accounts]` line of its own — the shape
/// `VenuePolicy::account` resolves to `paper` and the venue-row-only fold arms.
const A_LABELLED_ACCOUNT_WITH_NO_LINE: &[VenueFixture] =
    &[("binance", "live", &["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_KEY__ALT"])];

/// One `policy.toml` `[accounts]` line: `(roster venue id, account label, mode)`.
type AccountLine = (&'static str, &'static str, &'static str);

/// A venue whose LABELLED account states a HIGHER tier than the venue's own line — the arm of
/// `VenuePolicy::account` that no other fixture reaches (`(Some(mode), _) => venue_ceiling.cap(mode)`).
///
/// `binance = "demo"` with `[accounts.binance] ALT = "live"` is a legal file, and the old model
/// caps it ON READ: `ALT` went in at `min(demo, live)` = `demo`. A fold that reads the labelled
/// row and forgets the cap arms it to `live`. [`UNCAPPED`] is that fold, and
/// [`the_uncapped_labelled_line_is_caught_as_a_widening`] is the proof that the gate refuses it —
/// which is what tells a later author that [`SUBJECT`] disarming it is the RULE working rather
/// than an accident of the other fixtures.
const A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE: (&[VenueFixture], &[AccountLine]) = (
    &[("binance", "demo", &["BINANCE_DEMO_API_KEY", "BINANCE_LIVE_API_KEY__ALT"])],
    &[("binance", "ALT", "live")],
);

/// A real schema-2 store, built the way a real box gets one.
struct Fixture {
    _dir: tempfile::TempDir,
    /// `account` rows, as the store answers them.
    accounts: Vec<Account>,
    /// `venue_arming` rows, as the store answers them.
    arming: Vec<ArmingRow>,
    /// The old model's resolution, built from the STORE'S ROWS and from no file.
    policy: VenuePolicy,
}

/// **The classifier**, spelled here because `vike-config` cannot name the crate that owns the
/// production one (`vike_bridge_core::credentials`' `classify_credential_name`, layer 30) — the
/// same seam, and the same reason, that `crates/vike-secrets/tests/support/mod.rs` spells its own.
///
/// It is not a hand-written map: it is the production classifier's own two tables, reached from
/// the two crates this one CAN name — `crates/vike-secrets/src/venue_setting.rs`'s
/// `HAND_MAPPED_ACCOUNTS` (whose rows are the non-conforming families) and
/// `crates/vike-model/src/account_keys.rs`'s `account_ref_from_key` (the venue grammar, including
/// the `__LABEL` suffix). A name neither answers for is reported unrecognised, exactly as
/// production does.
fn classify(name: &str) -> Classification {
    let account = |key: AccountKey, field: &str| Classification {
        placement: Placement::Account(key),
        field: field.to_string(),
        secret: true,
        recognised: true,
        pending_move: None,
    };

    for &(head, token, venue, tier, discriminator, _why) in vike_secrets::HAND_MAPPED_ACCOUNTS {
        let prefix = vike_secrets::hand_mapped_prefix(head, token);
        if let Some(field) = name.strip_prefix(&prefix) {
            return account(
                AccountKey {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    label: None,
                    discriminator: discriminator.map(str::to_string),
                },
                field,
            );
        }
    }

    if let Some(reference) = vike_model::account_keys::account_ref_from_key(name) {
        let head = format!("{}_{}_", reference.venue.to_uppercase(), reference.tier);
        let base = name.split(vike_model::account_keys::ACCOUNT_SEPARATOR).next().unwrap_or(name);
        let field = base.strip_prefix(&head).unwrap_or(base);
        return account(
            AccountKey {
                venue: reference.venue.to_string(),
                tier: reference.tier.to_ascii_lowercase(),
                label: reference.label.text().map(str::to_string),
                discriminator: None,
            },
            field,
        );
    }

    Classification::unrecognised(name)
}

impl Fixture {
    /// [`Fixture::build_with`] for a store whose `policy.toml` states no `[accounts]` table, which
    /// is every box today.
    fn build(venues: &[VenueFixture]) -> Fixture {
        Fixture::build_with(venues, &[])
    }

    /// Build a schema-2 store from a set of venue lines and their credential keys, the way a real
    /// box gets one: credentials into `secrets.env`, `vike_secrets::migrate` to mint the `account`
    /// rows, a `policy.toml` `[venues]` (and optional `[accounts]`) table, and
    /// `crates/vike-config/src/mirror.rs`'s `rows_from_files` into `venue_arming`.
    fn build_with(venues: &[VenueFixture], accounts: &[AccountLine]) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path();

        let mut env = String::new();
        for (_venue, _mode, keys) in venues {
            for key in *keys {
                env.push_str(&format!("{key}=not-a-real-key-{key}\n"));
            }
        }
        std::fs::write(settings.join("secrets.env"), env).expect("write the credential file");

        // ⚠ The credentials come FIRST and are not decoration: `migrate` opens no write connection
        // when there is nothing to carry, so a store exists here only because a credential asked
        // for one — which is how a real box gets one.
        let path = settings.to_str().expect("utf-8 temp path");
        vike_secrets::migrate(Some(path), |_| false, &classify).expect("the migration ran");

        let mut policy_toml = String::from("[venues]\n");
        for (venue, mode, _keys) in venues {
            policy_toml.push_str(&format!("{venue} = \"{mode}\"\n"));
        }
        for (venue, label, mode) in accounts {
            policy_toml.push_str(&format!("\n[accounts.{venue}]\n{label} = \"{mode}\"\n"));
        }
        std::fs::write(settings.join("policy.toml"), policy_toml).expect("write policy.toml");

        let rows = rows_from_files(settings).expect("the files mirror");
        vike_secrets::write_settings_in(settings, &rows).expect("the rows land in the store");

        Fixture::read_back(dir)
    }

    /// Re-open the store and read the two tables the migration folds, plus the old model's
    /// resolution OF THOSE ROWS.
    fn read_back(dir: tempfile::TempDir) -> Fixture {
        let settings = dir.path().to_path_buf();
        let accounts = match vike_secrets::resolve_accounts_in(&settings).expect("the store opened")
        {
            vike_secrets::Accounts::Known(rows) => rows,
            vike_secrets::Accounts::Unanswerable(why) => {
                panic!("the store could not be asked for its accounts: {why}")
            }
        };
        let source = vike_secrets::read_settings_in(&settings).expect("the settings tables read");
        let stored = source.rows().expect("the tables are there").clone();

        // ⚠ **The policy is built from the STORE'S ROWS, never from `policy.toml`.** §5.4: an
        // ADOPTED box has no files under its rows, so the rows are the only copy of every ceiling
        // and the migration cannot fall back to re-reading the file. An unadopted box could, and
        // must still not — two paths would answer differently. `load(None, …)` hands `apply_rows`
        // a `Settings` with no file layer at all, which is that shape exactly.
        let mut settings_model = load(None, &HashMap::new()).expect("the default settings load");
        apply_rows(&mut settings_model, &stored).expect("the store's rows apply");

        Fixture { _dir: dir, accounts, arming: stored.arming, policy: settings_model.policy.venues }
    }

    /// The row an assertion means, by the cells a listing shows.
    fn id_of(&self, venue: &str, tier: &str) -> i64 {
        let hit: Vec<&Account> =
            self.accounts.iter().filter(|a| a.venue == venue && a.tier == tier).collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{tier} row: {hit:?}");
        hit[0].id
    }

    /// …and the LABELLED row, which `tier` alone cannot address: a venue can hold a default and a
    /// labelled account at one tier, which is what `A_LABELLED_ACCOUNT_WITH_NO_LINE` is.
    fn id_of_label(&self, venue: &str, label: &str) -> i64 {
        let hit: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| a.venue == venue && a.label.as_deref() == Some(label))
            .collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{label} row: {hit:?}");
        hit[0].id
    }
}

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

/// **THE GATE.** For every account on a store built from the fixtures: the new effective behaviour
/// is never ABOVE the old. Equality is the expectation; the assertion is the inequality because a
/// NARROWING is safe and a widening is not — and the row set is asserted separately, because a
/// fold that DROPS rows narrows them all and would otherwise pass.
///
/// ⚠ **[`PROD2_SHAPE`] alone cannot EXPRESS a widening, and running only it would be an assertion
/// that cannot fail for its stated reason.** A widening needs `tier > min(ceiling, tier)`, i.e.
/// `tier > ceiling` — and on that fixture every account's tier is at or below its venue's line, so
/// the property holds there for ANY assignment of `armed` whatsoever, including a deliberately
/// wrong one. The widening half is carried entirely by fixtures where a venue line sits BELOW an
/// account it covers, which is why [`A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE`] runs here too and why
/// [`the_struck_rule_from_the_signed_spec_is_caught_as_a_widening`] exists.
///
/// ⚠ **The subject is [`SUBJECT`] and NOT [`RULED`], and that is a finding rather than a
/// preference.** §5.2 step 5's venue-row-only wording widens a labelled account on
/// [`A_LABELLED_ACCOUNT_WITH_NO_LINE`], which is a legal store an operator produces by filing one
/// `__LABEL` credential key. Running the headline gate on [`RULED`] and omitting that fixture
/// would let stage 3 ship the venue-row-only fold with this gate green — the exact defect this
/// task found. So EVERY fixture in this file runs here, and the two rejected spellings are kill
/// proofs below.
#[test]
fn no_account_comes_out_of_the_migration_armed_higher_than_it_went_in() {
    for shape in [PROD2_SHAPE, A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE, A_LABELLED_ACCOUNT_WITH_NO_LINE] {
        let fx = Fixture::build(shape);
        let found = objections(&fx, &SUBJECT(&fx));
        assert!(
            found.is_empty(),
            "the 2->3 fold widens an account's ceiling:\n{}",
            found.join("\n")
        );
    }

    let (venues, accounts) = A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE;
    let fx = Fixture::build_with(venues, accounts);
    let found = objections(&fx, &SUBJECT(&fx));
    assert!(
        found.is_empty(),
        "the 2->3 fold widens an account whose own `[accounts]` line sits above its venue's:\n{}",
        found.join("\n")
    );
}

/// **A kill proof for the CRITICAL defect this gate shipped with**, and the reason [`went_in`]
/// exists.
///
/// The reshape hands back the rows it wrote, so it can rewrite them — and §5.2 step 5 DOES rewrite
/// `tier` on every row it copies (`sim` -> `paper`). A gate that recomputed the old ceiling from
/// the returned row would read a rewritten `tier = live` as *this row always could reach live* and
/// see no change at all. Here the subject rewrites `hyperliquid/demo` upward and arms it: it went
/// in at `live.cap(demo)` = `demo` and comes out `live`, and the gate must say so.
#[test]
fn a_reshape_that_rewrites_a_rows_tier_upward_cannot_hide_the_widening() {
    let fx = Fixture::build(PROD2_SHAPE);
    let target = fx.id_of("hyperliquid", "demo");
    let mut after = SUBJECT(&fx);
    for (account, armed) in &mut after {
        if account.id == target {
            account.tier = "live".to_string();
            *armed = true;
        }
    }

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains(&format!("account {target} ")) && f.contains("WIDENS")),
        "the old ceiling must be resolved from the row that WENT IN, or a reshape hides a \
         widening simply by rewriting the row it is judged on: {found:?}"
    );
    assert!(
        moves(&fx, &after).contains(&(target, Move::Widened)),
        "…and the equality pin must read it as WIDENED rather than as a narrowing"
    );

    // …and the defect is DEMONSTRATED rather than described: judged on the row handed back — the
    // spelling this file shipped with — the very same reshape raises nothing at all. Without this
    // line the assertion above would pass under both spellings and prove nothing about either.
    assert_eq!(
        judged_on_the_returned_row(&fx, &after),
        0,
        "recomputing the old ceiling from the AFTER row must be the thing that goes blind here; \
         if it now objects too, this kill proof has stopped exercising the fix"
    );
}

/// The comparison this file shipped with on `8b3fe904b`: old recomputed from the row the reshape
/// HANDED BACK. Kept as a measuring instrument so the kill proofs above can show the two spellings
/// disagree, rather than merely asserting that today's one is right.
fn judged_on_the_returned_row(fx: &Fixture, after: &[(Account, bool)]) -> usize {
    after
        .iter()
        .filter(|(a, armed)| new_effective(a, *armed) > old_effective(&fx.policy, a))
        .count()
}

/// **The same defect wearing the other column, which needs no tier rewrite at all.** §5.2 step 5
/// copies `label` too. Drop a labelled row's `label` and, judged on the after row, the account
/// resolves as its venue's DEFAULT — which inherits the venue line — so `paper` -> `live` reads as
/// no change.
#[test]
fn a_reshape_that_drops_a_rows_label_cannot_hide_the_widening() {
    let fx = Fixture::build(A_LABELLED_ACCOUNT_WITH_NO_LINE);
    let target = fx.id_of_label("binance", "ALT");

    let mut after = SUBJECT(&fx);
    for (account, armed) in &mut after {
        if account.id == target {
            account.label = None;
            *armed = true;
        }
    }

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains(&format!("account {target} ")) && f.contains("WIDENS")),
        "a labelled account went in at `paper` (it has no `[accounts]` line); a reshape that \
         drops the label and arms it comes out `live`, and the gate must say so: {found:?}"
    );

    // …and the same demonstration: on the returned row the account reads as its venue's DEFAULT,
    // which INHERITS the venue line, so the old spelling sees `live` -> `live` and says nothing.
    assert_eq!(
        judged_on_the_returned_row(&fx, &after),
        0,
        "the label column must be the thing that goes blind here; if it now objects too, this \
         kill proof has stopped exercising the fix"
    );
}

/// **A kill proof for the spec PROSE's own omission**, and the test that makes [`SUBJECT`]'s
/// answer on a stated labelled line demonstrably the rule rather than luck.
///
/// §5.2's amended wording says *read the LABELLED arming row* and does not mention the venue cap
/// `VenuePolicy::account` applies. On [`A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE`] —
/// `binance = "demo"` with `[accounts.binance] ALT = "live"` — that omission arms `ALT` to `live`
/// where the old model capped it to `demo`.
#[test]
fn the_uncapped_labelled_line_is_caught_as_a_widening() {
    let (venues, accounts) = A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE;
    let fx = Fixture::build_with(venues, accounts);
    assert!(
        fx.arming.iter().any(|r| r.label.as_deref() == Some("ALT") && r.mode == "live"),
        "the fixture must put a STATED labelled arming row in the store — the one arm of \
         `VenuePolicy::account` no other fixture here reaches: {:?}",
        fx.arming
    );
    let alt = fx.id_of_label("binance", "ALT");

    let found = objections(&fx, &UNCAPPED(&fx));
    assert!(
        found.iter().any(|f| f.contains(&format!("account {alt} ")) && f.contains("WIDENS")),
        "reading the labelled arming row WITHOUT the venue cap arms an account the old model \
         capped on read, and the gate must say so: {found:?}"
    );

    // …and the SHIPPED fold, which applies the cap, does not.
    //
    // ⚠ The `armed` BIT is asserted directly rather than inferred from the absence of an
    // objection. The two are equivalent HERE — arming this row would widen, so an empty objection
    // list implies a `false` bit — but the message above says *it disarms this account*, and a
    // test whose message names a stronger fact than its assertion is the shape that goes quietly
    // wrong when a fixture moves. Carried from Task 4's deferred Minor M-b, which was deferred
    // precisely because `SUBJECT` was a model and this is now the column.
    let armed_alt = SUBJECT(&fx)
        .into_iter()
        .find(|(a, _)| a.id == alt)
        .map(|(_, armed)| armed)
        .expect("the ALT row is in the fold's output");
    assert!(
        !armed_alt,
        "the shipped fold caps the stated `ALT = \"live\"` line by its venue's `demo` line, so \
         this account must come out DISARMED — `armed = true` here is the widening `UNCAPPED` \
         above was just refused for"
    );
    assert!(
        objections(&fx, &SUBJECT(&fx)).is_empty(),
        "…and nothing else on this store widens either"
    );
}

/// **…and that claim is MEASURED rather than asserted.** Arm EVERY row of the the CI box fixture — the
/// most aggressive assignment there is, and one no rule would produce — and the widening half
/// still raises nothing, because every account's tier already sits at or below its venue's line.
///
/// This is here so a reader does not mistake the the CI box fixture for the thing that guards against
/// widening. It guards the row set and the EQUALITY; the adversarial fixtures guard the
/// inequality.
#[test]
fn arming_every_row_of_the_prod2_fixture_still_widens_nothing() {
    let fx = Fixture::build(PROD2_SHAPE);
    let all_armed: Vec<(Account, bool)> = fx.accounts.iter().map(|a| (a.clone(), true)).collect();
    let found = objections(&fx, &all_armed);
    assert!(
        found.is_empty(),
        "the the CI box shape was believed unable to express a widening and it just did — re-read the \
         fixture before trusting anything else in this file: {found:?}"
    );
}

/// **The the CI box measurement, as a named expectation** — §5.2 step 5, MEASURED 2026-09-23. A
/// different answer on this shape is a defect, not a surprise.
///
/// ⚠ The spec's *"no row changes behaviour"* is a claim about the DEPLOYED behaviour and is true
/// there: hyperliquid's demo account is not trading today either, because one process mounts a
/// venue at one tier (`crates/vike-mount/src/arming.rs`'s `account_ceiling` carries that sentence)
/// and this box mounts hyperliquid at live. It is NOT true of the per-row ceiling arithmetic this
/// gate can compute, where that row goes `demo` -> `paper`. So the pin below is 15 rows UNCHANGED
/// and exactly ONE narrowing, NAMED — which is strictly stronger than pinning 16 unchanged would
/// have been, because it refuses any OTHER row moving at all.
#[test]
fn the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing() {
    let fx = Fixture::build(PROD2_SHAPE);
    assert_eq!(fx.accounts.len(), 16, "16 account rows: {:?}", fx.accounts);
    assert_eq!(
        fx.arming.iter().filter(|r| r.label.is_none()).count(),
        14,
        "14 venue lines — a declared `[venues]` table mirrors ROSTER-COMPLETE: {:?}",
        fx.arming
    );

    let after = SUBJECT(&fx);
    assert_eq!(after.len(), 16, "…and 16 come out");
    assert_eq!(after.iter().filter(|(_, armed)| *armed).count(), 15, "15 rows arm");

    let hyperliquid_demo = fx.id_of("hyperliquid", "demo");
    let disarmed: Vec<i64> = after.iter().filter(|(_, armed)| !*armed).map(|(a, _)| a.id).collect();
    assert_eq!(
        disarmed,
        vec![hyperliquid_demo],
        "the one row that does NOT arm is hyperliquid's demo account — the venue is mounted at \
         live, so its demo credential set is what the operator's own mode did not name"
    );

    let how = moves(&fx, &after);
    let narrowed: Vec<i64> =
        how.iter().filter(|(_, m)| *m == Move::Narrowed).map(|(id, _)| *id).collect();
    assert_eq!(
        narrowed,
        vec![hyperliquid_demo],
        "exactly one row's effective behaviour moves, and it is the same row: {how:?}"
    );
    assert_eq!(
        how.iter().filter(|(_, m)| *m == Move::Unchanged).count(),
        15,
        "…and the other 15 come out IDENTICAL, which is the equality §5.2 step 5 expects: {how:?}"
    );

    assert!(objections(&fx, &after).is_empty(), "…and nothing widens");
}

/// **A kill proof for the row-set half.** A reshape that forgets an account row narrows it to
/// `paper` — safe by the inequality, and a silent loss of the row the operator armed. The gate
/// must refuse it on the row set alone.
#[test]
fn a_reshape_that_drops_an_account_row_is_refused() {
    let fx = Fixture::build(PROD2_SHAPE);
    let dropped = fx.id_of("binance", "live");
    let after: Vec<(Account, bool)> =
        SUBJECT(&fx).into_iter().filter(|(a, _)| a.id != dropped).collect();

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains("did not preserve the account rows")),
        "a fold that loses a row must be refused on the ROW SET, since the inequality reads it as \
         a narrowing: {found:?}"
    );
}

/// **…and the count alone is not enough.** A reshape that drops one row and duplicates another
/// has the right count and the wrong rows.
#[test]
fn a_reshape_that_duplicates_a_row_in_place_of_another_is_refused() {
    let fx = Fixture::build(PROD2_SHAPE);
    let dropped = fx.id_of("binance", "live");
    let mut after: Vec<(Account, bool)> =
        SUBJECT(&fx).into_iter().filter(|(a, _)| a.id != dropped).collect();
    let twin = after[0].clone();
    after.push(twin);
    assert_eq!(after.len(), fx.accounts.len(), "the COUNT is right, which is the point");

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains("did not preserve the account rows")),
        "the row set is compared as a multiset of ids, not as a count: {found:?}"
    );
}

/// **A kill proof for the inequality half, and the reason §5.2 step 5 was rewritten.**
///
/// The wording this spec was SIGNED with — *`armed = 1` where the old effective ceiling was above
/// `paper`* — composed with §3.2's `armed ? tier : paper` arms a `tier = live` account whose venue
/// line says `demo`: old effective `demo`, new effective `live`. A widening, in the one step
/// annotated as the one that must not widen. The gate catches it.
#[test]
fn the_struck_rule_from_the_signed_spec_is_caught_as_a_widening() {
    let fx = Fixture::build(A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE);
    let live_row = fx.id_of("bybit", "live");

    let found = objections(&fx, &STRUCK(&fx));
    assert!(
        found.iter().any(|f| f.contains(&format!("account {live_row} ")) && f.contains("WIDENS")),
        "the struck rule arms bybit's live account under a demo line and the gate must say so: \
         {found:?}"
    );

    // …and the rule that replaced it does not.
    assert!(
        objections(&fx, &RULED(&fx)).is_empty(),
        "the RULED rule arms only where the operator's own mode named the account's tier"
    );
}

/// **A FINDING against the ruled rule, executable rather than argued.**
///
/// `VenuePolicy::account` resolves a LABELLED account with no `[accounts]` line of its own to
/// `paper` — *"the `bybit = "live"` line was written when bybit had one account; reading it as
/// consent for an account that did not exist when it was written is precisely the silent
/// escalation the ceiling exists to prevent"*. The ruled fold reads the VENUE row only, so it arms
/// that account: old effective `paper`, new effective `live`.
///
/// ⚠ **This is unreachable on either live box today** — every account a migration writes carries
/// `label: None` (`vike_secrets::Account`'s own doc), and neither store holds a `__LABEL`
/// credential key. It is reachable the moment one is filed, which is an operator action needing no
/// code change, so it is a live hazard for stage 3 rather than a curiosity.
///
/// ⚠ **And it is exactly what `VenuePolicy::get` cannot see**, which is why this gate does not use
/// it: the same rows, judged by the venue answer, produce NO objection at all.
#[test]
fn the_ruled_fold_widens_a_labelled_account_with_no_line_of_its_own() {
    let fx = Fixture::build(A_LABELLED_ACCOUNT_WITH_NO_LINE);
    let labelled: Vec<&Account> =
        fx.accounts.iter().filter(|a| a.label.as_deref() == Some("ALT")).collect();
    assert_eq!(labelled.len(), 1, "the fixture mints one LABELLED account: {:?}", fx.accounts);
    let alt = labelled[0].id;

    let found = objections(&fx, &RULED(&fx));
    assert!(
        found.iter().any(|f| f.contains(&format!("account {alt} ")) && f.contains("WIDENS")),
        "the ruled fold arms a labelled account the venue line never consented to, and the gate \
         must say so: {found:?}"
    );

    // The blindness the brief names, measured rather than asserted: judged by the VENUE answer,
    // the same rows raise nothing.
    let blind: Vec<String> = RULED(&fx)
        .iter()
        .filter(|(a, armed)| {
            let old = fx.policy.get(&a.venue).cap(mode(&a.tier));
            new_effective(a, *armed) > old
        })
        .map(|(a, _)| a.id.to_string())
        .collect();
    assert!(
        blind.is_empty(),
        "`VenuePolicy::get` makes a labelled widening INVISIBLE — that is the whole reason this \
         gate resolves through `VenuePolicy::account`: {blind:?}"
    );
}

/// **[`SUBJECT`] and §5.2 step 5's venue-row wording agree on the the CI box shape, row for row.**
///
/// That equality is what makes the shipped fold a safer SPELLING of the same migration rather
/// than a different migration: on a store whose accounts are all unlabelled — which is what both
/// live boxes hold — the two arms [`RULED`] omits are unreachable, so the two folds are
/// byte-identical.
///
/// ⚠ **This got STRONGER when stage 3 landed and [`SUBJECT`] stopped being a model.** It used to
/// compare two functions written in this file; it now compares the spec's wording against the
/// BITS `crates/vike-secrets/src/settings.rs`'s `fold_arming_into_accounts` actually wrote into a
/// real store.
///
/// ⚠ **This is INFERENCE from shape equivalence, not a measurement of either real store.** The
/// fixture is constructed to §5.2 step 5's description of the CI box (see [`PROD2_SHAPE`]); nobody has
/// run this fold against the actual database. What it licenses is *"the subject changes nothing
/// the ruled rule would not have changed, on a store of this shape"* — not *"the CI box is
/// unaffected"*. Confirming the latter needs the fold run on the box, which is stage 3's rollout
/// and not this gate's claim to make.
#[test]
fn the_subject_and_the_specs_venue_row_wording_agree_on_the_prod2_shape() {
    let fx = Fixture::build(PROD2_SHAPE);
    let ruled: Vec<(i64, bool)> = RULED(&fx).iter().map(|(a, b)| (a.id, *b)).collect();
    let subject: Vec<(i64, bool)> = SUBJECT(&fx).iter().map(|(a, b)| (a.id, *b)).collect();
    assert_eq!(
        ruled, subject,
        "the two folds must agree on every store whose accounts are unlabelled"
    );
}

/// **The tombstone's replacement.** ⚠ The test that stood here —
/// `the_shipped_ddl_has_no_armed_column_yet` — was DELETED when stage 3 landed, which is what its
/// own failure message instructed. This is the assertion in the OTHER direction, and it is not
/// ceremony: [`SUBJECT`] now reads `Account::armed`, and a `bool` field reads `false` just as
/// happily when the column has been dropped, renamed or lost in a rebuild. Every inequality in
/// this file would stay green over sixteen `false` bits — a NARROWING — so without this the gate
/// could go blind to the column's disappearance while reporting success.
#[test]
fn the_shipped_ddl_declares_the_armed_column() {
    let account_table = vike_secrets::DDL
        .split("CREATE TABLE IF NOT EXISTS account (")
        .nth(1)
        .and_then(|rest| rest.split(") STRICT;").next())
        .expect("the shipped DDL declares an `account` table");
    assert!(
        account_table.lines().any(|line| line.trim_start().starts_with("armed ")),
        "`account.armed` is GONE from the shipped DDL. This gate reads that column through \
         `vike_secrets::Account::armed` and would report every row as `paper` — a narrowing every \
         other assertion here tolerates. Re-point the gate before removing the column: {account_table}"
    );
    assert!(
        account_table.contains("CHECK (armed IN (0, 1))"),
        "…and the column keeps its `CHECK`: the fold writes 0/1, and `STRICT` types the column \
         INTEGER without constraining the value: {account_table}"
    );
}

/// **…and the column is actually WRITTEN, which the DDL alone cannot say.**
///
/// `armed INTEGER NOT NULL DEFAULT 0` means a store where the fold never ran answers `false` for
/// every row — indistinguishable, to every inequality in this file, from a store the operator
/// armed nothing on. This is the positive check: on the the CI box shape, some row comes back `true`,
/// so the write path was genuinely exercised rather than defaulted through.
#[test]
fn the_armed_column_is_written_by_the_store_and_not_merely_defaulted() {
    let fx = Fixture::build(PROD2_SHAPE);
    assert!(
        fx.accounts.iter().any(|a| a.armed),
        "every account came back DISARMED. Either `write_settings` no longer folds the arming \
         rows onto the account rows, or the fold ran before they landed — both read as a safe \
         narrowing to this gate's inequality and neither is what stage 3 shipped: {:?}",
        fx.accounts
    );
}

/// A guard on the fixture builder itself: a store that minted no accounts, or whose arming rows
/// never landed, would make every assertion above vacuous.
#[test]
fn the_fixture_builder_produces_a_real_store_with_both_tables_filled() {
    let fx = Fixture::build(PROD2_SHAPE);
    assert!(!fx.accounts.is_empty(), "the migration minted account rows");
    assert!(!fx.arming.is_empty(), "…and the mirror wrote arming rows");
    assert!(
        fx.policy.is_declared(),
        "…and the policy the gate compares against was DECLARED by those rows, not defaulted — \
         an undeclared policy reads every venue `paper` and would make every comparison pass"
    );
    for (venue, mode, _keys) in PROD2_SHAPE {
        assert_eq!(fx.policy.get(venue).as_str(), *mode, "the store's rows resolve {venue}'s line");
    }

    // …and the table is ROSTER-COMPLETE, which is what makes "14 venue lines" a fact about this
    // fixture rather than an accident. The mirror writes one arming row per roster venue whether
    // this table names it or not, so a roster that outgrew the table would leave venue lines with
    // no account under them. `just new-venue` reaches the table through the marker in
    // `PROD2_SHAPE`; this is the assertion that notices when it has not been run.
    let named: BTreeSet<&str> = PROD2_SHAPE.iter().map(|(venue, _, _)| *venue).collect();
    let unnamed: Vec<&&str> = vike_model::VENUES.iter().filter(|v| !named.contains(*v)).collect();
    assert!(
        unnamed.is_empty(),
        "`PROD2_SHAPE` must name every roster venue and does not: {unnamed:?}. Run \
         `just new-venue <name>`, fill in the scaffolded row, and move the three counts in \
         `the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing` with it."
    );
    let path: &Path = fx._dir.path();
    assert!(path.join("db").join("vike.db").is_file(), "a real database file is on disk");
}
