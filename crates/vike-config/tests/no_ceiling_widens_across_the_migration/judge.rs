//! The rule: the old and new effective ceilings, the candidate reshapes, and the objections that judge them.

use vike_config::{VenueMode, VenuePolicy};
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::{Account, ArmingRow};

use super::fixtures::Fixture;

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
pub(super) fn mode(word: &str) -> VenueMode {
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
pub(super) fn old_effective(policy: &VenuePolicy, account: &Account) -> VenueMode {
    policy.account(&account.venue, &label_of(account)).cap(mode(&account.tier))
}

/// **The NEW effective behaviour of one account row** — §3.2's `armed ? tier : paper`, and the
/// whole of it.
pub(super) fn new_effective(account: &Account, armed: bool) -> VenueMode {
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
pub(super) const RULED: Reshape = ruled;

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
pub(super) const STRUCK: Reshape = struck;

/// **THE SUBJECT — the SHIPPED fold, read back out of the store it wrote.**
///
/// ⚠ **This was a MODEL until stage 3 landed** — `fx.policy.account(venue, label) == tier`,
/// computed here in `vike-config` where `VenuePolicy::account` is nameable. It is now the
/// `account.armed` column that `crates/vike-secrets/src/settings/arming.rs`'s
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
pub(super) const SUBJECT: Reshape = subject;

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
pub(super) const UNCAPPED: Reshape = uncapped;

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
pub(super) fn objections(fx: &Fixture, after: &[(Account, bool)]) -> Vec<String> {
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
pub(super) enum Move {
    /// The row's effective behaviour is byte-for-byte what it was.
    Unchanged,
    /// The row comes out LOWER than it went in. Safe, and the thing the inequality tolerates.
    Narrowed,
    /// The row comes out HIGHER. [`objections`] is what refuses this; the variant exists so the
    /// equality pin cannot silently read a widening as a narrowing.
    Widened,
}

pub(super) fn moves(fx: &Fixture, after: &[(Account, bool)]) -> Vec<(i64, Move)> {
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
