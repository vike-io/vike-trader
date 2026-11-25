//! [`ArmingBlock`] — **why an account does not run at the tier its row states**, the one
//! vocabulary every daemon, client and report shares: the mount's projection, the arming table, the
//! trade wire (as [`ArmingBlock::word`]) and the core's not-running refusal.
//!
//! It came down from `vike-config` (where `vike_config::ArmingAccount`, the row that carries it, still
//! lives) so that a crate below the settings loader names the reason without a settings edge. The
//! words, the badges and the declaration order are unchanged by the move: [`ArmingBlock::word`] is a
//! wire spelling, pinned whole by this module's tests.

/// **Why an account does not run at the tier its row states** — the "Effective" column's second
/// half, and the reason a NOT RUNNING account is shown with.
///
/// The complaint this vocabulary exists to answer is *"I set live and it does not run"*. Every
/// variant below is a distinct, observable cause with a distinct operator response, which is why
/// they are not collapsed into one `Blocked(String)`: a row that says [`Self::AccountInactive`]
/// sends the operator to `vike-cli account activate`, one that says
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
    /// (`vike_venues::TierScope::Venue`: ibkr, decision 0140) the rows are those of the venue, OF
    /// ANY LABEL, and no account of it runs.
    TierConflict,
    /// **The account states no symbol for its engine to be mounted on** - its active row at the tier it
    /// would run at has no `account.symbol` (a derived lane twin: its parent states no `lane_twin_symbol`
    /// fact), so it does NOT RUN (decision 0156, amended 2026-11-13). No symbol is ever chosen for it in
    /// code: the operator states one (`vike-cli account set-symbol`). Decided above the credential
    /// read, from the rows alone. Never the answer on a venue whose engine is account-wide (polymarket).
    NoAccountSymbol,
    /// **The account's tier is `live` and the link conformance harness has not proven its venue's connection
    /// contract**, so it does NOT RUN (decision 0156, amended 2026-11-22, the owner's answer to the connection-sync
    /// plan's Q5: an unproven venue arms `demo`, never `live`). The proof is the venue's exec row in
    /// `vike_venues::venue_connection` (`VenueConnectionConformance::Unproven`, which the harness holds to its own
    /// answer). Its `demo` and `paper` accounts are untouched, and a `live` one is never run at demo in its place.
    /// Decided above the credential read, from the row and the table alone; the remedy is a harness row (code), or
    /// the account's tier.
    LiveConnectionUnproven,
    /// The GATEWAYHUB binary that answered has no arm for the venue (the venue half lives there
    /// since wave 2, so a tradehub reads this word off the gatewayhub's arming table and says it of
    /// THAT process): its cargo feature (`ibkr`, `polymarket`, `fxcm`) is off, so the account does
    /// not run here. The `account` row may legally name the venue anyway —
    /// the settings database must be portable between the boxes of one deployment — so the row
    /// renders the disagreement instead of hiding or refusing it.
    FeatureAbsent,
    /// The mount has NO exec arm for this venue in any build, so a `demo` or `live` account of it
    /// can never run.
    ///
    /// ⚠ No roster venue answers this today; `crates/vike-gatewayhub/src/trade/adapters/arming.rs`'s
    /// `venue_arming_under` reaches it for a venue its registry carries no row for, i.e. for a venue id that is not on the roster at all.
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
    /// store no shim can change and it is what the daemon's log says (fxcm's `resolve` and `start`
    /// ask in one order: `crates/bridges/fxcm/src/adapter.rs`'s `resolution_for`).
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
    /// (`vike_bridge_core::venue_adapter::ArmingNotRunningCause::LiveTierNotWired` is the bridge-side name), and
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
    /// `vike_execution::Engine::route_key` and `vike_boot::live_lock` exist to prevent.
    ///
    /// ⚠ **Each bridge's `VenueAdapterDeclaration::addresses_accounts` is the authority for WHICH venues
    /// this variant still refuses, and no prose anywhere — this doc included — restates the
    /// list.** (docs/decisions/0096.) **No roster venue produces it** — dukascopy addresses its
    /// accounts through the settings database's `account` table
    /// (`crates/bridges/dukascopy/src/adapter.rs`).
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
    /// it. `crates/vike-gatewayhub/src/trade/adapters/exclusive.rs` carries the rule, and
    /// `crates/bridges/dukascopy/src/adapter.rs`'s module doc the evidence and the measurement that
    /// retires the limit.
    ///
    /// ⚠ **The DEFAULT account can carry this block**, which no other refusal in this enum can do
    /// to it: when a labelled dukascopy account arms, the default yields the session to it.
    SidecarHeldElsewhere,
    /// **The startup preflight hard-FAILED for this venue** (`vike_tradehub_core::preflight`'s
    /// `PreflightVenueDisposition::Stopped`: the venue answered and refused the credentials, or its clock is
    /// measurably out of band), so none of its `demo`/`live` accounts runs this session. A probe that
    /// merely did not answer WARNs and stops nothing.
    PreflightFailed,
    /// **The probe armed this account and the venue's mount did not come up at its tier**: a
    /// key the venue declined (hyperliquid, polymarket, a cTrader grant), a venue answer that cannot be
    /// used (a contract row it words differently), an exclusive resource another start already holds,
    /// or a bridge that bound a tier other than the account's (refused by the mount's tier interlock). Unlike every other
    /// block it is known only AFTER the network, so the projection cannot predict it; the daemon's
    /// log names which cause it was. The account does not run, and the tradehub does NOT ask again by
    /// itself (a refusal parks: only the operator's change or a restart starts it), except a start the
    /// tradehub's own stop of the account still holds the lease of. A start that failed on a
    /// network failure or another kind a later try can pass answers [`Self::VenueUnreachable`] instead.
    AdapterFailed,
    /// **The venue's own endpoint could not be reached at the adapter start**: a venue call that
    /// failed on a network failure (IB Gateway not running, a refused or reset connection, DNS, a
    /// timeout, a TLS handshake) or on another kind a later try can pass (a venue 5xx, a rate limit,
    /// a maintenance window), so nothing about the account, its keys or its tier was refused (owner 2026-11-15:
    /// "the gateway or SDK not up yet, or a network failure"). TRANSIENT: the tradehub asks again on its
    /// backoff until it connects, and the account joins the running node by itself. Reported by every bridge
    /// whose start makes a venue call, from the one classification of a venue failure
    /// (`VenueAdapterExecutionOutcome::Unreachable`, `AdapterOutcome::failed`; owner 2026-11-16); a refusal is
    /// [`Self::AdapterFailed`].
    VenueUnreachable,
    /// **The gatewayhub did not answer**, so this `demo`/`live` account does NOT RUN this session
    /// (decision 0164 verdict D7). The tradehub opens no venue connection of its own: every venue
    /// client lives in `vike-gatewayhub`, and a tradehub started while that daemon is down waits a
    /// bounded `config.gatewayhub_connect_wait_ms`, then stops these accounts with this word and
    /// carries on with the ones that need no venue (a `paper` account runs the simulator and is
    /// untouched). It is never a paper stand-in (decision 0156).
    ///
    /// Distinct from [`Self::AdapterFailed`] on purpose: that one says the venue's own start
    /// refused, so the operator looks at the venue; this one says the daemon that would have
    /// started it was not there, so the operator looks at the gatewayhub, and the tradehub arms the
    /// account itself once the gatewayhub answers (one bounded graceful restart per process start).
    GatewayUnavailable,
    /// **The account mounts AT its tier and every order is refused**: the arm connects to the
    /// account, serves its data and reconciles it, and sends NO order. Each submit is rejected,
    /// terminally and loudly, so no order vanishes and none reaches the venue.
    ///
    /// One of the three blocks under which the account RUNS ([`Self::runs`]): its
    /// `vike_config::ArmingAccount::effective` is the account's own tier, and this word is the only thing on the
    /// row that says orders are held. The bridge-side name is
    /// `vike_bridge_core::venue_adapter::ArmingHeldBelowLive::OrdersHeld`: an arm whose order path is not
    /// proven at that tier (ibkr's `cpapi` LIVE tier, which has no observed live fill path:
    /// `docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, stage A). The hold is
    /// code, not a setting: no row lifts it.
    OrdersHeld,
    /// **The price feeds could not be had**, so this `demo`/`live` account does NOT RUN this session: the
    /// tradehub takes every price from the datahub's strategy lane (wave 2), and the lane (the datahub, or the
    /// gatewayhub behind it) did not answer, or the box holds no strategy-lane key. Nothing about the account
    /// is wrong and the venue's exec could have been armed; an engine that would trade with no prices at
    /// all is not started (decision 0156: NOT RUNNING with the reason, never a paper stand-in). Distinct from
    /// [`Self::GatewayUnavailable`] on purpose: that one says the daemon that owns the venue clients did not
    /// answer THIS tradehub (`vike-backend venues` asks the gatewayhub directly and shows the account as would-arm
    /// while this word is on the tradehub's own table), this one says the PRICE path is away. The daemon's
    /// log carries the cause in words; the tradehub arms the account itself once the path answers (the late
    /// arm). Produced by the tradehub's own feed gate and never by the gatewayhub.
    FeedsUnavailable,
    /// **The FX market is closed, so this account of an FX venue does not log in until it opens** (owner ruling
    /// 2026-11-13: no login and no login retry while the FX week is closed, `vike_venues::session::FX_WEEK`, read
    /// through `vike_venues::session::fx_login_closed_until`). Nothing about the account is wrong. The gatewayhub
    /// refuses a NEW venue session with it (a session that is already logged in keeps running), its words name
    /// the next open (`vike_venues::session::fx_login_closed_words`), and the tradehub starts the account itself
    /// at that open, once.
    FxMarketClosed,
}

impl ArmingBlock {
    /// Every variant, so a renderer and a test enumerate the same set without either writing it
    /// down again. Declaration order, which is roughly "most structural cause first".
    pub const ALL: [ArmingBlock; 27] = [
        ArmingBlock::None,
        ArmingBlock::PaperTier,
        ArmingBlock::AccountInactive,
        ArmingBlock::NoAccountRow,
        ArmingBlock::TierConflict,
        ArmingBlock::NoAccountSymbol,
        ArmingBlock::LiveConnectionUnproven,
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
        ArmingBlock::AdapterFailed,
        ArmingBlock::VenueUnreachable,
        ArmingBlock::GatewayUnavailable,
        ArmingBlock::OrdersHeld,
        ArmingBlock::FeedsUnavailable,
        ArmingBlock::FxMarketClosed,
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
            ArmingBlock::NoAccountSymbol => "no account.symbol",
            ArmingBlock::LiveConnectionUnproven => "connection unproven for live",
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
            ArmingBlock::AdapterFailed => "adapter failed",
            ArmingBlock::VenueUnreachable => "venue unreachable",
            ArmingBlock::GatewayUnavailable => "gatewayhub unavailable",
            ArmingBlock::OrdersHeld => "orders held",
            ArmingBlock::FeedsUnavailable => "price feeds unavailable (datahub)",
            ArmingBlock::FxMarketClosed => "FX market closed",
        }
    }

    /// **The wire word**: a stable identifier for this block, the spelling the trade wire carries
    /// (`vike-gatewayhub-client`'s `ArmingRow::block_word` and `AdapterReply::NotRunning`) and the
    /// only one [`Self::from_word`] reads back. Not [`Self::as_str`]: that is a badge for a person
    /// (`None` is the empty string there, and most badges hold spaces and dots), this is a key for a
    /// program and never changes with a rewording.
    ///
    /// ⚠ EXHAUSTIVE on purpose, no `_` arm: a new block fails to compile here until somebody names
    /// its word, and `the_wire_words_round_trip_and_are_distinct` holds the table whole.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            ArmingBlock::None => "none",
            ArmingBlock::PaperTier => "paper_tier",
            ArmingBlock::AccountInactive => "account_inactive",
            ArmingBlock::NoAccountRow => "no_account_row",
            ArmingBlock::TierConflict => "tier_conflict",
            ArmingBlock::NoAccountSymbol => "no_account_symbol",
            ArmingBlock::LiveConnectionUnproven => "live_connection_unproven",
            ArmingBlock::FeatureAbsent => "feature_absent",
            ArmingBlock::NoLiveArm => "no_live_arm",
            ArmingBlock::NoCredentials => "no_credentials",
            ArmingBlock::ExecFlagUnset => "exec_flag_unset",
            ArmingBlock::LiveSwitchOff => "live_switch_off",
            ArmingBlock::SdkAbsent => "sdk_absent",
            ArmingBlock::DemoOnlyArm => "demo_only_arm",
            ArmingBlock::LiveOnlyArm => "live_only_arm",
            ArmingBlock::LiveCredentialsAbsent => "live_credentials_absent",
            ArmingBlock::LiveTierNotWired => "live_tier_not_wired",
            ArmingBlock::NoAccountSupport => "no_account_support",
            ArmingBlock::AccountNotInStore => "account_not_in_store",
            ArmingBlock::SidecarHeldElsewhere => "sidecar_held_elsewhere",
            ArmingBlock::PreflightFailed => "preflight_failed",
            ArmingBlock::AdapterFailed => "adapter_failed",
            ArmingBlock::VenueUnreachable => "venue_unreachable",
            ArmingBlock::GatewayUnavailable => "gateway_unavailable",
            ArmingBlock::OrdersHeld => "orders_held",
            ArmingBlock::FeedsUnavailable => "feeds_unavailable",
            ArmingBlock::FxMarketClosed => "fx_market_closed",
        }
    }

    /// The block a wire word names, or `None` for a word this build does not know (a newer
    /// gatewayhub). **A caller maps `None` to [`Self::AdapterFailed`], never to a block that runs**
    /// (an unknown cause must not read as armed); the raw word belongs in the log beside it.
    #[must_use]
    pub fn from_word(word: &str) -> Option<ArmingBlock> {
        ArmingBlock::ALL.into_iter().find(|block| block.word() == word)
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

    /// **Whether this block says the account's VENUE is not connected**, rather than something about the account
    /// itself (owner 2026-11-14: "a strategy can trade only on CONNECTED venues; if a strategy trades a venue that is
    /// not connected, THAT STRATEGY is deactivated"; decision 0156's 2026-11-14 amendment). A strategy mount on such an
    /// account is DEACTIVATED and the node starts without it; it starts with the account. Every other block that does
    /// not run (an inactive row, a wrong tier, missing keys, a build without the venue) still refuses the node.
    ///
    /// The set: the FX market closed, the gatewayhub or the price path away, a venue session that did not come up,
    /// a venue that could not be reached, a preflight the venue failed, and fxcm's SDK not loaded on this box.
    #[must_use]
    pub fn venue_not_connected(self) -> bool {
        matches!(
            self,
            ArmingBlock::FxMarketClosed
                | ArmingBlock::GatewayUnavailable
                | ArmingBlock::FeedsUnavailable
                | ArmingBlock::AdapterFailed
                | ArmingBlock::VenueUnreachable
                | ArmingBlock::PreflightFailed
                | ArmingBlock::SdkAbsent
        )
    }
}

#[path = "arming_tests.rs"]
#[cfg(test)]
mod arming_tests;
