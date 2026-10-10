//! [`VenueArming`] — **one ACCOUNT's row on the arming screen**: the ceiling the operator wrote,
//! the tier the mount would actually reach, and WHY those two differ.
//!
//! ⚠ It was one row per VENUE until the mount began fanning out per account. A box with no
//! `[accounts]` table still gets exactly one row per roster venue — the DEFAULT account's — so the
//! screen it renders is unchanged; a second account adds a row of its own, carrying its own key
//! ([`VenueArming::key`]) and its own ceiling.
//!
//! # Why the type lives here and the ANSWER does not
//!
//! The question "what will venue X actually do when this binary mounts it" is answerable only by
//! `vike-mount`, which owns the per-venue arms and calls every bridge's own config loader
//! (`vike_mount::venue_arming_under` is the producer, and it is the SAME function
//! `would_mount_live_under` — the pre-connect live-intent probe — is derived from, so a row on the
//! screen cannot disagree with the mount about the venue it is describing).
//!
//! But the GUI links no mount: `vike-desktop` has no `vike-mount` edge, and dragging the whole
//! bridge tree into the GUI's shared crate to render a table would undo exactly that. So the ROW is
//! a plain data type here, one hop above `vike_model::VENUES` and beside [`VenueMode`] — the
//! ceiling half of every row — and the two producers hand it over:
//!
//! * a build WITH a mount: `vike_mount::venue_arming`, which resolves [`VenueArming::effective`];
//! * a build WITHOUT one: [`ceilings_only`], which states the ceiling and answers
//!   [`ArmingBlock::NoMountInThisBuild`] rather than guessing an effective tier it cannot know.
//!
//! ⚠ **Not to be confused with `crate::arming`**, the module one file over. That one asks whether
//! the CREDENTIAL FILE is being used to arm real money (`{VENUE}_MAINNET` appended to
//! `secrets.env`) and refuses startup over it. This one describes what a venue's mount will do.

use vike_model::accounts::account_keys::{AccountLabel, AccountRef};

use crate::{VenueMode, VenuePolicy};

/// **Why a venue's effective tier is what it is** — the "Effective" column's second half, and the
/// reason that column earns its place on the screen.
///
/// The complaint this vocabulary exists to answer is *"I set live and it is still paper"*. Every
/// variant below is a distinct, observable cause with a distinct operator response, which is why
/// they are not collapsed into one `Blocked(String)`: a row that says [`Self::NoCredentials`]
/// sends the operator to the Connections tab, one that says [`Self::LiveCredentialsAbsent`] sends
/// them to the credential store's LIVE tier, and one that says [`Self::FeatureAbsent`] sends them
/// to a different BINARY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmingBlock {
    /// Nothing is refusing anything: the effective tier IS the ceiling.
    None,
    /// `policy.venues.<venue>` is `paper`, so the mount returns the paper client ABOVE the
    /// credential read — no key is loaded, no grid fetched, no socket opened.
    Disarmed,
    /// This BINARY has no arm for the venue: its cargo feature (`ibkr`, `polymarket`, `fxcm`) is
    /// off, so `make_engine`'s match falls through to the paper `_` arm. The ceiling may legally
    /// name the venue anyway — the policy rows must be portable between the boxes of one
    /// deployment — so the row renders the disagreement instead of hiding or refusing it.
    FeatureAbsent,
    /// The mount has NO live exec arm for this venue in any build, so it can only ever be paper.
    ///
    /// ⚠ No roster venue answers this today; `vike_mount`'s `venue_arming_under` reaches it for a
    /// venue its
    /// registry carries no row for, i.e. for a venue id that is not on the roster at all.
    NoLiveArm,
    /// A build with no mount linked at all (`vike-desktop`). Nothing here can be
    /// stated about the effective tier, and this row says so rather than implying paper.
    NoMountInThisBuild,
    /// The arm exists and the ceiling permits it, but the credential store holds no usable key set
    /// for the tier the arm would reach — the ORIGINAL live gate, still in force under the ceiling.
    NoCredentials,
    /// Polymarket: `flags.poly_exec` is off, so its exec plane never arms.
    ExecFlagUnset,
    /// FXCM: the ForexConnect shim did not load on this box (the SDK is opened at RUNTIME, not
    /// linked), so `vike_fxcm::sdk_available()` refuses the live mount whatever the store says — a
    /// client mounted there would log in to nothing and reject every order.
    ///
    /// One cell of the store is named BEFORE the shim: a stand-alone LIVE-tier login answers
    /// [`Self::LiveTierNotWired`] on a box without the shim too, because that is a fact about the
    /// store no shim can change and it is what the daemon's log says (fxcm's `resolve` and `mount`
    /// ask in one order: `crates/bridges/fxcm/src/mount.rs`'s `resolution_for`).
    SdkAbsent,
    /// This venue's mount arm has no live tier at all — it hardcodes the demo/practice/sandbox
    /// endpoint — so a `live` ceiling over it still resolves demo.
    DemoOnlyArm,
    /// This venue's mount arm has no DEMO tier (polymarket runs no testnet), so a `demo` ceiling
    /// over it resolves paper.
    LiveOnlyArm,
    /// The ceiling permits live and the arm can reach it, but the store holds no LIVE-tier key set:
    /// a CEX venue or hyperliquid (decision 0095: `live` means mainnet) stays PAPER, aster falls to
    /// its testnet tier.
    LiveCredentialsAbsent,
    /// **A LIVE-tier key set is stored for this account and the arm mounts only its demo tier**, so
    /// the venue stays paper whatever the ceiling says — the cause for a credential that is PRESENT
    /// and UNUSABLE, which the credential doctrine treats as an error rather than an absence.
    ///
    /// Distinct from [`Self::NoCredentials`] on purpose, because the two send an operator to
    /// different places: `NoCredentials` means *no key set for the tier this arm would reach* and
    /// the fix is to write keys; this one means *the keys are there and nothing will ever read
    /// them*, and writing more of them changes nothing — the fix is the demo tier's keys, or (for
    /// the venue that refuses to choose while a live set is stored) removing the live ones. Distinct
    /// from [`Self::LiveCredentialsAbsent`] too, which is the opposite fact: the ceiling is `live`,
    /// the arm COULD use a live set, and none is stored.
    ///
    /// Produced by every roster arm that has no live arm and has a live-tier key vocabulary to find
    /// (`vike_bridge_core::venue_mount::PaperCause::LiveTierNotWired` is the bridge-side name). A
    /// build that wires a venue's live tier retires the cause for that venue.
    LiveTierNotWired,
    /// A LABELLED account with no `policy.accounts.<venue>.<LABEL>` line of its own. The venue's
    /// `[venues]` line was written when that venue had ONE account, so reading it as consent for an
    /// account that did not exist when it was written is the silent escalation the ceiling exists to
    /// prevent — [`VenuePolicy::account`](crate::VenuePolicy::account) resolves such an account to
    /// [`VenueMode::Paper`] and this is that answer, named.
    ///
    /// Unreachable for the DEFAULT account, which INHERITS its venue's line by design; a row
    /// carrying it therefore always names a second account.
    AccountNotNamed,
    /// **This venue's mount arm cannot address a second account at all**, so a labelled one is
    /// refused outright rather than armed onto the default account's keys.
    ///
    /// An arm qualifies only when it threads the account label into a loader that reads THAT
    /// account's key names — `vike_bridge_core::credentials::load_credentials_for_account` for the
    /// generic-credential venues, or the bridge's own `*_for_account` loader, each composing its
    /// bespoke names through `vike_bridge_core::credentials::account_var`. An arm that still read
    /// UNLABELLED names would build a SECOND live client on the DEFAULT account's credentials: two
    /// engines, one venue account, precisely the accident
    /// `vike_exec::ExecutionEngine::route_key` and `vike_ops::live_lock` exist to prevent.
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
    /// **The account is named in settings and the STORE does not know it** — so the mount cannot say
    /// which of the venue's accounts it is, and refuses rather than picking one.
    ///
    /// Distinct from [`Self::NoCredentials`] on purpose, because the two send an operator to
    /// different places: `NoCredentials` means *no key set for the tier this arm would reach*, and
    /// the fix is to write keys; this one means *the keys may well be there and nothing says which
    /// ACCOUNT they are*, and the fix is in the settings database rather than in the credential
    /// store. It is produced today by `dukascopy` alone, whose two demo accounts are two LEGAL
    /// ENTITIES distinguishable only by their `account` row — so guessing is routing an order to a
    /// broker nobody chose — and it is the answer on a box whose store carries no `account` table at
    /// all (a file store, or a database older than the table), where the DEFAULT account still arms
    /// exactly as it always did.
    ///
    /// ⚠ It also covers a STORE FAILURE — a database that exists and will not open — and the mount's
    /// own refusal line says which it was (`vike_dukascopy`'s `DukascopyRefusal::StoreUnreadable`
    /// against its `NoSuchAccount`). One block, two causes, because an operator's next act is the
    /// same for both: look at the store.
    AccountNotInStore,
    /// **Another ACCOUNT of this venue holds the one broker session this process may open**, so this
    /// one stays paper — nothing about its credentials, its ceiling or its `account` row is wrong.
    ///
    /// Produced today by `dukascopy` alone, and it is the only block whose cause is another ROW:
    /// two concurrent JForex sidecars share one platform cache whose corruption makes EVERY login
    /// fail, so exactly one dukascopy account is armed per process and WHICH one is the policy's to
    /// choose — a `policy.accounts.dukascopy.<LABEL>` row above `paper` takes the session, and with
    /// no such row the DEFAULT account keeps it. `vike_mount`'s `exclusive` module carries the rule
    /// (`pick_holder`), and its `dukascopy` module the evidence and the measurement that retires the
    /// limit.
    ///
    /// ⚠ **The DEFAULT account can carry this block**, which no other refusal in this enum can do
    /// to it. That is the intended shape rather than an accident: a labelled account cannot be
    /// armed without the venue line that arms the default account too, so without the default
    /// yielding there is no policy that mounts the second account at all — which is exactly the
    /// state this variant was added to end.
    SidecarHeldElsewhere,
}

impl ArmingBlock {
    /// Every variant, so a renderer and a test enumerate the same set without either writing it
    /// down again. Declaration order, which is roughly "most structural cause first".
    pub const ALL: [ArmingBlock; 16] = [
        ArmingBlock::None,
        ArmingBlock::Disarmed,
        ArmingBlock::FeatureAbsent,
        ArmingBlock::NoLiveArm,
        ArmingBlock::NoMountInThisBuild,
        ArmingBlock::NoCredentials,
        ArmingBlock::ExecFlagUnset,
        ArmingBlock::SdkAbsent,
        ArmingBlock::DemoOnlyArm,
        ArmingBlock::LiveOnlyArm,
        ArmingBlock::LiveCredentialsAbsent,
        ArmingBlock::LiveTierNotWired,
        ArmingBlock::AccountNotNamed,
        ArmingBlock::NoAccountSupport,
        ArmingBlock::AccountNotInStore,
        ArmingBlock::SidecarHeldElsewhere,
    ];

    /// A short, stable badge for the Effective cell — never a sentence. Empty for exactly
    /// [`Self::None`], so a rendered badge always means something is being refused.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ArmingBlock::None => "",
            ArmingBlock::Disarmed => "disarmed",
            ArmingBlock::FeatureAbsent => "feature absent",
            ArmingBlock::NoLiveArm => "no live arm",
            ArmingBlock::NoMountInThisBuild => "no mount in this build",
            ArmingBlock::NoCredentials => "no credentials",
            ArmingBlock::ExecFlagUnset => "flags.poly_exec off",
            ArmingBlock::SdkAbsent => "SDK not loaded",
            ArmingBlock::DemoOnlyArm => "demo-only arm",
            ArmingBlock::LiveOnlyArm => "live-only arm",
            ArmingBlock::LiveCredentialsAbsent => "no live credentials",
            ArmingBlock::LiveTierNotWired => "live tier not wired",
            ArmingBlock::AccountNotNamed => "account not named",
            ArmingBlock::NoAccountSupport => "no second-account support",
            ArmingBlock::AccountNotInStore => "account not in the store",
            ArmingBlock::SidecarHeldElsewhere => "another account holds the session",
        }
    }

    /// Whether this block is the ordinary "nothing is wrong" answer.
    #[must_use]
    pub fn is_clear(self) -> bool {
        self == ArmingBlock::None
    }
}

/// **One ACCOUNT's row.** `venue` is a `vike_model::VENUES` id (a `&'static str` from the roster
/// itself, exactly as [`VenuePolicy`] keys on), so a row can never name a venue that does not
/// exist, and [`Self::label`] says WHICH account of it.
///
/// ⚠ **It is not `Copy`, and that is the account label's doing** — [`AccountLabel::Named`]
/// owns a `String`. Every construction site therefore clones rather than copies; the type is built
/// once per account per frame on a settings screen and once per account at a mount, so nothing on
/// any hot path notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueArming {
    /// The roster id.
    pub venue: &'static str,
    /// **Which account of it.** [`AccountLabel::Default`] — the account a single-account box has —
    /// is what every row carried before this field existed, so a box with no `[accounts]` table
    /// renders exactly the rows it always did, one per roster venue.
    pub label: AccountLabel,
    /// What the operator's ceiling for this ACCOUNT says — `policy.venues.<venue>` for the default
    /// account, `policy.accounts.<venue>.<LABEL>` capped by it for a labelled one
    /// ([`VenuePolicy::account`](crate::VenuePolicy::account) is the resolution).
    pub ceiling: VenueMode,
    /// What the mount would ACTUALLY reach under that ceiling, with this binary's features and
    /// this box's credential store. Never above [`Self::ceiling`] — the ceiling is a `min`.
    pub effective: VenueMode,
    /// Why [`Self::effective`] is where it is.
    pub block: ArmingBlock,
}

impl VenueArming {
    /// The dotted settings key this row's switch writes — **and the string the typed confirm must
    /// equal**. Spelled once, through [`arming_key`] / [`account_arming_key`].
    ///
    /// ⚠ A LABELLED row writes the ACCOUNT's key, never the venue's. Writing the venue line from a
    /// second account's row would move the ceiling of every account under it — including the
    /// default one, which the operator was not looking at.
    #[must_use]
    pub fn key(&self) -> String {
        match self.label.text() {
            None => arming_key(self.venue),
            Some(label) => account_arming_key(self.venue, label),
        }
    }

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

    /// Is this the account a single-account box has? Every box with no `[accounts]` table has only
    /// these.
    #[must_use]
    pub fn is_default_account(&self) -> bool {
        self.label.is_default()
    }

    /// **This account's ROUTING key** — `vike_exec::ExecutionEngine::route_key`, and therefore the
    /// `LIVE-<route_key>.lock` filename `vike_ops::live_lock::LiveLock::acquire` takes and the name
    /// this account appears under in `vike_mount::make_engine_accounts`' `live_venues` record.
    ///
    /// ⚠ Rendered by [`AccountRef::route_key`] and NOWHERE else. Three separate places need this
    /// string — the pre-mount armed set a lock is claimed from, the post-mount record it is checked
    /// against, and the journal row — and a second spelling of it would be a lock held under a name
    /// nothing else uses. For the DEFAULT account it is the bare venue id, which is what
    /// `ExecutionEngine::new` already seeds the field with, so a single-account box's sentinel
    /// filenames do not move.
    ///
    /// The `tier` handed to the ref is the one this row RESOLVED, and it does not reach the answer:
    /// `AccountRef::route_key` deliberately carries no tier (one process mounts a venue at one
    /// tier). It is passed truthfully rather than hardcoded so the ref is never a lie.
    #[must_use]
    pub fn route_key(&self) -> String {
        AccountRef {
            venue: self.venue,
            tier: if self.effective == VenueMode::Live { "LIVE" } else { "DEMO" },
            label: self.label.clone(),
        }
        .route_key()
    }

    /// Whether the effective tier is BELOW the ceiling — i.e. whether the row is telling the
    /// operator something they did not ask for.
    #[must_use]
    pub fn is_capped(&self) -> bool {
        self.effective < self.ceiling
    }

    /// One operator-facing sentence for the Effective cell's detail line: what the row shows and
    /// what to do about it. Names the venue's own variable where a variable is the cause, so the
    /// answer is actionable without a second lookup.
    #[must_use]
    pub fn why(&self) -> String {
        let v = self.venue;
        match self.block {
            ArmingBlock::None => format!("{v} mounts {} — nothing is capping it", self.effective),
            ArmingBlock::Disarmed => {
                format!("{} is `paper`, so {v} loads no credential and opens no socket", self.key())
            }
            ArmingBlock::FeatureAbsent => format!(
                "this build has no {v} arm (its cargo feature is off), so the mount falls through \
                 to paper — the ceiling in the file is still valid, and a build that HAS the \
                 feature will honour it"
            ),
            ArmingBlock::NoLiveArm => {
                format!("{v} has no live exec arm in any build — it can only be paper")
            }
            ArmingBlock::NoMountInThisBuild => format!(
                "this build links no venue mount (a thin `--observe` client), so what {v} would do \
                 cannot be answered here — the ceiling beside it is still the file's"
            ),
            ArmingBlock::NoCredentials => format!(
                "no usable {v} credentials in the store — absent credentials are still the live \
                 gate, so {v} stays paper"
            ),
            ArmingBlock::ExecFlagUnset => format!(
                "{v} order placement needs flags.poly_exec as well as the ceiling; it is off, so \
                 {v} stays paper — vike-cli config set flags.poly_exec true"
            ),
            ArmingBlock::SdkAbsent => format!(
                "{v}'s ForexConnect shim did not load on this box, so its live mount is refused \
                 and it stays paper — a live client there would reject every order"
            ),
            ArmingBlock::DemoOnlyArm => format!(
                "{v}'s mount arm has no live tier — it always connects to the venue's demo \
                 endpoint, so a `live` ceiling still reaches demo"
            ),
            ArmingBlock::LiveOnlyArm => format!(
                "{v} runs no testnet, so its arm refuses anything below `live` and a `demo` \
                 ceiling leaves it on paper"
            ),
            ArmingBlock::LiveCredentialsAbsent => format!(
                "the ceiling is `live`, which means MAINNET, but the store holds no LIVE-tier {v} \
                 credentials, so {v} mounts {}",
                self.effective
            ),
            ArmingBlock::LiveTierNotWired => format!(
                "{subject} has a LIVE-tier credential set stored, but {v}'s mount arm mounts only \
                 its demo tier and never selects a live one, so {subject} stays PAPER whatever the \
                 ceiling says. Nothing was signed and no order can reach the live account from this \
                 build. To trade the demo account, store its demo-tier keys — and where {v} refuses \
                 to choose a tier while a live set is stored (oanda), remove the live-tier keys \
                 too. `vike-cli secrets list` names what the store holds",
                subject = self.subject()
            ),
            ArmingBlock::AccountNotNamed => format!(
                "{} has no row of its own, so it stays paper — a `policy.venues` row was written \
                 when {v} had ONE account and is not consent for a second. Run `vike-cli config \
                 set {} <mode>` to arm it",
                self.subject(),
                self.key()
            ),
            ArmingBlock::NoAccountSupport => format!(
                "{} cannot be armed: {v}'s mount reads its credentials through a loader that \
                 knows no account label, so a second account of it would trade the FIRST \
                 account's keys. Run it in its own project folder (its own settings directory \
                 and credential store) instead",
                self.subject()
            ),
            ArmingBlock::AccountNotInStore => format!(
                "{} cannot be armed: the settings database does not name it, so nothing says WHICH \
                 {v} account it is and the mount refuses rather than picking one — on {v} the \
                 accounts are separate brokers. `vike-cli secrets accounts` prints the rows this \
                 box has and `vike-cli secrets set-book` writes the identifier the venue itself \
                 gave one; a box whose store carries no account table at all needs \
                 `vike-cli secrets init` first. The DEFAULT account is unaffected",
                self.subject()
            ),
            ArmingBlock::SidecarHeldElsewhere => format!(
                "{} cannot be armed: this process opens ONE {v} broker session and another {v} \
                 account holds it. Nothing about this account's credentials, its ceiling or its \
                 store row is wrong — it lost a process-wide resource, and WHICH account gets it \
                 is the policy's to choose: a `policy.accounts.{v}.<LABEL>` row above `paper` takes \
                 it, and with no such row the DEFAULT account keeps it. To trade both at once, give \
                 the second account its own project folder (its own settings directory and \
                 credential store) and run a second process there",
                self.subject()
            ),
        }
    }
}

/// The dotted settings key for one ACCOUNT's ceiling — `policy.accounts.<venue>.<LABEL>`.
///
/// ⚠ THE one spelling, for the same reason [`arming_key`] is: it is what [`crate::write_setting_row`]
/// would be handed, what a typed confirm is compared against, and what a change-journal record
/// carries. `label` is an [`AccountLabel::Named`] text — the DEFAULT account has no key of its own,
/// because its ceiling IS the venue's line and a second spelling for one fact is exactly what
/// `vike_model::accounts::account_keys::RESERVED_DEFAULT_LABEL` refuses.
#[must_use]
pub fn account_arming_key(venue: &str, label: &str) -> String {
    format!("{}.accounts.{venue}.{label}", crate::write::SettingsSection::Policy.section())
}

/// The dotted settings key for one venue's ceiling — `policy.venues.<venue>`.
///
/// ⚠ THE one spelling. It is what [`crate::write_setting_row`] is handed, what the typed confirm is
/// compared against, and what the change-journal record carries; three copies of a format string
/// are three chances for the confirm to guard a key nothing writes.
#[must_use]
pub fn arming_key(venue: &str) -> String {
    format!("{}.venues.{venue}", crate::write::SettingsSection::Policy.section())
}

/// The rows a build with **no mount linked** can honestly state: every roster venue's ceiling, and
/// [`ArmingBlock::NoMountInThisBuild`] in place of an effective tier.
///
/// `effective` is set to the ceiling rather than to `Paper`, deliberately: `Paper` would be a
/// CLAIM — "this venue is on paper" — and this build is in no position to make it. Pairing the
/// ceiling with a block that says so keeps the column from asserting anything false while still
/// rendering the file's own content, which is the half a thin client CAN show.
#[must_use]
pub fn ceilings_only(policy: &VenuePolicy) -> Vec<VenueArming> {
    policy
        .iter()
        .map(|(venue, ceiling)| VenueArming {
            venue,
            // The DEFAULT account only. A mount-less build cannot enumerate the credential store's
            // labelled accounts (`vike_model::accounts::account_keys::accounts_in_store` needs the vars map
            // this function is not given), and inventing rows for the accounts a POLICY names would
            // list accounts that may not exist while omitting ones that do. One row per roster
            // venue is what a mount-less build (the desktop) renders.
            label: AccountLabel::Default,
            ceiling,
            effective: ceiling,
            block: ArmingBlock::NoMountInThisBuild,
        })
        .collect()
}

#[path = "venue_arming_tests.rs"]
#[cfg(test)]
mod venue_arming_tests;
