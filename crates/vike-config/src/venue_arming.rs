//! [`VenueArming`] — **one ACCOUNT's arming row**: the tier its `account` row states, whether the
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
//! `account.id`s that stated its tier ([`VenueArming::account_ids`]), which are what every remedy's
//! `--id` names.
//!
//! # Why the type lives here and the ANSWER does not
//!
//! The question "what will this account actually do when this binary mounts it" is answerable only
//! by `vike-mount`, which owns the per-venue arms, reads the `account` table's answer and calls every
//! bridge's own config loader (`vike_mount::venue_arming_under` is the producer, and it is the SAME
//! function `would_mount_live_under` — the pre-connect live-intent probe — is derived from, so a row
//! cannot disagree with the mount about the account it is describing). The ROW is a plain data type
//! here, one hop above `vike_model::VENUES` and beside [`AccountTier`], so a consumer that links no
//! mount (`vike-tradehub`'s `venues` report reads it off the mount's answer) can still name it.
//!
//! ⚠ **Not to be confused with `crate::arming`**, the module one file over. That one asks whether
//! the whole BOX intends to trade live (from the settings rows), for `vike-cli config check`. This
//! one describes what a venue's mount will do.

use vike_model::accounts::account_keys::{AccountLabel, AccountRef};

use vike_model::AccountTier;

/// **Why an account does not run at the tier its row states** — the "Effective" column's second
/// half, and the reason a NOT RUNNING account is shown with.
///
/// The complaint this vocabulary exists to answer is *"I set live and it does not run"*. Every
/// variant below is a distinct, observable cause with a distinct operator response, which is why
/// they are not collapsed into one `Blocked(String)`: a row that says [`Self::AccountInactive`]
/// sends the operator to `vike-cli secrets account activate`, one that says
/// [`Self::LiveCredentialsAbsent`] to the credential store's LIVE tier, and one that says
/// [`Self::FeatureAbsent`] to a different BINARY.
///
/// Three variants are not refusals: [`Self::None`] (a `demo`/`live` account runs at its tier),
/// [`Self::PaperTier`] (a `paper` account runs the paper simulator) and [`Self::OrdersHeld`] (it
/// runs at its tier and sends no order). Every other variant means the account does NOT RUN — no
/// engine, no paper book in its place ([`Self::runs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmingBlock {
    /// Nothing is refusing anything: the account runs at its own tier.
    None,
    /// Every ACTIVE `account` row of this account is tier `paper`, so it runs the PAPER SIMULATOR,
    /// built ABOVE the credential read — no key is loaded, no grid fetched, no socket opened.
    PaperTier,
    /// The `account` table holds rows for this account and NONE of them is active
    /// (`account.active = 0`) — the per-account off switch. It does not run; decided above the
    /// credential read.
    AccountInactive,
    /// No `account` row states a tier for this account — or no table was read at all — so it does
    /// not run. A labelled account whose label only a credential KEY carries lands here.
    NoAccountRow,
    /// **Two or more ACTIVE rows of this account name DIFFERENT non-paper tiers** (`demo` and
    /// `live`). One process mounts one engine per `(venue, label)` and its route key and live lock
    /// carry no tier, so the mount refuses to pick one and the account does NOT RUN, loudly; the
    /// operator deactivates the row they did not mean. For a VENUE-scoped tier
    /// (`vike_model::TierScope::Venue`: ibkr, decision 0140) the rows are those of the venue, OF
    /// ANY LABEL, and no account of it runs.
    TierConflict,
    /// This BINARY has no arm for the venue: its cargo feature (`ibkr`, `polymarket`, `fxcm`) is
    /// off, so the account does not run here. The `account` row may legally name the venue anyway —
    /// the settings database must be portable between the boxes of one deployment — so the row
    /// renders the disagreement instead of hiding or refusing it.
    FeatureAbsent,
    /// The mount has NO exec arm for this venue in any build, so a `demo` or `live` account of it
    /// can never run.
    ///
    /// ⚠ No roster venue answers this today; `vike_mount`'s `venue_arming_under` reaches it for a
    /// venue its registry carries no row for, i.e. for a venue id that is not on the roster at all.
    NoLiveArm,
    /// The arm exists and the account's tier permits it, but the credential store holds no usable
    /// key set for the tier the arm would reach — absent credentials are still the live gate.
    NoCredentials,
    /// Polymarket: `flags.poly_exec` is off — a STOP for every `demo`/`live` polymarket account
    /// (decision 0156 item 4), so its exec plane never arms and the account does not run.
    ExecFlagUnset,
    /// `flags.tradehub_live` is off — a STOP for every `demo`/`live` account of the daemon (decision
    /// 0156 item 4). A `paper` account still runs; the switch never turns an account into paper.
    LiveSwitchOff,
    /// FXCM: the ForexConnect shim did not load on this box (the SDK is opened at RUNTIME, not
    /// linked), so `vike_fxcm::sdk_available()` refuses the live mount whatever the store says — a
    /// client mounted there would log in to nothing and reject every order.
    ///
    /// One cell of the store is named BEFORE the shim: a stand-alone LIVE-tier login answers
    /// [`Self::LiveTierNotWired`] on a box without the shim too, because that is a fact about the
    /// store no shim can change and it is what the daemon's log says (fxcm's `resolve` and `mount`
    /// ask in one order: `crates/bridges/fxcm/src/mount.rs`'s `resolution_for`).
    SdkAbsent,
    /// This account can reach only the venue's demo/practice/sandbox endpoint — the arm has no live
    /// tier, or (an arm that has one, such as ibkr's) its live tier did not arm for this account —
    /// so only a `demo` account arms it. A `live` account over it does NOT RUN: a live account never
    /// trades demo.
    DemoOnlyArm,
    /// This venue's mount arm has no DEMO tier (polymarket runs no testnet), so a `demo` account
    /// over it does not run.
    LiveOnlyArm,
    /// The account's tier is `live` and the arm can reach it, but the store holds no LIVE-tier key
    /// set the arm would bind: the account does NOT RUN. A live account never falls to its venue's
    /// demo network (decision 0095: for a CEX venue or hyperliquid `live` means mainnet), so a
    /// bridge that would bind demo for it is refused here too.
    LiveCredentialsAbsent,
    /// **A LIVE-tier key set is stored for this account and the arm mounts only its demo tier**, so
    /// the account does not run whatever its tier says — the cause for a credential that is PRESENT
    /// and UNUSABLE, which the credential doctrine treats as an error rather than an absence.
    ///
    /// Distinct from [`Self::NoCredentials`] on purpose, because the two send an operator to
    /// different places: `NoCredentials` means *no key set for the tier this arm would reach* and
    /// the fix is to write keys; this one means *the keys are there and nothing will ever read
    /// them*, and writing more of them changes nothing — the fix is the demo tier's keys, or (for
    /// the venue that refuses to choose while a live set is stored) removing the live ones. Distinct
    /// from [`Self::LiveCredentialsAbsent`] too, which is the opposite fact: the account is `live`,
    /// the arm COULD use a live set, and none is stored.
    ///
    /// Produced by every roster arm that has no live arm and has a live-tier key vocabulary to find
    /// (`vike_bridge_core::venue_mount::PaperCause::LiveTierNotWired` is the bridge-side name), and
    /// by an arm that HAS a live tier whose own rules refused this account (ibkr: the account's tier
    /// below `live`, its `account` row not provably active, its gateway rows unloadable — the
    /// mount's `error!` names the rule). So "not wired" means "this account's live set is not
    /// what the arm mounts", never "the venue has no live arm".
    LiveTierNotWired,
    /// **This venue's mount arm cannot address a second account at all**, so a labelled one is
    /// refused outright rather than armed onto the default account's keys.
    ///
    /// An arm qualifies only when its loader reads THAT account's credentials — the fields filed
    /// against its own `account` row, handed to it as
    /// `vike_secrets::account_credentials::AccountCredentials` (decision 0156 item 6: a credential
    /// belongs to its account, and no loader composes a name). An arm that still read the DEFAULT
    /// account's credentials would build a SECOND live client on them: two
    /// engines, one venue account, precisely the accident
    /// `vike_exec::ExecutionEngine::route_key` and `vike_boot::live_lock` exist to prevent.
    ///
    /// ⚠ **Each bridge's `VenueDeclaration::addresses_accounts` is the authority for WHICH venues
    /// this variant still refuses, and no prose anywhere — this doc included — restates the
    /// list.** (docs/decisions/0096.) **No roster venue produces it** — dukascopy addresses its
    /// accounts through the settings database's `account` table
    /// (`crates/bridges/dukascopy/src/mount.rs`).
    ///
    /// ⚠ **That is not the same as unreachable, and the distinction is the reason this variant
    /// stays.** A venue scaffolded by `just new-venue` declares `addresses_accounts: false`, so the
    /// first refusal a NEW venue's second account meets is this one — as does a labelled account on
    /// a venue the mount's registry carries no row for. It is the block no venue produces today,
    /// not one nothing can produce.
    ///
    /// ⚠ A refusal rather than a gap filled in passing: making a bespoke loader account-aware is a
    /// change to that bridge's credential surface and belongs to a PR that can verify the venue.
    NoAccountSupport,
    /// **The settings database cannot say which account this is** — so the mount cannot address
    /// it, and refuses rather than picking one.
    ///
    /// Distinct from [`Self::NoCredentials`] on purpose, because the two send an operator to
    /// different places: `NoCredentials` means *no key set for the tier this arm would reach*, and
    /// the fix is to write keys; this one means *the keys may well be there and nothing says which
    /// ACCOUNT they are*, and the fix is in the settings database rather than in the credential
    /// store. It is produced by `dukascopy`, whose two demo accounts are two LEGAL ENTITIES
    /// distinguishable only by their `account` row — so guessing is routing an order to a broker
    /// nobody chose — and by the mount when the `account` table could not be read at all.
    ///
    /// ⚠ It also covers a STORE FAILURE — a database that exists and will not open — and the mount's
    /// own refusal line says which it was (`vike_dukascopy`'s `DukascopyRefusal::StoreUnreadable`
    /// against its `NoSuchAccount`). One block, two causes, because an operator's next act is the
    /// same for both: look at the store.
    AccountNotInStore,
    /// **Another ACCOUNT of this venue holds the one broker session this process may open**, so this
    /// one does not run — nothing about its credentials, its tier or its `account` row is wrong.
    ///
    /// Produced today by `dukascopy` alone, and it is the only block whose cause is another ROW:
    /// two concurrent JForex sidecars share one platform cache whose corruption makes EVERY login
    /// fail, so exactly one dukascopy account is armed per process. A LABELLED account whose own row
    /// arms takes the session (the first in label order), and with none the DEFAULT account keeps
    /// it. `vike_mount`'s `exclusive` module carries the rule, and its `dukascopy` module the
    /// evidence and the measurement that retires the limit.
    ///
    /// ⚠ **The DEFAULT account can carry this block**, which no other refusal in this enum can do
    /// to it: when a labelled dukascopy account arms, the default yields the session to it.
    SidecarHeldElsewhere,
    /// **The startup preflight hard-FAILED for this venue** (`vike_mount::preflight`'s
    /// `VenueDisposition::Stopped`: the venue answered and refused the credentials, or its clock is
    /// measurably out of band), so none of its `demo`/`live` accounts runs this session. A probe that
    /// merely did not answer WARNs and stops nothing.
    PreflightFailed,
    /// **The probe armed this account and the venue's mount did not come up at its tier**: a
    /// synchronous connect that failed (ctrader, ibkr), a key the venue declined (hyperliquid,
    /// polymarket), an exclusive resource another start already holds, or a bridge that bound a
    /// tier other than the account's (refused by the mount's tier interlock). Unlike every other
    /// block it is known only AFTER the network, so the projection cannot predict it; the daemon's
    /// log names which cause it was. The account does not run.
    MountFailed,
    /// **The account mounts AT its tier and every order is refused**: the arm connects to the
    /// account, serves its data and reconciles it, and sends NO order. Each submit is rejected,
    /// terminally and loudly, so no order vanishes and none reaches the venue.
    ///
    /// One of the three blocks under which the account RUNS ([`Self::runs`]): its
    /// [`VenueArming::effective`] is the account's own tier, and this word is the only thing on the
    /// row that says orders are held. The bridge-side name is
    /// `vike_bridge_core::venue_mount::HeldBelowLive::OrdersHeld`: an arm whose order path is not
    /// proven at that tier (ibkr's `cpapi` LIVE tier, which has no observed live fill path:
    /// `docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, stage A). The hold is
    /// code, not a setting: no row lifts it.
    OrdersHeld,
}

impl ArmingBlock {
    /// Every variant, so a renderer and a test enumerate the same set without either writing it
    /// down again. Declaration order, which is roughly "most structural cause first".
    pub const ALL: [ArmingBlock; 21] = [
        ArmingBlock::None,
        ArmingBlock::PaperTier,
        ArmingBlock::AccountInactive,
        ArmingBlock::NoAccountRow,
        ArmingBlock::TierConflict,
        ArmingBlock::FeatureAbsent,
        ArmingBlock::NoLiveArm,
        ArmingBlock::NoCredentials,
        ArmingBlock::ExecFlagUnset,
        ArmingBlock::LiveSwitchOff,
        ArmingBlock::SdkAbsent,
        ArmingBlock::DemoOnlyArm,
        ArmingBlock::LiveOnlyArm,
        ArmingBlock::LiveCredentialsAbsent,
        ArmingBlock::LiveTierNotWired,
        ArmingBlock::NoAccountSupport,
        ArmingBlock::AccountNotInStore,
        ArmingBlock::SidecarHeldElsewhere,
        ArmingBlock::PreflightFailed,
        ArmingBlock::MountFailed,
        ArmingBlock::OrdersHeld,
    ];

    /// A short, stable badge for the Effective cell — never a sentence. Empty for exactly
    /// [`Self::None`], so a rendered badge always means something is being refused. It is also the
    /// REASON WORD a not-running account carries to the core's refusal and onto the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ArmingBlock::None => "",
            ArmingBlock::PaperTier => "paper tier",
            ArmingBlock::AccountInactive => "account inactive",
            ArmingBlock::NoAccountRow => "no account row",
            ArmingBlock::TierConflict => "two active tiers",
            ArmingBlock::FeatureAbsent => "feature absent",
            ArmingBlock::NoLiveArm => "no live arm",
            ArmingBlock::NoCredentials => "no credentials",
            ArmingBlock::ExecFlagUnset => "flags.poly_exec off",
            ArmingBlock::LiveSwitchOff => "flags.tradehub_live off",
            ArmingBlock::SdkAbsent => "SDK not loaded",
            ArmingBlock::DemoOnlyArm => "demo-only arm",
            ArmingBlock::LiveOnlyArm => "live-only arm",
            ArmingBlock::LiveCredentialsAbsent => "no live credentials",
            ArmingBlock::LiveTierNotWired => "live tier not wired",
            ArmingBlock::NoAccountSupport => "no second-account support",
            ArmingBlock::AccountNotInStore => "account not in the store",
            ArmingBlock::SidecarHeldElsewhere => "another account holds the session",
            ArmingBlock::PreflightFailed => "preflight failed",
            ArmingBlock::MountFailed => "mount failed",
            ArmingBlock::OrdersHeld => "orders held",
        }
    }

    /// Whether this block is the ordinary "nothing is wrong" answer.
    #[must_use]
    pub fn is_clear(self) -> bool {
        self == ArmingBlock::None
    }

    /// **Whether an account under this block RUNS** — [`Self::None`] (at its `demo`/`live` tier),
    /// [`Self::PaperTier`] (the paper simulator) and [`Self::OrdersHeld`] (at its tier, sending no
    /// order). Every other block is NOT RUNNING, with this block as the reason (decision 0156).
    #[must_use]
    pub fn runs(self) -> bool {
        matches!(self, ArmingBlock::None | ArmingBlock::PaperTier | ArmingBlock::OrdersHeld)
    }
}

/// **One ACCOUNT's row.** `venue` is a `vike_model::VENUES` id (a `&'static str` from the roster
/// itself), so a row can never name a venue that does not exist, and [`Self::label`] says WHICH
/// account of it.
///
/// ⚠ **It is not `Copy`** — [`AccountLabel::Named`] owns a `String` and [`Self::account_ids`] is a
/// `Vec`. Every construction site therefore clones rather than copies; the type is built once per
/// account at a mount and once per account per report, so nothing on any hot path notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueArming {
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

impl VenueArming {
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

    /// **This account's ROUTING key** — `vike_exec::ExecutionEngine::route_key`, and therefore the
    /// `LIVE-<route_key>.lock` filename `vike_boot::live_lock::LiveLock::acquire` takes and the name
    /// this account appears under in `vike_mount::make_engine_accounts`' `live_venues` record.
    ///
    /// ⚠ Rendered by [`AccountRef::route_key`] and NOWHERE else. Three separate places need this
    /// string — the pre-mount armed set a lock is claimed from, the post-mount record it is checked
    /// against, and the journal row — and a second spelling of it would be a lock held under a name
    /// nothing else uses. For the DEFAULT account it is the bare venue id, which is what
    /// `ExecutionEngine::new` already seeds the field with, so a single-account box's sentinel
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
                if vike_model::venue_tier_scope(v) == vike_model::TierScope::Venue =>
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
            ArmingBlock::FeatureAbsent => format!(
                "this build has no {v} arm (its cargo feature is off), so {subject} does not run \
                 here — the account row is still valid, and a build that HAS the feature will \
                 honour it"
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
            ArmingBlock::MountFailed => format!(
                "{subject} armed, and {v}'s mount did not come up at its tier (a failed connect, a \
                 key the venue declined, or a bridge that bound another tier), so it does NOT RUN \
                 this session — the daemon's log names the cause; it is retried at the next \
                 restart"
            ),
            ArmingBlock::OrdersHeld => format!(
                "{subject} mounts {} with every ORDER HELD: {v}'s arm connects to the account, \
                 serves its data and reconciles it, and REFUSES every order — each one is \
                 rejected, loudly, and none reaches the venue — because its order path at this \
                 tier is not proven yet. No setting lifts the hold; the daemon's log names why it \
                 holds",
                Self::tier_word(self.effective)
            ),
        }
    }
}

#[path = "venue_arming_tests.rs"]
#[cfg(test)]
mod venue_arming_tests;
