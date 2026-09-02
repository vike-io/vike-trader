//! **The shared-book rule**: two ACTIVE accounts of one venue that resolve to the SAME effective
//! trading book are reported — loudly, by name — and both are mounted anyway. A pure predicate and
//! its vocabulary; the identities it compares are computed one layer up, by
//! `vike_mount::book_identity`.
//!
//! # ⚠ THE SYMBOL IS NOT PART OF THIS RULE, AND THAT IS A CORRECTION
//!
//! This module used to state a different rule — *two active accounts of one venue may not be armed
//! on the same symbol* — and enforce it by capping the loser to `paper`. **That rule was wrong, and
//! the thing it refused is an ordinary spread**: long BTC on account A, short BTC on account B.
//! Two accounts are two wallets — hyperliquid SUB-ACCOUNTS have their own addresses — so they hold
//! entirely separate positions, the venue nets nothing between them, and no local book sizes
//! against the other's. There was nothing to refuse. The account is the unit; WHAT it trades is the
//! strategy's business, and a mount now names the account it trades on
//! (`vike_run::MountSpec::account`, `vike_core::StrategyMount::account`).
//!
//! # The hazard that IS real, stated concretely
//!
//! One venue BOOK is one position ledger at the venue. Two of this process's engines pointed at the
//! same book — an agent/API key signing for a master whose own key is also configured, or one
//! credential set pasted under two labels — is the shape that breaks: each engine reads "I hold 1"
//! while the venue holds 2. Every position-side mechanism then reads the wrong number, and
//! reconcile's auto-applied `PositionDrift` — the one no-local-origin divergence `hybrid` folds
//! without an operator — rewrites each engine's local size onto a venue total that includes the
//! other's.
//!
//! # ⚠ …and it is REPORTED, never refused
//!
//! The operator writes every credential by hand, under an explicit label; writing a second set
//! means meaning a second account. `docs/decisions/0013-degrade-vs-refuse.md` is this workspace's
//! standing verdict on that choice, and `vike_mount::venue_arming_migration` is the precedent —
//! it names what it found at startup and starts anyway. So `vike_mount::make_engine_accounts`
//! emits one `tracing::warn!` per pair, naming the venue, BOTH labels and the shared book, and
//! mounts both engines. A warning that names the two labels is what lets an operator recognise
//! their own paste error in seconds; a refusal would strand them with a venue silently on paper.
//!
//! **Where the book cannot be determined offline, nothing is said.** `vike_mount::book_identity`
//! answers per venue, and "cannot determine offline" is a legitimate answer for a key/secret venue
//! whose store names no account — an unprovable suspicion is not a finding, so such an account
//! contributes no [`ArmedBook`] at all and no pair can form.
//!
//! # What the field does, and what we are declining to copy
//!
//! **Freqtrade** — the most widely deployed of the three surveyed — refuses multi-account
//! in-process entirely (one bot, one config, one systemd unit per account) and answers the
//! shared-book hazard with a documentation WARNING: their `tradable_balance_ratio` docs tell the
//! operator not to use it when two bots share one account. So the hazard is acknowledged in the
//! field and left to the reader. **NautilusTrader** keys clients on an arbitrary operator string
//! and prescribes nothing about what those clients trade. **Hummingbot** names accounts but pairs a
//! strategy with one of them and stops there.
//!
//! A sentence in a document is the state of the art, and it is what this module declines to copy.
//! The rule is a FUNCTION evaluated at every mount, so the warning names THIS box's two labels and
//! THIS box's shared address rather than describing a hazard in general.

use std::collections::BTreeSet;

use vike_model::account_keys::AccountLabel;

use crate::VenueMode;

/// **One armed account and the venue book it resolves to** — the unit the rule is stated over.
///
/// `venue` is a `vike_model::VENUES` id, `label` says WHICH account of it, `book` is the effective
/// trading identity `vike_mount::book_identity::effective_book` resolved for it, and `mode` is the
/// account's RESOLVED tier (`VenuePolicy::account`'s answer capped by what the arm can reach), not
/// its stated ceiling: an account capped down to paper touches no venue book at all.
///
/// ⚠ **An account whose book could not be determined offline has NO `ArmedBook`.** It is not
/// represented with an empty or placeholder `book` — a placeholder would compare equal to every
/// other undeterminable account of the venue and manufacture a pair out of two unknowns, which is
/// precisely the false report the "warn nothing where you cannot tell" rule exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArmedBook {
    /// The roster id.
    pub venue: &'static str,
    /// Which account of it.
    pub label: AccountLabel,
    /// **The effective trading book**, compared EXACTLY. `vike_mount::book_identity` normalizes
    /// (trim + lowercase) before it builds one of these, so the comparison here is a plain string
    /// equality over already-canonical text: an EVM address and an account id are both
    /// case-insensitive identifiers, and normalizing at the producer keeps this rule from carrying
    /// a second, weaker copy of that knowledge.
    pub book: String,
    /// The tier this account resolved to.
    pub mode: VenueMode,
}

impl ArmedBook {
    /// **Is this account trading a venue-side book at all?** `paper` is not: the paper exchange
    /// fills locally, so two paper accounts touch no venue book and share nothing. `demo` IS —
    /// a demo account is a real book with real rejections, and two engines double-folding it is the
    /// same defect with play money.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.mode > VenueMode::Paper
    }
}

/// **Two active accounts of one venue resolving to ONE book.** Labels are stored in sorted order,
/// so one shared book produces one record whichever order the accounts arrived in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SharedBook {
    /// The venue both accounts belong to.
    pub venue: &'static str,
    /// The effective trading book both resolve to — an address or an account id, never a secret
    /// (`vike_mount::book_identity` only ever produces identifiers that are safe to log; see its
    /// `BookIdentity::Undeterminable` for the family it deliberately declines to fingerprint).
    pub book: String,
    /// The lower-sorting of the two accounts.
    pub first: AccountLabel,
    /// The higher-sorting of the two.
    pub second: AccountLabel,
}

impl SharedBook {
    /// One operator-facing sentence: what is shared, and what that does. Names both accounts and
    /// the book, because "bybit has a problem" is not something anybody can act on, and because
    /// recognising one's own paste error takes seconds once the two labels are on the line.
    #[must_use]
    pub fn why(&self) -> String {
        format!(
            "{} accounts `{}` and `{}` both resolve to the SAME venue book `{}` — two engines over \
             one position ledger, so each reads its own size while the venue holds the sum, and \
             reconcile's auto-applied `PositionDrift` rewrites each onto a total that includes the \
             other's. If that is not what you meant, one of the two credential sets names the \
             wrong account",
            self.venue, self.first, self.second, self.book
        )
    }
}

/// **THE rule.** Every pair of DISTINCT, ACTIVE accounts of the SAME venue resolving to the SAME
/// book, deduplicated and in a deterministic order.
///
/// Four conditions, each load-bearing:
///
/// * **same venue** — two accounts at two venues share no book, whatever their identifiers are
///   called. (An EVM address genuinely CAN be the same string on hyperliquid and aster; they are
///   still two exchanges holding two ledgers.)
/// * **same book**, compared exactly (see [`ArmedBook::book`]).
/// * **distinct accounts** — the same account listed twice is a duplicate row, not two engines. It
///   is a different defect with a different fix, and reporting it here would send the operator
///   looking for a second account that does not exist.
/// * **both active** ([`ArmedBook::is_active`]) — a paper account shares nothing.
///
/// The empty vector is the quiet answer, so a caller can iterate and warn per pair. There is no
/// yes/no twin on purpose: nothing in this workspace REFUSES on this predicate any more, and a
/// boolean is the shape a refusal wants.
#[must_use]
pub fn shared_books(books: &[ArmedBook]) -> Vec<SharedBook> {
    let mut out: BTreeSet<SharedBook> = BTreeSet::new();
    for (i, a) in books.iter().enumerate() {
        if !a.is_active() {
            continue;
        }
        for b in books.iter().skip(i + 1) {
            if !b.is_active() || a.venue != b.venue || a.book != b.book || a.label == b.label {
                continue;
            }
            let (first, second) =
                if a.label <= b.label { (&a.label, &b.label) } else { (&b.label, &a.label) };
            out.insert(SharedBook {
                venue: a.venue,
                book: a.book.clone(),
                first: first.clone(),
                second: second.clone(),
            });
        }
    }
    out.into_iter().collect()
}
