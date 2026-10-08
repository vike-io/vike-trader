//! The row vocabulary: profile kinds, and the profile, mount, recorder and subscription rows.

use std::collections::BTreeMap;

use super::*;

// ---------------------------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------------------------

/// Which PROFILE DOCUMENT a row is a body of.
///
/// ⚠ **`Recorder` LANDED 2026-09-16, and this enum used to have TWO variants with a NAMED REFUSAL
/// where the third is.** `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`
/// answered `recorder.toml` with a NO, on the ground that it belongs to a different daemon and that
/// moving it would settle the deploy-layout question by accident. The owner OVERRULED that on
/// 2026-09-16, and what fired is the record's own stated reopener: the deploy-layout question was
/// answered the same day (ONE unit file per daemon, the project root a substitutable parameter).
/// The schema's `CHECK` already carried the word — [`profile_ddl`]'s own doc records that as
/// deliberate foresight — so the flip needed no column change and no migration of an existing
/// store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProfileKind {
    /// The daemon profile, `vike_tradehub::config::DaemonProfile` — what `settings/tradehub.toml`
    /// held before it became rows (the file is now only an import source for `vike-cli config
    /// mirror`). The mount set.
    Daemon,
    /// `settings/run-live.toml` — `vike_core::RunProfile`. The `[risk]` ceilings and the sinks.
    Run,
    /// `settings/recorder.toml` — `vike_recorder::config::RecorderProfile`. The store root, the
    /// subscription list, and the maintenance/alerting knobs.
    ///
    /// ⚠ **Unlike the other two, this document decides WHICH VENUE FEEDS OPEN.** A wrong body is
    /// not a mis-read setting; it is a daemon subscribing to something nobody asked for. That is
    /// why the migration's fence is a ROUND-TRIP equality against the profile in force rather than
    /// [`plan_active_row`] alone — see `crates/vike-cli/src/cmd/config/mirror_recorder.rs`.
    Recorder,
}

impl ProfileKind {
    /// The `profile.kind` word. A `&'static str` from a closed enum — never operator input, which
    /// is what keeps it safe to compare against the CHECK.
    #[must_use]
    pub fn sql_word(self) -> &'static str {
        match self {
            ProfileKind::Daemon => "daemon",
            ProfileKind::Run => "run",
            ProfileKind::Recorder => "recorder",
        }
    }

    /// Parse a stored `kind`.
    ///
    /// ⚠ `recorder` was a NAMED refusal here, citing 0057's NO. That NO was overruled by the owner
    /// on 2026-09-16 and the word parses now; the refusal arm is DELETED rather than softened,
    /// because a word the CHECK admits and this function refuses is a row nothing can read.
    ///
    /// # Errors
    ///
    /// [`ProfileError::UnreadableKind`] for anything the CHECK would have refused, naming the word.
    pub fn parse(word: &str) -> Result<Self, ProfileError> {
        match word {
            "daemon" => Ok(ProfileKind::Daemon),
            "run" => Ok(ProfileKind::Run),
            "recorder" => Ok(ProfileKind::Recorder),
            other => Err(ProfileError::UnreadableKind { kind: other.to_string() }),
        }
    }
}

/// One `profile` row's own columns — the identity, not the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRow {
    /// The profile's NAME, which is its primary key and what an `active` selection names.
    pub name: String,
    /// Which document this is a body of.
    pub kind: ProfileKind,
    /// **The owner's ruling, as one bit.** Exactly one row per [`ProfileKind`] may carry it
    /// (`profile_one_active_per_kind`), and a kind with NO active row is the state every box is in
    /// today.
    pub active: bool,
    /// The operator's own note. Never read by any resolver.
    pub note: Option<String>,
}

/// One `mount` row — `vike_tradehub::config::MountCfg` as columns, plus the explicit primary.
///
/// Every field except [`Self::venue`] and [`Self::ord`] is optional, exactly as the TOML spelling
/// is: an omitted column and an omitted key must mean the same thing, or a round trip through the
/// store would silently freeze a default.
#[derive(Debug, Clone, PartialEq)]
pub struct MountRow {
    /// The row's ORDINAL — what a TOML array-of-tables has and a table does not. It is preserved
    /// because it is an OBSERVABLE: mount indices are attribution keys (`coid_mount` values, the
    /// `{idx}|` strategy-tag prefix), so re-ordering rows re-labels a live daemon's ledger.
    ///
    /// ⚠ **It is NOT the primary.** That is [`Self::is_primary`], and the split is the whole of
    /// 0057's *"the primary must become EXPLICIT"*.
    pub ord: i64,
    /// **THE DECLARED PRIMARY.** See [`StoredProfile::primary`].
    pub is_primary: bool,
    /// `MountCfg::venue`, with the profile's `"polymarket"` default already applied — a row always
    /// names its venue, because a NULL here could not be told from the default.
    pub venue: String,
    /// **WHAT PRODUCT this mount trades**, as the stored word of a `vike_model::AssetClass` —
    /// `"CryptoSpot"`, `"CryptoPerp"`, `"Option"`, … Phase 5 of
    /// `docs/decisions/0061-an-instrument-names-its-kind.md`, ORDERED by the owner: *"we need to add
    /// type if it is spot or perp or option or anything else bcz it will help us in future"*.
    ///
    /// ⚠ **NOT an `Option`, and that is the whole point.** Every other field here is optional
    /// because an omitted column and an omitted TOML key must mean the same thing; this one is
    /// REQUIRED because a mount that does not say which product it trades is under-specified, and a
    /// nullable column would make 0061's *"a missing claim becomes a legal value"* hazard permanent
    /// for this seam. The schema enforces it twice — `NOT NULL` and a `CHECK` over the vocabulary.
    ///
    /// ⚠ A `String` rather than a typed enum for the reason [`profile_ddl`]'s doc gives at length —
    /// and ⚠ that reason has EXPIRED, twice: `AssetClass` is not a layer-20 type any more (it lives
    /// in `vike-model` at rank 10, which is INSIDE tier 15's floor — the band's rule is *nothing
    /// above rank 10*, not *no `vike-*` dependency*), and the manifest then declared that edge
    /// outright (`docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`,
    /// accepted 2026-09-20). The column stays a `String` because [`profile_ddl`]'s doc RULED the
    /// vocabulary parameter in place (2026-09-26) on design grounds — a typed enum here would be
    /// the collapse wearing a different hat; nothing here rests on the expired reason.
    /// The word is produced by `vike_model::AssetClass::sql_word` and read back by
    /// `AssetClass::from_sql_word`; a word the schema would have refused can only come from a
    /// hand-edited store, and it parses to `None` there rather than to a wrong variant.
    pub asset_class: String,
    /// `MountCfg::symbol` — exactly one of this and [`Self::token_id`] is set, which the schema's
    /// `CHECK ((symbol IS NULL) != (token_id IS NULL))` enforces rather than the loader.
    pub symbol: Option<String>,
    /// `MountCfg::token_id` — the Polymarket spelling of [`Self::symbol`].
    pub token_id: Option<String>,
    /// `MountCfg::interval`.
    pub interval: Option<String>,
    /// `MountCfg::interval_ms`.
    pub interval_ms: Option<i64>,
    /// `MountCfg::resolution_ts_ms`.
    pub resolution_ts_ms: Option<i64>,
    /// `MountCfg::qty`.
    pub qty: Option<f64>,
    /// `MountCfg::half_spread`.
    pub half_spread: Option<f64>,
    /// `MountCfg::tick_size`.
    pub tick_size: Option<f64>,
    /// `MountCfg::seed_cash`.
    pub seed_cash: Option<f64>,
    /// `MountCfg::data_only`.
    pub data_only: Option<bool>,
    /// `MountCfg::account` — the label, NULL meaning the venue's DEFAULT account, which is how
    /// `vike_model::accounts::account_keys::AccountLabel::Default` renders everywhere else in this tree.
    pub account: Option<String>,
    /// `StrategyCfg`'s registry NAME.
    pub strategy_name: Option<String>,
    /// `StrategyCfg`'s Rhai script PATH.
    pub strategy_rhai: Option<String>,
}

impl MountRow {
    /// A minimal row: an ordinal, a venue, an asset class, everything else absent. Every constructor
    /// in the tests starts here so a field added to [`MountRow`] cannot silently acquire a value in
    /// one.
    ///
    /// ⚠ `asset_class` is a PARAMETER rather than a default, which is the Rust half of the column's
    /// `NOT NULL`: there is no way to build a row that does not name its product, so the refusal
    /// happens at the type rather than at the database. Pass
    /// `vike_model::AssetClass::sql_word()`.
    #[must_use]
    pub fn new(ord: i64, venue: &str, asset_class: &str) -> Self {
        MountRow {
            ord,
            is_primary: false,
            venue: venue.to_string(),
            asset_class: asset_class.to_string(),
            symbol: None,
            token_id: None,
            interval: None,
            interval_ms: None,
            resolution_ts_ms: None,
            qty: None,
            half_spread: None,
            tick_size: None,
            seed_cash: None,
            data_only: None,
            account: None,
            strategy_name: None,
            strategy_rhai: None,
        }
    }

    /// The mount SYMBOL from whichever spelling the row used — the row twin of
    /// `vike_tradehub::config::DaemonProfile::mount_symbol`, and deliberately the same shape: the
    /// schema's exclusive CHECK has already refused a row that set neither or both.
    #[must_use]
    pub fn mount_symbol(&self) -> &str {
        self.symbol.as_deref().or(self.token_id.as_deref()).unwrap_or_default()
    }
}

/// **WHICH mount is the daemon's singular identity**, and how that was decided.
///
/// # Why this is an enum rather than a `usize`
///
/// MEASURED in `crates/vike-tradehub/src/tradehub_cli.rs`: the primary is `resolved[0]` — the first
/// mount row — and its own comment calls it *"the daemon's historical singular identity (summary
/// token, mode line, seed policy)"*. 0057 adds the half that comment does not: on a paper core
/// `crates/vike-mount/src/run.rs` derives ENGINE ORDER from first-appearance venue order, while on a
/// LIVE core row order decides nothing at all, because `crates/vike-mount/src/node/build.rs`'s `build_node`
/// builds its engine list straight-line from `WIRED_MARKETS`. So *"row 0 silently means three things
/// on paper and nothing on live"*.
///
/// A table has no inherent order. Reproducing that from rows with an ordinal alone would carry the
/// accident forward and make it harder to see; declaring it makes the meaning readable and leaves
/// the accident named. Both arms exist because **the migration must not change the answer**: a
/// profile that declares nothing keeps the first row, byte for byte, and says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primary {
    /// A row carries `is_primary = 1`. The ordinal is its `ord`, which need NOT be the lowest —
    /// that is the entire point of the column.
    Declared(i64),
    /// No row declares one, so the LOWEST ordinal is the primary — today's rule, unchanged. A
    /// caller that reports the mount set should say *implicit* rather than print a bare number.
    ImplicitFirst(i64),
    /// The profile has no mounts at all. Nothing can be primary, and a live mount of this profile
    /// is a refusal somewhere above rather than a choice made here.
    NoMounts,
}

/// A whole profile as it sits in the store: the identity row, its mounts, and its non-mount leaves.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredProfile {
    /// The `profile` row.
    pub row: ProfileRow,
    /// The `mount` rows, **ordered by `ord`** — the read path sorts, so a caller never depends on
    /// SQLite's row order for a fact the schema says is in a column.
    pub mounts: Vec<MountRow>,
    /// The `mount_param` rows, keyed `(mount ord, key)` → the TOML scalar rendering of the value.
    pub params: BTreeMap<(i64, String), String>,
    /// The `profile_setting` rows, keyed by dotted path → the TOML scalar rendering of the value.
    pub settings: BTreeMap<String, String>,
    /// The `recorder` + `subscription` rows, present only on a [`ProfileKind::Recorder`] profile.
    ///
    /// `None` on every other kind and on every store written before 2026-09-16, which is the same
    /// shape [`Profiles::none`] gives the whole read path: a body that is not there behaves
    /// exactly as it did before the tables existed.
    pub recorder: Option<RecorderBody>,
}

/// A `recorder` profile's whole body — the scalars row plus its subscriptions, in `ord` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderBody {
    /// The one `recorder` row.
    pub row: RecorderRow,
    /// The `subscription` rows, **ordered by `ord`**.
    pub subscriptions: Vec<SubscriptionRow>,
}

/// The `recorder` table's columns for one profile.
///
/// ⚠ **Every `Option` here means ABSENT IN THE PROFILE, never "the default".** The defaults are
/// `vike_recorder::config`'s `Maintenance`/`Alerting` `Default` impls and they stay there; a row
/// that stored a resolved default would make [`render_recorder_toml`] emit a key the operator's
/// file did not carry, and the migration's fence is that the rendered document and the file PARSE
/// EQUAL. See [`profile_ddl`]'s decision 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderRow {
    /// `store` — the profile's store root. Required, exactly as in the file.
    pub store: String,
    /// `[maintenance].interval_secs`.
    pub interval_secs: Option<i64>,
    /// `[maintenance].min_parts`.
    pub min_parts: Option<i64>,
    /// `[maintenance].target_mb`.
    pub target_mb: Option<i64>,
    /// `[maintenance].max_merge_rows`.
    pub max_merge_rows: Option<i64>,
    /// `[maintenance].retention_days`.
    pub retention_days: Option<i64>,
    /// `[alerting].webhooks`, as the TOML ARRAY rendering (`["telegram"]`) — never a token. The
    /// profile names TARGETS and the credential store holds what is behind a name.
    pub alert_webhooks: Option<String>,
    /// `[alerting].repeat_secs`.
    pub alert_repeat_secs: Option<i64>,
    /// `[alerting].series_prefix`.
    pub alert_series_prefix: Option<String>,
    /// The operator's own note. **Required by the design rather than optional decoration**: the CI box's
    /// live profile carries an inline measurement comment on `max_merge_rows` that is the only
    /// place on that box where the number's justification lives, and a migration that dropped it
    /// would be the largest unpriced loss in the move.
    pub note: Option<String>,
}

/// One `[[subscribe]]` entry as a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionRow {
    /// Row identity, and the order feeds are built in. ⚠ **NOT an attribution key** — see
    /// [`profile_ddl`]'s decision 5 for why that distinction from `mount.ord` is written down.
    pub ord: i64,
    /// `venue`. No `CHECK`: the recordable set is a per-BUILD fact.
    pub venue: String,
    /// `family` — a whole market family, recorded as ONE grouped series.
    pub family: Option<String>,
    /// `symbols`, as the TOML ARRAY rendering (`["BTCUSDT.P"]`) — recorded per-symbol.
    pub symbols: Option<String>,
    /// `backfill` — `venue` | `archive` | `off`. `None` = the key was absent.
    pub backfill: Option<String>,
    /// The operator's own note for this subscription — the inline comment a TOML file carries.
    pub note: Option<String>,
}
