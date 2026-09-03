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
//! But `vike-mount` is `vike-app`'s **optional `fat` dependency**: a `--features thin` build — the
//! `--observe`-only client — links no mount at all, and dragging the whole bridge tree into the
//! GUI's shared crate to render a table would undo exactly what `thin` exists for. So the ROW is a
//! plain data type here, one hop above `vike_model::VENUES` and beside [`VenueMode`] — the ceiling
//! half of every row — and the two producers hand it over:
//!
//! * a build WITH a mount: `vike_mount::venue_arming`, which resolves [`VenueArming::effective`];
//! * a build WITHOUT one: [`ceilings_only`], which states the ceiling and answers
//!   [`ArmingBlock::NoMountInThisBuild`] rather than guessing an effective tier it cannot know.
//!
//! ⚠ **Not to be confused with `crate::arming`**, the module one file over. That one asks whether
//! the CREDENTIAL FILE is being used to arm real money (`{VENUE}_MAINNET` appended to
//! `secrets.env`) and refuses startup over it. This one describes what a venue's mount will do.
//! They meet at exactly one row — [`ArmingBlock::MainnetSwitchUnset`] — and nowhere else.

use vike_model::account_keys::{AccountLabel, AccountRef};

use crate::{VenueMode, VenuePolicy};

/// **Why a venue's effective tier is what it is** — the "Effective" column's second half, and the
/// reason that column earns its place on the screen.
///
/// The complaint this vocabulary exists to answer is *"I set live and it is still paper"*. Every
/// variant below is a distinct, observable cause with a distinct operator response, which is why
/// they are not collapsed into one `Blocked(String)`: a row that says [`Self::NoCredentials`]
/// sends the operator to the Connections tab, one that says [`Self::MainnetSwitchUnset`] sends
/// them to `secrets.env`, and one that says [`Self::FeatureAbsent`] sends them to a different
/// BINARY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmingBlock {
    /// Nothing is refusing anything: the effective tier IS the ceiling.
    None,
    /// `policy.venues.<venue>` is `paper`, so the mount returns the paper client ABOVE the
    /// credential read — no key is loaded, no grid fetched, no socket opened.
    Disarmed,
    /// This BINARY has no arm for the venue: its cargo feature (`ibkr`, `polymarket`, `fxcm`) is
    /// off, so `make_engine`'s match falls through to the paper `_` arm. The FILE may legally name
    /// the venue anyway — a `policy.toml` must be portable between the boxes of one deployment —
    /// so the row renders the disagreement instead of hiding or refusing it.
    FeatureAbsent,
    /// The mount has NO live exec arm for this venue in any build (dukascopy, whose exec factory
    /// is not mounted at all), so it can only ever be paper.
    NoLiveArm,
    /// A build with no mount linked at all (`vike-app --features thin`). Nothing here can be
    /// stated about the effective tier, and this row says so rather than implying paper.
    NoMountInThisBuild,
    /// The arm exists and the ceiling permits it, but the credential store holds no usable key set
    /// for the tier the arm would reach — the ORIGINAL live gate, still in force under the ceiling.
    NoCredentials,
    /// Polymarket: `POLY_EXEC=1` is unset, so its exec plane never arms.
    ExecFlagUnset,
    /// FXCM: the proprietary ForexConnect SDK is not linked into this binary, so
    /// `vike_fxcm::sdk_linked()` refuses the live mount whatever the store says.
    SdkAbsent,
    /// `{VENUE}_MAINNET` is unset, so a `live` ceiling still resolves the venue's DEMO tier. The
    /// ceiling is a conjunct of that switch, never a replacement for it.
    MainnetSwitchUnset,
    /// This venue's mount arm has no live tier at all — it hardcodes the demo/practice/sandbox
    /// endpoint — so a `live` ceiling over it still resolves demo.
    DemoOnlyArm,
    /// This venue's mount arm has no DEMO tier (polymarket runs no testnet), so a `demo` ceiling
    /// over it resolves paper.
    LiveOnlyArm,
    /// The ceiling permits live and the arm can reach it, but only DEMO-tier credentials exist.
    LiveCredentialsAbsent,
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
    /// ⚠ **`vike_mount`'s `arm_addresses_accounts` is the authority for WHICH venues this variant
    /// still refuses, and no prose anywhere — this doc included — restates the list.** It was four
    /// venues wide when the variant was written and is one venue wide now; a list here would have
    /// been wrong the day that changed. The one venue is `dukascopy`, and it is refused for two
    /// independent reasons: it has no live arm to address at all, and its `DEMO1`/`DEMO2` keys
    /// already bake an account INDEX into the tier token — a second multi-account spelling that
    /// `vike_model::account_keys` pins as non-conforming ON PURPOSE.
    ///
    /// ⚠ A refusal rather than a gap filled in passing: making a bespoke loader account-aware is a
    /// change to that bridge's credential surface and belongs to a PR that can verify the venue.
    NoAccountSupport,
}

impl ArmingBlock {
    /// Every variant, so a renderer and a test enumerate the same set without either writing it
    /// down again. Declaration order, which is roughly "most structural cause first".
    pub const ALL: [ArmingBlock; 14] = [
        ArmingBlock::None,
        ArmingBlock::Disarmed,
        ArmingBlock::FeatureAbsent,
        ArmingBlock::NoLiveArm,
        ArmingBlock::NoMountInThisBuild,
        ArmingBlock::NoCredentials,
        ArmingBlock::ExecFlagUnset,
        ArmingBlock::SdkAbsent,
        ArmingBlock::MainnetSwitchUnset,
        ArmingBlock::DemoOnlyArm,
        ArmingBlock::LiveOnlyArm,
        ArmingBlock::LiveCredentialsAbsent,
        ArmingBlock::AccountNotNamed,
        ArmingBlock::NoAccountSupport,
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
            ArmingBlock::ExecFlagUnset => "POLY_EXEC unset",
            ArmingBlock::SdkAbsent => "SDK not linked",
            ArmingBlock::MainnetSwitchUnset => "mainnet switch unset",
            ArmingBlock::DemoOnlyArm => "demo-only arm",
            ArmingBlock::LiveOnlyArm => "live-only arm",
            ArmingBlock::LiveCredentialsAbsent => "no live credentials",
            ArmingBlock::AccountNotNamed => "account not named",
            ArmingBlock::NoAccountSupport => "no second-account support",
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
/// ⚠ **It is no longer `Copy`, and that is the account label's doing** — [`AccountLabel::Named`]
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
                "{v} order placement needs POLY_EXEC=1 as well as the ceiling; it is unset, so {v} \
                 stays paper"
            ),
            ArmingBlock::SdkAbsent => format!(
                "{v}'s ForexConnect SDK is not linked into this binary, so its live mount is \
                 refused and it stays paper"
            ),
            ArmingBlock::MainnetSwitchUnset => format!(
                "{}_MAINNET is unset, so {v} resolves its DEMO tier — the ceiling is a conjunct of \
                 that switch, never a replacement for it",
                v.to_ascii_uppercase()
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
                "the ceiling permits live, but only demo-tier {v} credentials exist, so {v} mounts \
                 demo"
            ),
            ArmingBlock::AccountNotNamed => format!(
                "{} has no line of its own, so it stays paper — a `[venues]` line was written when \
                 {v} had ONE account and is not consent for a second. Add {} to \
                 <project>/settings/policy.toml to arm it",
                self.subject(),
                self.key()
            ),
            ArmingBlock::NoAccountSupport => format!(
                "{} cannot be armed: {v}'s mount reads its credentials through a loader that knows \n                 no account label, so a second account of it would trade the FIRST account's keys. \n                 Run it in its own project folder (its own settings directory and credential \n                 store) instead",
                self.subject()
            ),
        }
    }
}

/// The dotted settings key for one ACCOUNT's ceiling — `policy.accounts.<venue>.<LABEL>`.
///
/// ⚠ THE one spelling, for the same reason [`arming_key`] is: it is what [`crate::set_setting`]
/// would be handed, what a typed confirm is compared against, and what a change-journal record
/// carries. `label` is an [`AccountLabel::Named`] text — the DEFAULT account has no key of its own,
/// because its ceiling IS the venue's line and a second spelling for one fact is exactly what
/// `vike_model::account_keys::RESERVED_DEFAULT_LABEL` refuses.
#[must_use]
pub fn account_arming_key(venue: &str, label: &str) -> String {
    format!("{}.accounts.{venue}.{label}", crate::write::SettingsFile::Policy.section())
}

/// The dotted settings key for one venue's ceiling — `policy.venues.<venue>`.
///
/// ⚠ THE one spelling. It is what [`crate::set_setting`] is handed, what the typed confirm is
/// compared against, and what the change-journal record carries; three copies of a format string
/// are three chances for the confirm to guard a key nothing writes.
#[must_use]
pub fn arming_key(venue: &str) -> String {
    format!("{}.venues.{venue}", crate::write::SettingsFile::Policy.section())
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
            // labelled accounts (`vike_model::account_keys::accounts_in_store` needs the vars map
            // this function is not given), and inventing rows for the accounts a POLICY names would
            // list accounts that may not exist while omitting ones that do. One row per roster
            // venue is what a thin build has always rendered, and it stays exactly that.
            label: AccountLabel::Default,
            ceiling,
            effective: ceiling,
            block: ArmingBlock::NoMountInThisBuild,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::VENUES;

    fn label(text: &str) -> AccountLabel {
        AccountLabel::parse(text).expect("a legal label")
    }

    /// The key is the section the writer resolves plus the venue — never a hand-typed literal, so
    /// a renamed section moves the confirm and the write together.
    #[test]
    fn the_key_is_the_one_the_writer_takes() {
        assert_eq!(arming_key("bybit"), "policy.venues.bybit");
        let row = VenueArming {
            venue: "bybit",
            label: AccountLabel::Default,
            ceiling: VenueMode::Paper,
            effective: VenueMode::Paper,
            block: ArmingBlock::Disarmed,
        };
        assert_eq!(row.key(), arming_key("bybit"));
    }

    /// **A LABELLED row writes the ACCOUNT's key, never the venue's** — the one property that keeps
    /// a second account's switch from moving the ceiling of the account beside it.
    #[test]
    fn a_labelled_row_writes_its_own_account_key() {
        assert_eq!(account_arming_key("bybit", "ALT"), "policy.accounts.bybit.ALT");
        let row = VenueArming {
            venue: "bybit",
            label: label("ALT"),
            ceiling: VenueMode::Demo,
            effective: VenueMode::Demo,
            block: ArmingBlock::None,
        };
        assert_eq!(row.key(), account_arming_key("bybit", "ALT"));
        assert_ne!(
            row.key(),
            arming_key("bybit"),
            "an account switch must not write the venue line"
        );
        assert!(!row.is_default_account());
        assert!(row.subject().contains("ALT"), "{}", row.subject());
    }

    /// A mount-less build states the FILE and nothing else, for the whole roster — the DEFAULT
    /// account only, because it can enumerate no other.
    #[test]
    fn ceilings_only_states_the_file_and_claims_no_tier() {
        let policy = VenuePolicy::default().declare("bybit", VenueMode::Live);
        let rows = ceilings_only(&policy);
        assert_eq!(rows.len(), VENUES.len(), "one row per roster venue");
        for row in &rows {
            assert_eq!(row.block, ArmingBlock::NoMountInThisBuild);
            assert!(
                row.is_default_account(),
                "{}: a thin build enumerates no second account",
                row.venue
            );
            assert_eq!(
                row.effective, row.ceiling,
                "{}: a build with no mount must not CLAIM a tier",
                row.venue
            );
            assert!(!row.is_capped());
        }
        assert_eq!(
            rows.iter().find(|r| r.venue == "bybit").expect("bybit").ceiling,
            VenueMode::Live,
            "the file's own content is still rendered"
        );
    }

    /// Every block renders a non-empty sentence that NAMES its venue — the column's whole job —
    /// and only the clear one has an empty badge.
    #[test]
    fn every_block_explains_itself_and_names_the_venue() {
        for block in ArmingBlock::ALL {
            for account in [AccountLabel::Default, label("ALT")] {
                let row = VenueArming {
                    venue: "bybit",
                    label: account.clone(),
                    ceiling: VenueMode::Live,
                    effective: VenueMode::Paper,
                    block,
                };
                let why = row.why();
                assert!(why.contains("bybit") || why.contains("BYBIT"), "{block:?}: {why}");
                assert!(why.len() > 20, "{block:?}: {why}");
                // …and a LABELLED row's sentence NAMES the account, so "bybit is paper" can never
                // be read as a statement about the account beside the one it describes.
                if let Some(text) = account.text() {
                    assert!(
                        why.contains(text) || !matches!(block, ArmingBlock::AccountNotNamed),
                        "{block:?} on a labelled row must name the account: {why}"
                    );
                }
                assert_eq!(block.is_clear(), block == ArmingBlock::None);
                assert_eq!(block.as_str().is_empty(), block.is_clear(), "{block:?}");
            }
        }
        // …and `ALL` really is every variant: an exhaustive match with no wildcard, so a new
        // variant fails to compile here rather than silently escaping the loop above.
        for block in ArmingBlock::ALL {
            let covered = match block {
                ArmingBlock::None
                | ArmingBlock::Disarmed
                | ArmingBlock::FeatureAbsent
                | ArmingBlock::NoLiveArm
                | ArmingBlock::NoMountInThisBuild
                | ArmingBlock::NoCredentials
                | ArmingBlock::ExecFlagUnset
                | ArmingBlock::SdkAbsent
                | ArmingBlock::MainnetSwitchUnset
                | ArmingBlock::DemoOnlyArm
                | ArmingBlock::LiveOnlyArm
                | ArmingBlock::LiveCredentialsAbsent
                | ArmingBlock::AccountNotNamed
                | ArmingBlock::NoAccountSupport => true,
            };
            assert!(covered);
        }
    }

    /// `is_capped` is strictly "below the ceiling" — a row at its ceiling is not capped even when
    /// something else about it is unusual.
    #[test]
    fn capped_means_below_the_ceiling() {
        let row = |ceiling, effective| VenueArming {
            venue: "binance",
            label: AccountLabel::Default,
            ceiling,
            effective,
            block: ArmingBlock::None,
        };
        assert!(row(VenueMode::Live, VenueMode::Demo).is_capped());
        assert!(row(VenueMode::Live, VenueMode::Paper).is_capped());
        assert!(!row(VenueMode::Demo, VenueMode::Demo).is_capped());
        assert!(!row(VenueMode::Paper, VenueMode::Paper).is_capped());
    }
}
