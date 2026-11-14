//! [`ArmingAccount`] — **one ACCOUNT's arming row**: the tier its `account` row states, whether the
//! mount would RUN it at that tier, and, when it would not, WHY.
//!
//! An account runs at the tier its active row states or it does not run at all
//! (`docs/decisions/0156-an-account-runs-at-its-tier-or-not-at-all-and-accounttier-is-the-one-tier-type.md`):
//! a `paper` row runs the paper simulator, a `demo` or `live` row runs a venue client armed at
//! exactly that tier, and every other outcome is NOT RUNNING with its [`ArmingBlock`] as the reason —
//! never a paper engine under the account's name.
//!
//! One row per account the mount enumerates: the DEFAULT account of every roster venue, plus each
//! labelled account the credential store or the `account` table names. A row carries the
//! `account.id`s that stated its tier ([`ArmingAccount::account_ids`]), which are what every remedy's
//! `--id` names.
//!
//! # Why the type lives here and the ANSWER does not
//!
//! The question "what will this account actually do when this binary mounts it" is answerable only
//! by `vike-tradehub-core`, which owns the per-venue arms, reads the `account` table's answer and calls every
//! bridge's own config loader (`vike_tradehub_core::venue_arming_under` is the producer, and it is the SAME
//! function `would_go_live_under` — the pre-connect live-intent probe — is derived from, so a row
//! cannot disagree with the mount about the account it is describing). The ROW is a plain data type
//! here, one hop above `vike_model::VENUES` and beside [`AccountTier`], so a consumer that links no
//! mount (`vike-tradehub`'s `venues` report reads it off the mount's answer) can still name it.
//!
//! ⚠ **Not to be confused with `crate::arming`**, the module one file over. That one asks whether
//! the whole BOX intends to trade live (from the settings rows), for `vike-cli config check`. This
//! one describes what a venue's mount will do.

use vike_model::accounts::account_keys::{AccountLabel, AccountRef};

use vike_model::{AccountTier, ArmingBlock};

/// **One ACCOUNT's row.** `venue` is a `vike_model::VENUES` id (a `&'static str` from the roster
/// itself), so a row can never name a venue that does not exist, and [`Self::label`] says WHICH
/// account of it.
///
/// ⚠ **It is not `Copy`** — [`AccountLabel::Named`] owns a `String` and [`Self::account_ids`] is a
/// `Vec`. Every construction site therefore clones rather than copies; the type is built once per
/// account at a mount and once per account per report, so nothing on any hot path notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmingAccount {
    /// The roster id.
    pub venue: &'static str,
    /// **Which account of it.** [`AccountLabel::Default`] is the unlabelled account — the one a
    /// single-account box has.
    pub label: AccountLabel,
    /// The tier the account table states for this account; `None` when no active row states exactly
    /// one (no row, none active, two active tiers, an unreadable table) — [`Self::block`] says which.
    pub tier: Option<AccountTier>,
    /// What the mount would ACTUALLY run this account at, with this binary's features and this
    /// box's credential store: `Some(tier)` when it runs (the paper simulator for a `paper` row, a
    /// venue client bound at `demo`/`live`), `None` when it does NOT RUN — never a paper engine in
    /// its place (decision 0156).
    ///
    /// ⚠ The projection is deliberately NOT clamped to [`Self::tier`]: a bridge whose `resolve`
    /// answers a tier ABOVE the account's shows that wrong answer here, and the mount's tier
    /// interlock refuses it ([`Self::runs`] is false for such a row).
    pub effective: Option<AccountTier>,
    /// Why the account does not run, or [`ArmingBlock::None`] / [`ArmingBlock::PaperTier`] /
    /// [`ArmingBlock::OrdersHeld`] when it does.
    pub block: ArmingBlock,
    /// The ACTIVE `account.id`s of (venue, label) that stated `tier`; empty when none (the remedy's
    /// `--id`).
    pub account_ids: Vec<i64>,
}

impl ArmingAccount {
    /// How this row names its subject in a sentence: the bare venue for the default account,
    /// `venue account LABEL` for a labelled one. Every [`Self::why`] arm goes through it, so a row
    /// can never describe a second account as though it were the venue.
    #[must_use]
    pub fn subject(&self) -> String {
        match self.label.text() {
            None => self.venue.to_string(),
            Some(label) => format!("{} account `{label}`", self.venue),
        }
    }

    /// Is this the unlabelled account — the one a single-account box has?
    #[must_use]
    pub fn is_default_account(&self) -> bool {
        self.label.is_default()
    }

    /// **This account's ROUTING key** — `vike_execution::Engine::route_key`, and therefore the
    /// `LIVE-<route_key>.lock` filename `vike_boot::live_lock::LiveLock::acquire` takes and the name
    /// this account appears under in `vike_tradehub_core::make_engine_accounts`' `live_venues` record.
    ///
    /// ⚠ Rendered by [`AccountRef::route_key`] and NOWHERE else. Three separate places need this
    /// string — the pre-mount armed set a lock is claimed from, the post-mount record it is checked
    /// against, and the journal row — and a second spelling of it would be a lock held under a name
    /// nothing else uses. For the DEFAULT account it is the bare venue id, which is what
    /// `Engine::new` already seeds the field with, so a single-account box's sentinel
    /// filenames do not move. It carries no tier: an account runs at its row's one tier.
    #[must_use]
    pub fn route_key(&self) -> String {
        AccountRef { venue: self.venue, label: self.label.clone() }.route_key()
    }

    /// **Whether this account RUNS** — at exactly the tier its row states (decision 0156): a
    /// `paper` row on the paper simulator, a `demo`/`live` row on a venue client bound at that tier.
    /// `false` is NOT RUNNING, with [`Self::block`] as the reason, and also for a bridge whose
    /// projection answered a tier other than the account's (the mount refuses it).
    #[must_use]
    pub fn runs(&self) -> bool {
        self.effective.is_some() && self.effective == self.tier && self.block.runs()
    }

    /// The tier word a sentence names: the tier's own spelling, or `none` when no row states one.
    fn tier_word(tier: Option<AccountTier>) -> &'static str {
        tier.map_or("none", AccountTier::as_str)
    }

    /// The `--id` argument a remedy names: this row's own ids when it has them, else a placeholder
    /// pointing at the listing that prints them.
    fn id_arg(&self) -> String {
        match self.account_ids.as_slice() {
            [] => "--id <N>".to_string(),
            [one] => format!("--id {one}"),
            many => format!(
                "--id <one of {}>",
                many.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// One operator-facing sentence for the Effective cell's detail line: what the row shows and
    /// what to do about it. A remedy names the account verb and the row's `--id`
    /// (`vike-cli secrets accounts` lists every row), so the answer is actionable without a second
    /// lookup.
    #[must_use]
    pub fn why(&self) -> String {
        let v = self.venue;
        let subject = self.subject();
        let id = self.id_arg();
        match self.block {
            ArmingBlock::None => format!(
                "{subject} runs at {} — nothing is holding it back",
                Self::tier_word(self.effective)
            ),
            ArmingBlock::PaperTier => format!(
                "every active account row of {subject} is tier `paper`, so it runs the paper \
                 simulator, loads no credential and opens no socket — `vike-cli secrets account \
                 set-tier {id} --tier demo` (or `live`) arms it at the next restart"
            ),
            ArmingBlock::AccountInactive => format!(
                "{subject} has account rows and none is active, so it does NOT RUN — `vike-cli \
                 secrets account activate {id}` runs it at its row's tier at the next restart \
                 (`vike-cli secrets accounts` lists the rows)"
            ),
            ArmingBlock::NoAccountRow => format!(
                "no account row names {subject}, so it does NOT RUN — `vike-cli secrets account \
                 add --venue {v} --tier <paper|demo|live>` (with `--label` for a labelled account) \
                 creates one, active, and it runs at the next restart; `vike-cli secrets accounts` \
                 lists the rows this box has"
            ),
            // A VENUE-scoped tier (decision 0140) is judged over the rows of every label.
            ArmingBlock::TierConflict
                if vike_venues::venue_tier_scope(v) == vike_venues::TierScope::Venue =>
            {
                format!(
                    "{v} rides ONE session per box, and its ACTIVE account rows, of any label, sit \
                     at different tiers (demo and live), so {subject} does NOT RUN — nor does any \
                     {v} account — rather than guessing which one you meant; deactivate the one \
                     you did not mean with `vike-cli secrets account deactivate {id}` (`vike-cli \
                     secrets accounts` lists the rows)"
                )
            }
            ArmingBlock::TierConflict => format!(
                "{subject} has two ACTIVE account rows at different tiers (demo and live), so it \
                 does NOT RUN rather than guessing which one you meant — deactivate the one you did \
                 not mean with `vike-cli secrets account deactivate {id}` (`vike-cli secrets \
                 accounts` lists the rows)"
            ),
            ArmingBlock::NoAccountSymbol => format!(
                "{subject} states no symbol for its engine to be mounted on (its row's \
                 `account.symbol` is unset; a lane twin's is its parent row's `lane_twin_symbol` \
                 fact), so it does NOT RUN: no symbol is ever chosen for an account in code. \
                 `vike-cli secrets account set-symbol {id} <SYMBOL>` states one (`vike-cli secrets \
                 account set-fact {id} lane_twin_symbol <SYMBOL>` on the parent, for a lane twin), \
                 and a running tradehub starts the account once the row is written"
            ),
            ArmingBlock::FeatureAbsent => format!(
                "this gatewayhub build has no {v} arm (its cargo feature is off), so {subject} does \
                 not run here — the account row is still valid, and a gatewayhub build that HAS \
                 the feature will honour it"
            ),
            ArmingBlock::NoLiveArm => format!(
                "{v} has no exec arm in any build — a `demo` or `live` account of it can never run \
                 (`vike-cli secrets account set-tier {id} --tier paper` runs the paper simulator)"
            ),
            ArmingBlock::NoCredentials => format!(
                "no usable {v} credentials in the store for {subject} — absent credentials are \
                 still the live gate, so it does not run"
            ),
            ArmingBlock::ExecFlagUnset => format!(
                "{v} order placement needs flags.poly_exec as well as an active account row; it is \
                 off, so {subject} does not run — vike-cli config set flags.poly_exec true"
            ),
            ArmingBlock::LiveSwitchOff => format!(
                "flags.tradehub_live is off — a STOP for every demo and live account of this \
                 daemon — so {subject} (tier {}) does not run; a paper account still does. \
                 vike-cli config set flags.tradehub_live true runs it at the next restart",
                Self::tier_word(self.tier)
            ),
            ArmingBlock::SdkAbsent => format!(
                "{v}'s ForexConnect shim did not load on this box, so its live mount is refused \
                 and {subject} does not run — a live client there would reject every order"
            ),
            ArmingBlock::DemoOnlyArm => format!(
                "{subject} can reach only {v}'s demo endpoint: the mount arm has no live tier, or \
                 its live tier did not arm for this account (the daemon's log names the rule that \
                 refused it), so only a `demo` account arms it and a `live` one does not run \
                 (`vike-cli secrets account set-tier {id} --tier demo`)"
            ),
            ArmingBlock::LiveOnlyArm => format!(
                "{v} runs no testnet, so its arm refuses anything below `live` and a `demo` \
                 account of it does not run (`vike-cli secrets account set-tier {id} --tier live`)"
            ),
            ArmingBlock::LiveCredentialsAbsent => format!(
                "{subject}'s account tier is `live`, which means MAINNET, but the store holds no \
                 LIVE-tier {v} credentials, so it does NOT RUN — a live account never falls to the \
                 demo network. Store the LIVE keys, or `vike-cli secrets account set-tier {id} \
                 --tier demo` to trade the demo account"
            ),
            ArmingBlock::LiveTierNotWired => format!(
                "{subject} has a LIVE-tier credential set stored, but {v}'s mount arm does not \
                 select it — the arm mounts only its demo tier, or its live tier refused this \
                 account under one of its own rules (the daemon's log names which) — so {subject} \
                 does NOT RUN whatever its tier says. Nothing was signed and no order can reach the \
                 live account. To trade the demo account, store its demo-tier keys — and where {v} \
                 refuses to choose a tier while a live set is stored (oanda), remove the live-tier \
                 keys too. `vike-cli secrets list` names what the store holds"
            ),
            ArmingBlock::NoAccountSupport => format!(
                "{subject} cannot be armed and does not run: {v}'s mount reads its credentials \
                 through a loader that \
                 knows no account label, so a second account of it would trade the FIRST \
                 account's keys. Run it in its own project folder (its own settings directory \
                 and credential store) instead"
            ),
            ArmingBlock::AccountNotInStore => format!(
                "{subject} cannot be armed and does not run: the settings database does not say \
                 WHICH {v} account it \
                 is (or could not be read), and the mount refuses rather than picking one — on {v} \
                 the accounts are separate brokers. `vike-cli secrets accounts` prints the rows \
                 this box has and `vike-cli secrets set-book` writes the identifier the venue \
                 itself gave one; a box with no settings database needs `vike-cli secrets init` \
                 first"
            ),
            ArmingBlock::SidecarHeldElsewhere => format!(
                "{subject} cannot be armed and does not run: this process opens ONE {v} broker \
                 session and another \
                 {v} account holds it. Nothing about this account's credentials, its tier or its \
                 row is wrong — it lost a process-wide resource: an active labelled {v} account \
                 takes the session, and with none the DEFAULT account keeps it (`vike-cli secrets \
                 account deactivate --id <N>` on the other one hands it over). To trade both at \
                 once, give the second account its own project folder (its own settings directory \
                 and credential store) and run a second process there"
            ),
            ArmingBlock::PreflightFailed => format!(
                "the startup preflight hard-FAILED for {v} (the venue answered and refused the \
                 credentials, or its clock is measurably out of band), so {subject} does NOT RUN \
                 this session — the daemon's log names the failing check; fix it and restart, or \
                 `vike-cli config set flags.preflight_skip true` to mount without any preflight"
            ),
            ArmingBlock::AdapterFailed => format!(
                "{subject} armed, and {v}'s mount did not come up at its tier (a failed connect, a \
                 key the venue declined, or a bridge that bound another tier), so it does NOT RUN \
                 this session — the daemon's log names the cause; it is retried at the next \
                 restart"
            ),
            ArmingBlock::GatewayUnavailable => format!(
                "the gatewayhub, the daemon that owns every venue connection, did not answer this \
                 tradehub within `config.gatewayhub_connect_wait_ms`, so {subject} (tier {}) does \
                 NOT RUN this session — {v} has no client of its own to fall back on, and a \
                 `paper` account is not started in its place. Start the gatewayhub \
                 (`vike-backend gatewayhub`, or its unit): the tradehub arms the account itself \
                 once the gatewayhub answers, with one graceful restart",
                Self::tier_word(self.tier)
            ),
            ArmingBlock::OrdersHeld => format!(
                "{subject} mounts {} with every ORDER HELD: {v}'s arm connects to the account, \
                 serves its data and reconciles it, and REFUSES every order — each one is \
                 rejected, loudly, and none reaches the venue — because its order path at this \
                 tier is not proven yet. No setting lifts the hold; the daemon's log names why it \
                 holds",
                Self::tier_word(self.effective)
            ),
            ArmingBlock::FeedsUnavailable => format!(
                "this tradehub takes its prices from the datahub's strategy lane and that lane did \
                 not answer at start (the datahub, or the gatewayhub behind it, is away, or the \
                 box holds no strategy-lane key), so {subject} (tier {}) does NOT RUN this session \
                 - an engine with no prices is not started, and a `paper` engine is not started in \
                 this account's place. Nothing about the account is wrong: `vike-backend venues` asks the gatewayhub \
                 directly and may still show {v} as would-arm. The tradehub's log carries the cause \
                 (`PRICE FEEDS UNAVAILABLE`); the tradehub arms the account itself once the price \
                 path answers, with one graceful restart",
                Self::tier_word(self.tier)
            ),
            ArmingBlock::FxMarketClosed => format!(
                "the FX market is closed (Friday 22:00 to Sunday 21:00 UTC), so {subject} (tier {}) does NOT \
                 RUN until it opens: no {v} login is made and none is retried while it is closed, and its orders \
                 are refused with this reason. Nothing about the account is wrong; the tradehub starts it itself \
                 at the open, once",
                Self::tier_word(self.tier)
            ),
        }
    }
}

#[path = "venue_arming_tests.rs"]
#[cfg(test)]
mod venue_arming_tests;
