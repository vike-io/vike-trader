//! The per-ACCOUNT exec badge and each venue's arming disclosure (`CexArming` and its producers).

use std::collections::HashSet;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};

#[cfg(doc)]
use super::tier::cex_mainnet_enabled;
use super::tier::selected_tier;
use crate::CexVenue;

// -------------------------------------------------------------------------------------------
// The exec badge is per ACCOUNT, not per venue
// -------------------------------------------------------------------------------------------

/// The `exec` badge for a venue whose EVERY account is on the paper book.
pub(crate) const EXEC_PAPER: &str = "PAPER";

/// The `exec` badge for a venue whose start account armed real exec and which has no other.
pub(crate) const EXEC_LIVE: &str = "LIVE";

/// The `exec` badge for a venue whose START account is paper while a LABELLED account of it armed
/// real exec in this same process.
///
/// ⚠ It deliberately does not begin with `PAPER`. `docs/ops/tradehub-the CI box.md` teaches operators
/// to grep this field, and a badge spelled `PAPER (…)` would answer a `exec=PAPER` grep for a
/// venue that IS signing real orders from another account — the under-claim this constant exists
/// to end, wearing a longer string.
pub(crate) const EXEC_OTHER_ACCOUNT_LIVE: &str = "OTHER-ACCOUNT-LIVE";

/// The `exec` badge for a venue where the start account AND at least one labelled account armed.
/// Begins with `LIVE`, so an `exec=LIVE` grep still finds it.
pub(crate) const EXEC_LIVE_MULTI_ACCOUNT: &str = "LIVE-MULTI-ACCOUNT";

/// **Which of this venue's OTHER accounts armed real exec** — the route keys of every NON-mount
/// account of `venue` that appears in `assemble_runtime`'s `live_venues` record, sorted.
///
/// ⚠ **The membership test is `VenueArming::route_key`, and nothing here parses one.**
/// `live_venues` is keyed per ACCOUNT (`vike_tradehub_core::armed_live_venues`' own doc), and its labelled
/// entries are `venue#LABEL` — a spelling `vike_model::accounts::account_keys::AccountRef::route_key` owns.
/// Splitting those strings on `#` here would be a second reader of that grammar, and a second
/// reader is what goes stale when the first one changes; folding the ROWS through the same
/// renderer the start recorded under cannot.
///
/// "The mount's accounts" are `mount_accounts` (a mount naming none trades the default account, so
/// the caller passes [`AccountLabel::Default`] for it): the accounts the venue's mounts TRADE are
/// not "other", whichever label they carry (decision 0172).
///
/// EMPTY on a box with one account per venue — every row there is a default-account row, the one a
/// default-account mount trades — so every caller below is byte-identical there.
pub(crate) fn other_live_accounts(
    venue: &str,
    arming: &[vike_config::VenueArming],
    live_venues: &HashSet<String>,
    mount_accounts: &[AccountLabel],
) -> Vec<String> {
    let mounted = mount_routes(venue, mount_accounts);
    let mut out: Vec<String> = arming
        .iter()
        .filter(|row| row.venue == venue)
        .map(|row| row.route_key())
        .filter(|key| live_venues.contains(key) && !mounted.contains(key))
        .collect();
    out.sort();
    out
}

/// **The route keys of the accounts one venue's mounts TRADE** — `venue` for the default account,
/// `venue#LABEL` for a labelled one, rendered by `route_key_of`, the spelling `live_venues` is keyed
/// by. A mount naming no account trades the default one (the caller passes
/// [`AccountLabel::Default`]).
pub(crate) fn mount_routes(venue: &str, mount_accounts: &[AccountLabel]) -> Vec<String> {
    mount_accounts.iter().map(|label| route_key_of(venue, label)).collect()
}

/// **Whether any account the venue's mounts trade got a real exec client** in this process — the
/// fact every arm's `exec` badge is about. It used to ask `live_venues.contains(<bare venue>)`,
/// which is the DEFAULT account's key only: a mount on `bybit#DEMO` that really traded the demo
/// venue announced `EXEC IS PAPER` (decision 0172).
pub(crate) fn mounts_exec_live(
    venue: &str,
    mount_accounts: &[AccountLabel],
    live_venues: &HashSet<String>,
) -> bool {
    mount_routes(venue, mount_accounts).iter().any(|key| live_venues.contains(key))
}

/// **Which network the announcement says the mounts' exec side bound** — `planned` (the plan's
/// answer, the DEFAULT account's tier) for a default-account mount, and a labelled mount account's
/// OWN row tier ([`selected_tier`]) otherwise; MAINNET wins when they differ, as it does for the feed
/// (`feed_tier_from_rows`). The default account's tier is not a fact about `bybit#LIVE`: a line
/// saying `DEMO` beside a real-money mount is the worst direction to be wrong in.
pub(crate) fn mounts_on_mainnet(
    venue: &str,
    arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    planned: bool,
) -> bool {
    let default_mounted =
        mount_accounts.is_empty() || mount_accounts.iter().any(AccountLabel::is_default);
    let labelled_live = mount_accounts
        .iter()
        .filter(|label| !label.is_default())
        .filter_map(|label| arming.iter().find(|row| row.venue == venue && row.label == *label))
        .any(|row| selected_tier(row) == AccountTier::Live);
    (default_mounted && planned) || labelled_live
}

/// **The remedy for a mount on a LABELLED account.** The producers' remedies speak for the DEFAULT
/// account (`add_to_the_store`, `the_account_row`: an unlabelled row, `--no-label`), and applied to a
/// labelled mount they send the operator to create a SECOND, unlabelled account that arms nothing for
/// it (decision 0172). A mount on a labelled account gets the labelled account's own steps instead
/// (appended to the default remedy when the venue's mounts trade both). No remedy (live) and
/// default-only mounts return `base` unchanged.
pub(crate) fn with_labelled_mount_remedy(
    base: CexArming,
    venue: &str,
    mount_accounts: &[AccountLabel],
) -> CexArming {
    let labelled: Vec<String> =
        mount_accounts.iter().filter(|l| !l.is_default()).map(|l| format!("`{l}`")).collect();
    let Some(remedy) = base.remedy.as_deref().filter(|_| !labelled.is_empty()) else {
        return base;
    };
    let own = format!(
        "this mount trades {venue} account {names}, not the default account: `vike-cli secrets \
         accounts` lists its row and id; the row must be ACTIVE (`vike-cli secrets account \
         activate --id <N>`) at the tier you want (`vike-cli secrets account set-tier --id <N> \
         --tier <paper|demo|live>`), and the account must hold its OWN credential fields \
         (`vike-cli secrets set --venue {venue} --label <LABEL> <FIELD>`); an unlabelled account \
         is a different account and arms nothing for this mount",
        names = labelled.join(", ")
    );
    let remedy = if mount_accounts.iter().any(AccountLabel::is_default) {
        format!("{remedy}; and for the labelled account: {own}")
    } else {
        own
    };
    CexArming { remedy: Some(remedy), ..base }
}

/// The four-way `exec` badge, from the two facts that actually differ.
///
/// `default_live` is the MOUNT'S account's ([`mounts_exec_live`]: the default account for a mount
/// naming none, else the labelled account it names — the name predates labelled mounts). It is the
/// fact the LIVE/PAPER sentences below are about, and it stays the input to every `*_arming`
/// producer's remedy. `others_live` is the venue-wide fact the badge used to be blind to.
pub(crate) fn exec_badge(default_live: bool, others_live: bool) -> &'static str {
    match (default_live, others_live) {
        (true, false) => EXEC_LIVE,
        (true, true) => EXEC_LIVE_MULTI_ACCOUNT,
        (false, false) => EXEC_PAPER,
        (false, true) => EXEC_OTHER_ACCOUNT_LIVE,
    }
}

/// **Correct one venue's announcement for the accounts the venue-keyed badge could not see.**
///
/// The badge used to be `live_venues.contains(slug)` — the DEFAULT account's route key — so a box
/// whose only armed `bybit` account was a labelled one announced `exec = PAPER` beside
/// `NO ORDER FROM THIS START WILL EVER REACH THE VENUE`, while that process held a live
/// authenticated bybit session and signed real orders through it. Under-claiming, but false.
///
/// ⚠ `others` EMPTY returns `base` **unchanged**, field for field — which is every box with no
/// labelled account, so this function is a no-op there by construction rather than by care.
///
/// What it does NOT do is promote the PAPER *sentences* to live ones. The start's own orders
/// really do go to the paper book in that state (`RouteKey::sole_account_of`), so the remedy is
/// still the remedy for arming THIS start — it is merely prefixed with the fact that makes the
/// paper verdict a statement about one account rather than about the venue.
pub(crate) fn with_other_live_accounts(
    base: CexArming,
    venue: &str,
    others: Vec<String>,
) -> CexArming {
    if others.is_empty() {
        return base;
    }
    let default_live = base.exec == EXEC_LIVE;
    let named = others.join(", ");
    let remedy = base.remedy.map(|r| {
        format!(
            "⚠ {venue} is ALREADY LIVE in this process on another ACCOUNT ({named}) — the paper \
             verdict above is about the account THIS mount trades ({venue}), not about the venue, \
             and orders from that other account are REAL. To arm this mount too: {r}"
        )
    });
    CexArming { exec: exec_badge(default_live, true), other_live: others, remedy, ..base }
}

/// What the CEX arm ANNOUNCES at start: which network the exec side bound to, whether it is a real
/// venue client or the paper exchange, which maker verbs the pump drives, and — when it is PAPER —
/// the remedy that will ACTUALLY arm it.
///
/// (Since split-plane I9 the CREDENTIALED-DATA arms reuse this struct for the same disclosure —
/// [`alpaca_arming`], [`ctrader_arming`] and [`oanda_arming`] are the producers, each documenting
/// where its facts diverge from the CEX vocabulary. The field docs below speak CEX because that is
/// where the defects they record shipped.)
///
/// ⚠ Split out of the feed block and made PURE so it can be tested. `live_runtime` is a `main.rs`
/// function that spawns a core, opens two sockets and reads the credential store, so no test can
/// call it; before this existed, nothing exercised the block at all and both of the defects below
/// shipped inside it.
///
/// [`CexArming::remedy`] is the whole reason this is a function rather than two inline `format!`s —
/// see that field.
///
/// ⚠ Its `exec` badge is corrected for the venue's OTHER accounts by
/// [`with_other_live_accounts`] before it is announced — see [`other_live_accounts`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CexArming {
    /// What `assemble_runtime` ACTUALLY started, taken from its own `live_venues` record rather than
    /// from what was configured. One of the four [`exec_badge`] strings — `"LIVE"` / `"PAPER"` on
    /// a box with one account per venue, and never anything else there.
    pub exec: &'static str,
    /// `"MAINNET"` or `"DEMO"` (aster spells its non-mainnet network `"TESTNET"`, the venue's own
    /// word) — WHICH NETWORK the exec side bound to: the DEFAULT account's tier as the
    /// venue's registry row answers it ([`cex_mainnet_enabled`]; decision 0095 made the tier the
    /// network for binance/bybit/okx — since decision 0119 the ACCOUNT's own tier — and aster's row
    /// runs its credential chain under that tier).
    ///
    /// ⚠ The LIVE announcement used to omit this entirely: it said "orders from this start are REAL
    /// orders" without ever saying whether that meant the demo venue or real money. Both states are
    /// reachable and they differ by one environment variable.
    pub network: &'static str,
    /// The maker verbs this venue's pump actually drives. Every CEX venue here publishes quote,
    /// trade and book, so this is the same string for each — stated as a FIELD rather than assumed, because
    /// the claim it replaces was a per-venue one that was false (see [`CexVenue::quote_source`]).
    pub requote_lanes: &'static str,
    /// Where the L1 quote comes from — [`CexVenue::quote_source`].
    pub quote_source: &'static str,
    /// What the operator must change to arm real exec, or `None` when it is already LIVE.
    ///
    /// ⚠ **This is state-dependent, and the version it replaces was UNREACHABLE ADVICE.** The old
    /// PAPER branch said, unconditionally, "add the DEMO key set". But the network is the
    /// account's own tier (decision 0095, restated by 0119), and an account holds ONE credential
    /// set, at that one tier (decision 0156 item 6) — so for an account whose tier is `live`, a
    /// demo key set arms NOTHING (a live account never trades demo). An operator following that
    /// instruction gets the identical PAPER start and the identical warning, with no way to tell
    /// why. Naming the account's tier, the FIELDS it must hold and the account row that chose the
    /// tier is the difference between a warning and a wild goose chase.
    pub remedy: Option<String>,
    /// The route keys of this venue's OTHER accounts that armed real exec — [`other_live_accounts`].
    ///
    /// ⚠ **Always EMPTY out of the six producers**, and filled only by
    /// [`with_other_live_accounts`], which is where the venue-wide record is consulted. The
    /// producers answer from what THIS start resolved; a second account is not a fact any of them
    /// is holding, and giving each one the whole `live_venues` record so it could look would be six
    /// readers of a grammar with one owner.
    pub other_live: Vec<String>,
}

/// **WHERE a credential goes.** The remedy names the account's FIELDS and the VERB that writes
/// them: a credential belongs to its ACCOUNT and is written by field, never by a name (decision
/// 0156 item 6). `vike-cli secrets set --venue <VENUE> <FIELD>` writes the DEFAULT account's field
/// into the settings database, the only credential store, and `vike-cli secrets path` prints where
/// it is.
fn add_to_the_store(venue: &str, fields: &str) -> String {
    format!(
        "give this venue's DEFAULT account its {fields} fields — `vike-cli secrets set --venue \
         {venue} <FIELD>` writes each one into the settings database, the only credential store, \
         and `vike-cli secrets path` prints where it is"
    )
}

/// **WHICH account row arms it** (decision 0119): the account's own `tier` + `active` decide, so
/// the fields alone are not the whole remedy when the row they belong to is inactive, at another
/// tier, one of two active tiers — or absent: a credential write never creates an account or
/// chooses its tier (decision 0156 item 6), so the clause names the verb that does.
fn the_account_row(venue: &str, tier: &str) -> String {
    format!(
        "and that account row must be ACTIVE at tier `{tier}` — a credential write never creates \
         one: `vike-cli secrets account add --venue {venue} --tier {tier} --no-label` does; \
         `vike-cli secrets accounts` lists the rows and their ids, `vike-cli secrets account \
         activate --id <N>` / `set-tier --id <N> --tier {tier}` fixes one, and two ACTIVE \
         non-paper tiers for one account mount it PAPER until one is deactivated"
    )
}

/// Resolve [`CexArming`] from the three facts the start knows. Pure: no env, no store, no clock.
///
/// ⚠ **Aster diverges from the flag venues on every string below, and each divergence is a fact
/// about `make_engine`, not a style choice**: its non-mainnet network is spelled TESTNET (not
/// DEMO), its fields are the agent-wallet `USER`/`PRIVATE_KEY` pair (classic
/// `API_KEY`/`API_SECRET` was discontinued by the venue — `vike_aster::signing::load_aster_credentials`
/// is the field authority, `SIGNER` optional), and it has NO `{VENUE}_MAINNET` flag: the ACCOUNT's
/// tier is the network choice — a `live` account's fields arm mainnet and a `demo` account's
/// testnet, and an account holds ONE set (decisions 0095, 0119 and 0156 item 6). A remedy naming
/// `API_KEY` for aster or "UNSET ASTER_MAINNET" would be exactly the unreachable-advice defect this
/// function exists to prevent.
pub(crate) fn cex_arming(venue: CexVenue, mainnet: bool, exec_live: bool) -> CexArming {
    let slug = venue.slug();
    let upper = slug.to_uppercase();
    let is_aster = matches!(venue, CexVenue::Aster);
    // The account's FIELDS the venue's loader reads (`crates/vike-venues/src/credential_schema.rs`'s
    // `VENUE_KEY_SCHEMAS` row): the generic `API_KEY` / `API_SECRET`, plus `API_PASSPHRASE` where
    // the venue signs with one. OKX v5 requires the passphrase on every signed request; binance and
    // bybit use none, and naming a field that venue has no use for would send an operator looking
    // for a value that does not exist.
    let fields = if is_aster {
        "USER + PRIVATE_KEY (agent wallet; SIGNER optional)".to_string()
    } else {
        let mut f = "API_KEY + API_SECRET".to_string();
        if matches!(venue, CexVenue::Okx) {
            f.push_str(" + API_PASSPHRASE");
        }
        f
    };
    let remedy = match (exec_live, is_aster, mainnet) {
        (true, ..) => None,
        // A `live`-tier aster account holding an incomplete set: PAPER (`LiveCredentialsAbsent`),
        // never testnet — the account holds ONE set, at its ONE tier. What the remedy must say is
        // what completing it does: REAL-MONEY mainnet exec.
        (false, true, true) => Some(format!(
            "{store} — ⚠ this aster account's tier is `live`, so those fields arm REAL-MONEY \
             MAINNET exec and nothing else (the account holds ONE key set at its ONE tier, and a \
             live account never falls back to another tier's keys); to mount aster on testnet \
             instead: \
             `vike-cli secrets account set-tier --id <N> --tier demo` (`vike-cli secrets \
             accounts` lists the ids)",
            store = add_to_the_store(slug, &fields),
        )),
        // Below `live` the aster account's fields arm testnet. What the remedy must still say,
        // prominently, is how the OTHER network gets armed — because the account's tier alone
        // chooses it, and that is a real-money decision.
        (false, true, false) => Some(format!(
            "{store}, {row} — ⚠ aster has NO {upper}_MAINNET flag: the account's TIER is the \
             network choice. `vike-cli secrets account set-tier --id <N> --tier live` (or a \
             `live` account created with `account add --venue {slug} --tier live`) arms \
             REAL-MONEY MAINNET exec at the next restart",
            store = add_to_the_store(slug, &fields),
            row = the_account_row(slug, "demo"),
        )),
        (false, false, true) => Some(format!(
            "{store} — ⚠ this account's tier is `live`, so those fields arm MAINNET exec and \
             nothing else (an account holds ONE credential set, at its ONE tier: a live account \
             never trades demo); to mount this venue on DEMO instead: \
             `vike-cli secrets account set-tier --id <N> --tier demo` (`vike-cli secrets \
             accounts` lists the ids)",
            store = add_to_the_store(slug, &fields),
        )),
        (false, false, false) => Some(format!(
            "{store}, {row}",
            store = add_to_the_store(slug, &fields),
            row = the_account_row(slug, "demo")
        )),
    };
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: if mainnet {
            "MAINNET"
        } else if is_aster {
            "TESTNET"
        } else {
            "DEMO"
        },
        requote_lanes: "on_quote_tick + on_order_book",
        quote_source: venue.quote_source(),
        remedy,
    }
}

/// The arming disclosure for the ALPACA arm — the credentialed-data reuse of [`CexArming`]
/// (split-plane I9). Pure, like [`cex_arming`], so the announcement's claims are testable without
/// a socket. Every field is an alpaca FACT, not a style choice:
///
/// - `network` is always `"SANDBOX"`: the exec start (`crates/bridges/alpaca/src/adapter.rs`'s
///   `AlpacaVenueAdapter`) arms only a `demo` account and loads its config at `AccountTier::Demo`
///   unconditionally — there is no `ALPACA_MAINNET` flag, and a `live` account holding its trio
///   does NOT RUN (`LiveTierNotWired`): a LIVE flip is a deliberate code change. (Alpaca says
///   "sandbox", not "demo" — `vike_alpaca::hosts_for(AccountTier::Demo)` is the sandbox host table —
///   so the log speaks the venue's own word for the network.)
/// - `requote_lanes` is `on_quote_tick` ONLY: alpaca declares no book lane
///   (`live_data.book = false` — `subscribe_book` is a caps refusal), so the maker's
///   `on_order_book` verb never fires here. Stated because assuming the CEX pair would be the
///   false-claim shape [`CexVenue::quote_source`]'s doc records.
/// - the PAPER remedy exists for honesty but names its own near-unreachability: [`crate::venue_plan::alpaca_plan`]
///   already refused the start when the default `demo` account's trio was absent, and with the
///   trio present `AlpacaExecutionClient::spawn` is INFALLIBLE at start (auth failures surface
///   later, in the actor) — so `exec = PAPER` here means the exec gate and the feed gate READ
///   DIFFERENT ANSWERS from one account, which is a bug to report, not a field to add. ⚠ ONE
///   DECLARED EXCEPTION: a profile with `data_only = true` makes the two gates read different
///   credentials ON PURPOSE ([`super::withhold_data_only_credentials`]) — the wire arm then
///   announces the declaration through [`data_only_arming`] instead of this remedy, so the "bug to
///   report" text is never printed for a state the operator asked for.
pub(crate) fn alpaca_arming(exec_live: bool) -> CexArming {
    let upper = "alpaca".to_uppercase();
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "SANDBOX",
        requote_lanes: "on_quote_tick",
        quote_source: "IEX equities WS (native L1 quote channel, SANDBOX data hosts)",
        remedy: (!exec_live).then(|| {
            format!(
                "the feed gate resolved the DEFAULT {upper} account's CLIENT_ID/CLIENT_SECRET/\
                 ACCOUNT_ID trio (this mount could not have started otherwise) yet `assemble_runtime` \
                 recorded no live {upper} exec — the two gates read ONE account, so this state is \
                 a daemon bug to report, not a missing key; orders go to the paper book until it \
                 is resolved"
            )
        }),
    }
}

/// The arming disclosure for the CTRADER arm (split-plane I9) — same [`CexArming`] reuse and the
/// same pure-function argument as [`alpaca_arming`]. The ctrader facts:
///
/// - `network` is always `"DEMO"`: cTrader's start (`crates/bridges/ctrader/src/adapter.rs`'s
///   `CtraderVenueAdapter`) arms only a `demo` account's grant — no `CTRADER_MAINNET` flag exists,
///   and a `live` account does NOT RUN (`LiveTierNotWired`): the live host is a deliberate code
///   change.
/// - `requote_lanes` is `on_quote_tick` ONLY (no trade-print stream, no book lane — both are caps
///   refusals on `CtraderData`).
/// - the PAPER remedy IS reachable, and differently from every credential remedy above:
///   [`crate::venue_plan::ctrader_plan`] proved the DEMO tokens present, but ctrader's exec client connects
///   SYNCHRONOUSLY inside `make_engine` and a connect/auth failure DEMOTES that venue to paper
///   for the session (dropping its recon handle) — while this feed arm's own, later data connect
///   can still have succeeded. The remedy is therefore a RESTART (retry the exec handshake), not
///   a key. ⚠ ONE DECLARED EXCEPTION: under a `data_only = true` profile no handshake was even
///   attempted ([`super::withhold_data_only_credentials`] kept the tokens from `make_engine`) and
///   a restart would change nothing — the wire arm announces the declaration through
///   [`data_only_arming`] instead of this remedy.
pub(crate) fn ctrader_arming(exec_live: bool) -> CexArming {
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "DEMO",
        requote_lanes: "on_quote_tick",
        quote_source: "SPOT_EVENT bid/ask over the dedicated protobuf socket (native L1 channel, \
                       demo host)",
        remedy: (!exec_live).then(|| {
            "the DEMO tokens are present (this feed's own socket authenticated with them), but \
             ctrader exec connects SYNCHRONOUSLY at mount and its connect/auth FAILED, so \
             `make_engine` demoted ctrader to PAPER for this session and dropped its recon \
             handle — restart the daemon to retry the exec handshake; until then no order from \
             this mount reaches the venue"
                .to_string()
        }),
    }
}

/// The arming disclosure for the OANDA arm (split-plane I9) — the same [`CexArming`] reuse and the
/// same pure-function argument as [`alpaca_arming`]. The oanda facts:
///
/// - `network` is always `"PRACTICE"`, which is the VENUE's own word for the environment
///   `vike_oanda::adapter::OandaVenueAdapter` binds when it arms: it arms only a `demo` account, and
///   `vike_oanda::oanda_hosts(Demo)` is the `api-fxpractice` / `stream-fxpractice` pair.
///   There is no `OANDA_MAINNET` flag; the fxTrade tier and its hosts are implemented and tested
///   with NO caller anywhere in this workspace (`crates/bridges/oanda/CLAUDE.md` records that as a
///   live trap), so a live flip is a deliberate code change rather than a config one.
///   ⚠ The ACCOUNT's tier is `demo` while the NETWORK is spelled practice — one environment, two
///   vocabularies. The disclosure says `PRACTICE` because the field answers "which network did
///   exec bind", and [`CexArming::remedy`] names the fields, so neither word has to carry the
///   other's job.
/// - `requote_lanes` is `on_quote_tick` ONLY: oanda declares no book lane and no trade tape
///   (`live_data.book`/`.trades` are both false — `subscribe_book`/`subscribe_trades` are caps
///   refusals), so the maker's `on_order_book` verb never fires here. Claiming the CEX pair would
///   be the false-lanes defect [`CexVenue::quote_source`]'s doc records.
/// - the PAPER remedy is alpaca's, not ctrader's, and for alpaca's reason: [`crate::venue_plan::oanda_plan`] already
///   refused the start when the token/account pair was absent, and with them present
///   `OandaExecutionClient::spawn` is INFALLIBLE at start (a bad token surfaces later, inside the
///   exec thread's REST calls) — so `exec = PAPER` here means the feed gate and the exec gate read
///   DIFFERENT answers out of one account, which is a bug to report rather than a field to add.
///   ⚠ ONE DECLARED EXCEPTION: a `data_only = true` profile makes the gates read different
///   credentials ON PURPOSE ([`super::withhold_data_only_credentials`]) — the wire arm then
///   announces the declaration through [`data_only_arming`] instead of this remedy.
pub(crate) fn oanda_arming(exec_live: bool) -> CexArming {
    let upper = "oanda".to_uppercase();
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "PRACTICE",
        requote_lanes: "on_quote_tick",
        quote_source: "/pricing/stream chunked-HTTP line stream (native L1 bid/ask, fxPractice \
                       stream host; no market-data WS exists on this venue)",
        remedy: (!exec_live).then(|| {
            format!(
                "the feed gate resolved the DEFAULT {upper} account's API_KEY/ACCOUNT_ID pair \
                 (this mount could not have started otherwise) yet `assemble_runtime` recorded no live \
                 {upper} exec — the two gates read ONE account, so this state is a daemon bug to \
                 report, not a missing key; orders go to the paper book until it is resolved"
            )
        }),
    }
}

/// The arming disclosure for the DERIBIT arm (split-plane I9) — the same [`CexArming`] reuse and
/// the same pure-function argument as [`cex_arming`], but every field diverges from the CEX
/// vocabulary and each divergence is a `vike_tradehub_core::make_engine` fact:
///
/// - `network` is always `"TESTNET"`, and unlike every venue above it is not a resolution at all.
///   Deribit was never one of decision 0095's four tier-selects-mainnet venues
///   (binance/bybit/okx/hyperliquid only), so no `DERIBIT_MAINNET` read exists anywhere; `cex_mainnet_enabled` has no deribit arm so
///   `make_engine` arms only a `demo` account, and every authed socket
///   `crates/bridges/deribit/src/exec.rs` opens is the hardcoded `TESTNET_WS`/`TESTNET_REST` pair.
///   `transport.rs`'s `MAINNET_REST` is referenced by nothing in the workspace: there is no path
///   to deribit MAINNET execution in this tree, by flag or by account tier, so a `live` deribit
///   account holding its keys answers paper with `PaperCause::LiveTierNotWired`
///   (`crates/bridges/deribit/CLAUDE.md`'s two-networks section is the authority).
///   ⚠ The ACCOUNT's tier is `demo` while the NETWORK is TESTNET — one environment, two
///   vocabularies, exactly the split [`oanda_arming`] documents. The field answers "which network
///   did exec bind"; [`CexArming::remedy`] names the fields.
/// - `requote_lanes` is the full CEX pair, and here it is EARNED rather than inherited:
///   `vike_venues::venue_caps::DERIBIT` declares `book: true` and this arm really subscribes the
///   lossless `book.{inst}.100ms` lane onto `LiveDataSink::book`, so `on_order_book` genuinely
///   fires. (`depth: false` is the row that stays refused — see [`crate::feeds::wire_venue_feeds`]' arm.)
/// - `quote_source` names the MAINNET public host, because the two halves of this venue point at
///   different networks and a disclosure that said only "TESTNET" would be a half-truth about
///   where the prices come from.
/// - the PAPER remedy is the CEX one — give the default `demo` account its `API_KEY` +
///   `API_SECRET` fields — and it is fully reachable here, unlike the credentialed-data arms'
///   "report a bug": nothing gated the feed on credentials, so a keyless deribit start with no
///   keys in the store is the ORDINARY unconfigured state. There is no `{VENUE}_MAINNET` caveat to
///   attach, because the flag does not exist for this venue — naming one would be the
///   unreachable-advice defect [`CexArming::remedy`] records.
pub(crate) fn deribit_arming(exec_live: bool) -> CexArming {
    let slug = "deribit";
    let upper = slug.to_uppercase();
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "TESTNET",
        requote_lanes: "on_quote_tick + on_order_book",
        quote_source: "quote.{instrument} + book.{instrument}.100ms JSON-RPC channels (native \
                       venue-throttled L1 and a change_id-chained L2 book, KEYLESS public MAINNET \
                       host — the network exec does NOT bind)",
        remedy: (!exec_live).then(|| {
            format!(
                "{store}, {row} — ⚠ this venue has NO {upper}_MAINNET flag and no mainnet exec \
                 path at all, so those fields arm TESTNET exec (test.deribit.com) while the feed \
                 above keeps reading the MAINNET book",
                store = add_to_the_store(slug, "API_KEY + API_SECRET"),
                row = the_account_row(slug, "demo"),
            )
        }),
    }
}

/// The arming disclosure for the IG arm (split-plane I9) — the same [`CexArming`] reuse and the
/// same pure-function argument as [`alpaca_arming`]. The IG facts:
///
/// - `network` is always `"DEMO"`, which is both the account's tier and the gateway:
///   `vike_ig::adapter::IgVenueAdapter` arms only a `demo` account, through
///   `load_ig_config_from(Demo, …)`, and `vike_ig::ig_rest_base(Demo)` is the demo dealing
///   gateway. No `IG_MAINNET` flag exists — IG was never one of decision 0095's four
///   tier-selects-mainnet venues — and `ig_rest_base(Live)` has an implementation with NO caller
///   anywhere in this workspace — so a `live` IG account holding its trio does NOT RUN
///   (`LiveTierNotWired`, `crates/bridges/ig/CLAUDE.md` records that trap). Unlike oanda there is
///   no second vocabulary here: the tier and the network are the same word.
/// - `requote_lanes` is `on_quote_tick` ONLY, and this one is STRUCTURAL rather than a wiring
///   choice: `vike_venues::venue_caps::IG` declares `trades: false` and `book: false` because IG is
///   a DEALER venue publishing its own two-sided price — there is no public tape and no L2 ladder
///   to subscribe. It is also why IG stays DEFERRED in
///   `crates/vike-bridge-core/tests/market_data_conformance.rs` even now that it has a live feed:
///   that harness checks three L2-BOOK invariants and there is no book here for them to hold on.
///   Claiming the CEX pair would be the false-lanes defect [`CexVenue::quote_source`]'s doc
///   records.
/// - the PAPER remedy is alpaca's, not ctrader's, and for alpaca's reason: [`crate::venue_plan::ig_plan`] already
///   refused the start when the trio was absent, and with it present `IgExecutionClient::spawn` is
///   INFALLIBLE at start (its own module doc says so — an `ExecActor` plus a background stream
///   thread, each self-gating on its own login, with no blocking startup handshake and therefore
///   no connect-failure paper demotion). So `exec = PAPER` here means the feed gate and the exec
///   gate read DIFFERENT answers out of one account, which is a bug to report rather than a field
///   to add. ⚠ ONE DECLARED EXCEPTION: a `data_only = true` profile makes the gates read
///   different credentials ON PURPOSE ([`super::withhold_data_only_credentials`]) — the wire arm
///   then announces the declaration through [`data_only_arming`] instead of this remedy.
pub(crate) fn ig_arming(exec_live: bool) -> CexArming {
    let upper = "ig".to_uppercase();
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "DEMO",
        requote_lanes: "on_quote_tick",
        quote_source: "PRICE:{accountId}:{epic} Lightstreamer TLCP MERGE item on the Pricing data \
                       adapter (the dealer's own two-sided top-of-ladder bid/ask, demo gateway; this \
                       venue publishes no depth and no trade tape)",
        remedy: (!exec_live).then(|| {
            format!(
                "the feed gate resolved the DEFAULT {upper} account's API_KEY/IDENTIFIER/PASSWORD \
                 trio (this mount could not have started otherwise) yet `assemble_runtime` recorded no \
                 live {upper} exec — the two gates read ONE account, so this state is a daemon \
                 bug to report, not a missing key; orders go to the paper book until it is resolved"
            )
        }),
    }
}

/// The arming disclosure for a DECLARED data-plane-only start (the `data_only` profile key) —
/// the venue's own PAPER disclosure with ONLY the remedy replaced: `base` is that venue's
/// `*_arming(false)` (so `network`/`requote_lanes`/`quote_source` stay the venue facts their own
/// functions own, never a second copy), and the remedy names the DECLARATION instead of the
/// venue's ordinary paper cause.
///
/// Exists because every credentialed-data venue's ordinary paper remedy is FALSE advice on the
/// declared path: alpaca/oanda/ig say "the two gates read ONE account, so this state is a daemon
/// bug to report" — but on a declared start the gates deliberately read DIFFERENT credentials (the
/// plan resolved before [`super::withhold_data_only_credentials`] ran) and the state is exactly
/// what the operator asked for; ctrader says "restart the daemon to retry the exec handshake" —
/// but no handshake failed and a restart repeats the withhold. Pure, like [`cex_arming`], so the
/// announcement's claims are testable without a socket.
pub(crate) fn data_only_arming(base: CexArming, venue: &str) -> CexArming {
    CexArming {
        remedy: Some(format!(
            "this profile DECLARES `data_only = true` for {venue}: the daemon WITHHELD every \
             {venue} account's credentials from the exec mount, so the workspace live gate (absent \
             credentials ⇒ paper) kept exec on the paper book BY DECLARATION while the \
             credentialed feed above authenticates from the same store — a data-only mount, not a \
             bug; remove `data_only` from the profile to arm demo exec from those credentials"
        )),
        ..base
    }
}
