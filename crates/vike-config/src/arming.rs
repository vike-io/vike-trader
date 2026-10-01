//! `arming` — the credential file may not ARM REAL MONEY.
//!
//! `<project>/settings/secrets.env` is a plaintext `KEY=value` file, and `parse_dotenv` builds its
//! map with `out.insert(..)` — LAST WINS. So an attacker who can APPEND one line to it, and who
//! never needs to READ it, could flip a venue from demo to mainnet, or turn Polymarket order
//! placement on. That is a privilege escalation from "can write a config file" to "trades real
//! money on the operator's account", and the two capabilities are not the same: an append needs no
//! read permission, leaves the existing credentials untouched, and survives review precisely
//! because nobody re-reads a credentials file.
//!
//! ## What this does NOT change
//!
//! This section used to describe `vike_bridge_core::mainnet`, a signed-off, machine-gated
//! convergence that read every `{VENUE}_MAINNET` flag from BOTH the process env and the workspace
//! credential map — closing the repo-wide "the credential file is never exported to the process env"
//! trap, so a flag written in the file where every other venue setting lives did not silently parse
//! as unset. **Decision 0095 deleted that mechanism along with the flag it converged.** For
//! binance/bybit/okx/hyperliquid the arming CEILING (`policy.venues.<venue>`) alone chooses the
//! network now, and `{VENUE}_MAINNET` arms nothing at ANY layer any more — not the process
//! environment ([`crate::refuse_removed_env`]'s job), and not this file. Its four rows stay in
//! [`CREDENTIAL_FILE_ARMING_REFUSED`] anyway: a line left over from before the migration is refused
//! rather than left looking like live configuration — the same argument this module makes for every
//! other row, now true of these four for a different reason than it used to be.
//!
//! The two Polymarket rows (`POLY_EXEC`/`POLY_RECONCILE`) never went through
//! `vike_bridge_core::mainnet` and are untouched by decision 0095 — they still arm exactly as
//! before, and everything below still applies to them.
//!
//! ## Why refuse rather than ignore
//!
//! Ignoring the file's value and dropping to demo would be the other fail-safe option, and it is
//! worse. An operator who wrote `POLY_EXEC=1` believes they are armed; a build that quietly
//! dropped to demo would have them reading demo fills as real ones. Silently changing which venue
//! tier you trade on is worse than either behaviour alone — so the process names the variable and
//! stops, and the operator moves it one file over, into the real process env this row's flag still
//! reads. ⚠ That remedy does not exist for the four retired `{VENUE}_MAINNET` rows above:
//! decision 0095 refuses them at EVERY layer now, process env included, so their remedy is
//! `policy.venues.<venue>`, not a different file to move the line to.
//!
//! ## Residual, stated rather than implied
//!
//! This is a ROOT-level check, so it binds only the composition roots that call it. Three exposures
//! it deliberately does NOT cover, because each needs its own change:
//!
//! - **Credential PRESENCE as the live gate.** aster's mount (`AsterVenueMount` in
//!   `crates/bridges/aster/src/mount.rs`) tries `Environment::Live` FIRST and falls back to Demo,
//!   so `ASTER_LIVE_*` keys in the file arm mainnet with no flag involved at all. Aster was always switchless — decision 0095's migration
//!   touches only binance/bybit/okx/hyperliquid
//!   (`vike_secrets::live_means_mainnet::SWITCHED_VENUES`) — so there was never an `ASTER_MAINNET`
//!   to refuse, before or after it. Same shape for the legacy `*_MAINNET_*` credential-name aliases
//!   `Environment::legacy_str` accepts.
//! - **Libraries that load the credential file THEMSELVES**, below any root: whatever file
//!   `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` still lists reads the
//!   store with no caller-supplied map passing through here.
//!   (The Polymarket bridge's two such readers went with decision 0095: the rate gate's `.env`
//!   reader, `dotenv_rate_gate` — the gate is map-only now, folded from
//!   `venue.polymarket.rate_gate` — and the chain watcher's `dotenv_chain_vars`, whose
//!   `POLY_CHAIN_WATCH` is a parameter of code nothing starts now.)
//! - A root that never calls [`refuse_credential_file_arming`] is unbound by it.
//!
//! ## The SECOND question this module answers, and why one table could not answer it
//!
//! [`armed_settings_in`] asks the same table of the PROCESS ENVIRONMENT — "is this box armed for
//! live?" — which is what `vike-cli config check` uses to decide whether an UNREADABLE credential
//! store is the degrade `docs/decisions/0013-degrade-vs-refuse.md` records or the false belief it
//! refuses on.
//!
//! That question had an answer the table alone cannot give, and [`armed_for_live`] is why this
//! module now has two sources instead of one. [`CREDENTIAL_FILE_ARMING_REFUSED`] is VENUE-SCOPED by
//! construction: every row names one venue in its own variable, and only the four venues
//! `vike_secrets::live_means_mainnet::SWITCHED_VENUES` names — the ones decision 0095's ceiling now
//! chooses the network for — have such a variable at all, and even those four arm nothing any more
//! (see above). The every OTHER roster venue picks its tier from the credential PREFIX (`DERIBIT_LIVE_*` vs
//! `DERIBIT_DEMO_*`), from which gateway it logs into (ibkr), or not at all because it is
//! mainnet-only (polymarket) — so a box live on one of them read as UNARMED and its unreadable store
//! took the degrade — silently starting all-paper while every surface said live, which is the
//! set-but-unhonoured false belief the refusal exists to catch.
//!
//! ⚠ This paragraph used to name that set as "the other nine … deribit, oanda, ig, fxcm, dukascopy,
//! ctrader, alpaca, ibkr, aster". It was a hand-written roster and it had already rotted — the set
//! is TEN and the missing one was polymarket. `crates/vike-bridge-core/src/mainnet.rs`'s
//! `every_roster_venue_is_classified` PINNED both partitions and asserted they summed to
//! `vike_model::VENUES`, until decision 0095 deleted the module along with the four-venue
//! classification it pinned; `vike_secrets::live_means_mainnet::SWITCHED_VENUES` is what remains of
//! the SWITCHED half. Do not restate either list here regardless; a roster in prose is what the
//! root `CLAUDE.md` forbids, for this reason.
//!
//! ⚠ **Widening the table cannot fix that, and the reason is worth stating once.** For a switchless
//! venue "am I armed for live" is answerable only from the credential prefixes — inside the very
//! file that could not be read. The signal and the failure are THE SAME OBJECT, so any repair has
//! to come from a source that is independent of the store.
//!
//! [`TRADEHUB_LIVE_ARMING`] is that source. `flags.tradehub_live` — `<project>/settings/flags.toml`
//! or `VIKE_TRADEHUB_LIVE`, resolved by [`fn@crate::load`] like any other setting — is the headless
//! daemon's live master gate, and it is **NODE-scoped**: it selects `crates/vike-tradehub/src/tradehub_cli.rs`'s
//! `live_mount` over the paper one, and that mount is `crates/vike-mount/src/node.rs`'s `build_node`,
//! which issues a `make_engine` call for EVERY venue in `WIRED_MARKETS` and takes each one live iff
//! its credentials resolve — switched and switchless alike. So it sees the nine the table cannot, it
//! lives outside the credential store, and it is read by the daemon itself
//! (`crate::CONSUMPTION`'s `flags.tradehub_live` row names `crates/vike-tradehub/src/tradehub_cli.rs`) —
//! which is what makes it evidence about this box rather than a declaration nobody honours.
//!
//! ### ⚠ The run profile's `venue` was the obvious second source, and it is the WRONG one
//!
//! It is the source that suggests itself, because a profile plainly names a venue
//! (`venue = "bybit"`), and it UNDER-COUNTS by construction. `build_node` does not mount the
//! profile's venue — it mounts the whole `WIRED_MARKETS` table and returns `Node::live_venues`, "the
//! set of venues with a credential-gated LIVE exec client". The profile's venue selects which pair
//! the STRATEGY trades, not which venues authenticate.
//!
//! MEASURED on the CI box (2026-08-09, read-only): `settings/tradehub.toml` and `settings/run-live.toml`
//! both name `bybit`, and the store holds `BYBIT_DEMO_*` only — but a `DERIBIT_LIVE_*` pair appended
//! to that same store would be taken live by the same mount, on the same start, with no profile
//! edit. A check keyed on the profile's venue would report a deribit-live box as bybit-only. So the
//! profile is not consulted here at all, and no `--profile` flag was added to the verb: a second
//! input that answers a NARROWER question than the one already available is a way to be wrong with
//! more ceremony.
//!
//! ### ⚠ `flags.poly_exec` / `flags.poly_reconcile` WERE excluded, and are not any more
//!
//! This bullet used to say they were "deliberately NOT folded in", on one stated fact: neither was
//! consumed, so a `poly_exec = true` line in `flags.toml` armed NOTHING, and refusing a box over a
//! setting with no effect is how a check teaches people to work around it. That fact is dead. The
//! unread-settings sweep wired both — `crate::CONSUMPTION` names
//! `crates/vike-tradehub/src/tradehub_cli.rs` for each, where the resolved flags are folded into the
//! map the Polymarket mount reads — so a file line now arms a real exec mount and excluding it would
//! be a hole in exactly the direction this module exists to close: armed from a file, reported
//! UNARMED, degrading past an unreadable store.
//!
//! [`POLY_FILE_ARMING`] is the fold, and the exclusion was gated rather than written down precisely
//! so this would be noticed: `the_polymarket_flags_are_excluded_because_the_file_layer_arms_nothing`
//! FAILED on the commit that wired them, and is now
//! `the_polymarket_flags_are_file_evidence_now`, asserting the opposite fact the same way.
//!
//! Decision 0095 made the rows the ONLY source: the variables refuse startup, so on a box that
//! starts at all the file evidence is the whole of this venue's arming.
//!
//! ### One more thing deliberately NOT folded in
//!
//! - **`VIKE_TRADEHUB_LIVE` as a [`CREDENTIAL_FILE_ARMING_REFUSED`] row.** That table refuses a line
//!   in the credential FILE, and `vike-tradehub` resolves this flag through [`fn@crate::load`] over its
//!   own `std::env::vars()` sweep — the store is never merged in, so a `VIKE_TRADEHUB_LIVE=1` line
//!   in `secrets.env` arms nothing, and refusing it would fire on a harmless line.
//!
//! ### ⚠ "I could not tell" must never be spelled "not armed"
//!
//! The node-scoped source is a RESOLVED setting, so a caller that could not resolve the settings
//! tree has no answer — and the failure mode being closed here is precisely a box reading UNARMED
//! for want of a signal. Answering that case `Unarmed` would rebuild the same hole one level up, so
//! it is not an available answer: [`LiveArmingVerdict`] has a third variant,
//! [`LiveArmingVerdict::Undetermined`], and [`LiveArmingVerdict::refuses`] is true for it.
//!
//! ### What this still does NOT see
//!
//! Nothing the GUI does, any more. `vike-app` had no live gate at all — a venue mounted live iff
//! its credentials were present, so on that box "armed for live" WAS "the store has credentials"
//! and an unreadable store left the question unanswerable — and the residual was tolerated because
//! it was an interactive program rather than an unattended unit. `vike-desktop` mounts no venue
//! since the desktop lost its local core (#1610), so the case is gone rather than closed. (This
//! paragraph described it in the present tense until 2026-09-28.)

use std::collections::HashMap;

use crate::flags::Flags;

/// One variable that must not ARM from the credential file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmingSetting {
    /// The variable name, as written in the file.
    pub var: &'static str,
    /// What an armed value turns on, in the operator's own terms — this is what the refusal message
    /// leads with, because "BINANCE_MAINNET is set" is not why they should care.
    pub arms: &'static str,
    /// The remedy line the refusal prints for an operator who meant what the row says.
    pub instead: &'static str,
}

/// Every variable whose presence in the credential file is refused, because an armed value there
/// turns PAPER or DEMO into REAL MONEY.
///
/// The set is deliberately the ARMING gates only, not every flag that is map-readable. A knob that
/// cannot by itself cause a real order — `POLY_PRESUBMIT_REGISTER` (a within-exec behaviour toggle
/// that does nothing unless `POLY_EXEC` already armed), `POLY_HEARTBEAT` — is not refused, because
/// a refusal list that fires on harmless lines trains operators to work around it.
///
/// `{VENUE}_MAINNET` rows are exactly the four venues decision 0095's migration rewrites —
/// `vike_secrets::live_means_mainnet::SWITCHED_VENUES` — and now arm nothing at any layer; they stay
/// so a stale row is refused rather than looking like configuration. Every other roster venue never
/// had such a flag — notably **aster**, whose tier is chosen by which credential PREFIX is present
/// (`ASTER_LIVE_*` vs `ASTER_TESTNET_*`), so there is nothing named `ASTER_MAINNET` for this table
/// to hold (see this module's residual note).
pub const CREDENTIAL_FILE_ARMING_REFUSED: &[ArmingSetting] = &[
    ArmingSetting {
        var: "BINANCE_MAINNET",
        arms: "nothing any more — decision 0095 deleted this switch; the ceiling \
               `policy.venues.binance` alone chooses the network",
        instead: "vike-cli config set policy.venues.binance live",
    },
    ArmingSetting {
        var: "BYBIT_MAINNET",
        arms: "nothing any more — decision 0095 deleted this switch; the ceiling \
               `policy.venues.bybit` alone chooses the network",
        instead: "vike-cli config set policy.venues.bybit live",
    },
    ArmingSetting {
        var: "OKX_MAINNET",
        arms: "nothing any more — decision 0095 deleted this switch; the ceiling \
               `policy.venues.okx` alone chooses the network",
        instead: "vike-cli config set policy.venues.okx live",
    },
    ArmingSetting {
        var: "HYPERLIQUID_MAINNET",
        arms: "nothing any more — decision 0095 deleted this switch; the ceiling \
               `policy.venues.hyperliquid` alone chooses the network",
        instead: "vike-cli config set policy.venues.hyperliquid live",
    },
    ArmingSetting {
        var: "POLY_EXEC",
        arms: "Polymarket LIVE order placement — the venue has no testnet, so every order it \
               mounts is real money on Polygon mainnet",
        instead: "vike-cli config set flags.poly_exec true",
    },
    ArmingSetting {
        var: "POLY_RECONCILE",
        arms: "Polymarket reconcile — authenticated MAINNET account reads",
        instead: "vike-cli config set flags.poly_reconcile true",
    },
];

/// The value grammar an arming line uses: the EXACT string `"1"`, after stripping the trailing
/// `# comment` the real credential file annotates its lines with and trimming whitespace.
///
/// Mirrors `vike_polymarket::config::first_token` (the map-side normalisation the Polymarket gates
/// apply) UNION the plain `== "1"` the `{VENUE}_MAINNET` fold uses, so this check cannot be evaded
/// by a spelling one of the real readers accepts. Anything else — `0`, empty, `true` — arms nothing
/// at any reader and is therefore not refused: refusing a disarming line would be noise.
fn is_arming(raw: &str) -> bool {
    raw.split('#').next().unwrap_or("").trim() == "1"
}

/// Every [`CREDENTIAL_FILE_ARMING_REFUSED`] row the given map ARMS, in table order — empty in the
/// overwhelmingly common case.
///
/// **The map is whatever the caller is asking ABOUT, and there are two questions.**
/// [`refuse_credential_file_arming`] asks it of the CREDENTIAL FILE, where an arming value is
/// refused outright (this module's whole subject). A pre-flight check asks the same table of the
/// PROCESS ENVIRONMENT — "is this box armed for live?" — which is the one place the arming rule
/// still lets these turn paper into real money, and therefore the honest signal for whether an
/// operator believes a live venue is in force.
///
/// One table and one value grammar for both, deliberately: a second, hand-maintained list of "the
/// flags that mean live" is exactly the hand copy this repo keeps having to gate
/// (`crates/vike-ops/tests/settings_registry.rs`'s ratchets are the same shape), and it would rot
/// the moment a venue's switch is added here alone.
pub fn armed_settings_in(vars: &HashMap<String, String>) -> Vec<&'static ArmingSetting> {
    CREDENTIAL_FILE_ARMING_REFUSED
        .iter()
        .filter(|s| vars.get(s.var).is_some_and(|v| is_arming(v)))
        .collect()
}

// -------------------------------------------------------------------------------------------
// "Is this box armed for live?" — the union of the venue-scoped table and the node-scoped flag
// -------------------------------------------------------------------------------------------

/// How many venues one piece of arming evidence can speak for.
///
/// The distinction is the whole reason [`armed_for_live`] exists rather than just
/// [`armed_settings_in`], so it is a TYPE and not a comment: a report that says "armed" without
/// saying whether the evidence covers one venue or the whole mount cannot tell an operator whether
/// the absence of evidence means anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmingScope {
    /// ONE venue, named in the variable itself (`BINANCE_MAINNET`, `POLY_EXEC`). Silence about a
    /// venue with no such variable — the nine SWITCHLESS ones — is silence, not a `no`.
    Venue,
    /// The whole NODE: every venue the mount would take live. Silence here IS a `no`, because the
    /// gate is a single resolved setting rather than a per-venue variable that may not exist.
    Node,
}

/// One reason to believe this box intends to trade LIVE, in terms an operator would recognise.
///
/// Deliberately flat `&'static str`s rather than a borrow of [`ArmingSetting`]: the node-scoped
/// source is not a table row and never will be, and a caller assembling a refusal message wants one
/// list to iterate, not two shapes to match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveArming {
    /// What the operator SET, spelled the way they would find it again — a variable name for a
    /// table row, `key (file, or VARIABLE)` for a setting that has both layers.
    pub source: &'static str,
    /// What that turns on, in the operator's own terms — the same field, and the same job, as
    /// [`ArmingSetting::arms`].
    pub arms: &'static str,
    /// Whether this evidence speaks for one venue or for the mount.
    pub scope: ArmingScope,
}

impl LiveArming {
    /// The venue-scoped view of one [`CREDENTIAL_FILE_ARMING_REFUSED`] row.
    #[must_use]
    pub const fn of_setting(s: &'static ArmingSetting) -> Self {
        LiveArming { source: s.var, arms: s.arms, scope: ArmingScope::Venue }
    }
}

/// The ONE node-scoped arming source: the headless daemon's real-money master gate.
///
/// ⚠ `source` names BOTH layers on purpose. The flag resolves from `<project>/settings/flags.toml`
/// **or** `VIKE_TRADEHUB_LIVE` (env wins), and an operator handed only one spelling looks in one
/// place, finds nothing, and concludes the report is wrong. On the one box this is deployed to that
/// would be exactly backwards — MEASURED on the CI box (2026-08-09, read-only): its `flags.toml` sets
/// `cancel_orders_on_shutdown` and says nothing whatever about the live gate, while the unit's
/// `EnvironmentFile=` carries `VIKE_TRADEHUB_LIVE=1`. Naming only the file would have sent that
/// operator to the one place the answer is not.
pub const TRADEHUB_LIVE_ARMING: LiveArming = LiveArming {
    source: "tradehub_live (settings/flags.toml, or VIKE_TRADEHUB_LIVE)",
    arms: "the headless daemon's credential-gated TWELVE-VENUE live mount — every venue whose \
           credentials are in the store goes live, SWITCHLESS ones (deribit/alpaca/aster/…) \
           included",
    scope: ArmingScope::Node,
};

/// The FILE-layer spelling of [`CREDENTIAL_FILE_ARMING_REFUSED`]'s two Polymarket rows.
///
/// ⚠ **These exist because the exclusion they replace STOPPED BEING TRUE**, and the gate that
/// noticed is `the_polymarket_flags_are_file_evidence_now` below. Both flags were excluded from the
/// file layer on one stated fact — that nothing consumed them, so a `poly_exec = true` line in
/// `flags.toml` armed nothing and refusing a box over it would fire on a setting with no effect.
/// The unread-settings sweep WIRED both (`crate::CONSUMPTION` names
/// `crates/vike-tradehub/src/tradehub_cli.rs` for each), so that line now arms a real Polymarket
/// exec mount and the exclusion would be a hole in exactly the direction this module exists to
/// close: a box armed from a file, reported UNARMED, degrading past an unreadable store.
///
/// The `arms` text is taken from the table row rather than restated, so the two spellings of one
/// gate cannot describe different things; only [`PolyFileArming::source`] differs. It named BOTH
/// layers, for the reason [`TRADEHUB_LIVE_ARMING`]'s still does, until decision 0095 retired the
/// environment layer of both flags: now the settings row is the only thing that can arm either, and
/// the source names that one.
const POLY_FILE_ARMING: &[PolyFileArming] = &[
    PolyFileArming {
        var: "POLY_EXEC",
        on: |f| f.poly_exec,
        source: "flags.poly_exec (the settings database)",
    },
    PolyFileArming {
        var: "POLY_RECONCILE",
        on: |f| f.poly_reconcile,
        source: "flags.poly_reconcile (the settings database)",
    },
];

/// One [`POLY_FILE_ARMING`] row.
///
/// A named struct rather than the `(&str, fn(Flags) -> bool, &str)` tuple it started as: clippy's
/// `type_complexity` refuses that spelling under this workspace's `-D warnings` merge gate. The
/// fields read better than positions anyway — `on` is a FLAG READER, not a predicate on the
/// variable, and a tuple hides which of the two `&str`s is which.
struct PolyFileArming {
    /// The retired variable this row's gate used to answer to — the name `armed_settings_in`
    /// reports under, so one gate is never counted twice.
    var: &'static str,
    /// Reads the RESOLVED flag out of [`Flags`] — the settings row alone, since decision 0095 retired
    /// the flag's environment layer (a set variable refuses startup instead).
    on: fn(Flags) -> bool,
    /// What [`LiveArming::source`] says when this row armed the box: the settings row that did.
    source: &'static str,
}

/// Whether this box intends to trade LIVE — the three answers, one of which is "I could not tell".
///
/// A plain `Vec<LiveArming>` was the obvious return type and it is the wrong one: an empty vector
/// reads as "not armed" whether the sources all answered NO or one of them could not be consulted,
/// and conflating those two is the exact defect this whole second source exists to repair. So the
/// ignorance case is a VARIANT, and [`LiveArmingVerdict::refuses`] treats it like an arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveArmingVerdict {
    /// Every source answered, and none of them says live. The ONLY answer that permits the ADR 0013
    /// degrade — a paper box, which is the overwhelmingly common case and must keep starting.
    Unarmed,
    /// At least one source says live. Never empty; venue-scoped rows first, in table order.
    Armed(Vec<LiveArming>),
    /// The NODE-scoped source could not be resolved (the settings tree did not load), and no
    /// venue-scoped row answered either — so "unarmed" is not something anyone here knows.
    Undetermined,
}

impl LiveArmingVerdict {
    /// Whether a caller deciding a DISPOSITION must take the strict branch: armed, or unknowable.
    ///
    /// The fail-safe direction, and the whole point of the [`LiveArmingVerdict::Undetermined`]
    /// variant. Only [`LiveArmingVerdict::Unarmed`] — every source consulted, every source silent —
    /// earns the degrade.
    #[must_use]
    pub fn refuses(&self) -> bool {
        !matches!(self, LiveArmingVerdict::Unarmed)
    }

    /// The evidence, for a message that has to NAME what armed the box. Empty for the two variants
    /// that have none, which a caller renders as its own sentence rather than a list.
    #[must_use]
    pub fn evidence(&self) -> &[LiveArming] {
        match self {
            LiveArmingVerdict::Armed(v) => v,
            LiveArmingVerdict::Unarmed | LiveArmingVerdict::Undetermined => &[],
        }
    }
}

/// Does this box intend to trade LIVE? The union of two independent sources, NEITHER of which is
/// the credential store — see the module doc's second-question section for the argument.
///
/// 1. [`armed_settings_in`] over `env`, the PROCESS environment — the `{VENUE}_MAINNET` / `POLY_*`
///    rows, one venue each, and blind to the nine SWITCHLESS venues by construction.
/// 2. [`TRADEHUB_LIVE_ARMING`], when the RESOLVED `flags.tradehub_live` is on — node-scoped, so it
///    is the source that sees a switchless venue at all.
///
/// ⚠ `flags` is `None` ONLY when the settings tree did not LOAD, which is a real state for the
/// caller that has it: `vike-cli config check` reports on a directory whose `policy.toml` does not
/// parse. It does NOT mean "no flags file" — an absent `flags.toml` resolves to
/// `Flags::default()`, every field false, and that is a genuine `Some`. Passing `Flags::default()`
/// for "I do not know" is the one call-site mistake that reopens the hole, which is why the
/// parameter is an `Option` and the unknown answer is a variant rather than an empty list.
#[must_use]
pub fn armed_for_live(flags: Option<Flags>, env: &HashMap<String, String>) -> LiveArmingVerdict {
    let mut out: Vec<LiveArming> =
        armed_settings_in(env).into_iter().map(LiveArming::of_setting).collect();
    match flags {
        Some(f) => {
            if f.tradehub_live {
                out.push(TRADEHUB_LIVE_ARMING);
            }
            // ...and the two VENUE-scoped gates that resolve from a FILE as well as a variable —
            // see [`POLY_FILE_ARMING`] for why they stopped being an exclusion.
            //
            // ⚠ Skipped when the process-env row already reported the same variable. The resolved
            // flag no longer reads that variable (decision 0095 retired its environment layer; a
            // set one refuses startup), but an environment judged here can still carry it beside
            // the row, and pushing both would report one arm as two pieces of evidence — an
            // operator would go looking for a second setting they never made. The env row wins the
            // tie because it names the spelling somebody has to remove.
            for row in POLY_FILE_ARMING {
                if !(row.on)(f) || out.iter().any(|a| a.source == row.var) {
                    continue;
                }
                let Some(s) = CREDENTIAL_FILE_ARMING_REFUSED.iter().find(|s| s.var == row.var)
                else {
                    continue;
                };
                out.push(LiveArming {
                    source: row.source,
                    arms: s.arms,
                    scope: ArmingScope::Venue,
                });
            }
            if out.is_empty() { LiveArmingVerdict::Unarmed } else { LiveArmingVerdict::Armed(out) }
        }
        // The node-scoped half is unknown. A venue-scoped row still ARMS on its own — it is
        // positive evidence and needs no corroboration — and reporting it beats reporting nothing,
        // since both answers refuse and only one of them names something the operator can act on.
        None if !out.is_empty() => LiveArmingVerdict::Armed(out),
        None => LiveArmingVerdict::Undetermined,
    }
}

/// Refuse to start when the credential file carries a value that would ARM real money.
/// `Ok(())` is the overwhelmingly common case.
///
/// `secrets` is the parsed credential map (`<project>/settings/secrets.env`), NOT the process
/// environment — the whole point is that this file is the wrong place for these. The `Err` string
/// is the complete operator-facing message, ready to print; every offender is reported in ONE pass,
/// because fixing a file one restart at a time is a worse experience than being handed the list.
///
/// ⚠ The offending VALUE is never echoed, unlike `refuse_removed_env`'s `echo_value` rows. This
/// function reads a credentials file, and a line's value there is not something to print into a log
/// or a terminal on the strength of its NAME matching a table.
pub fn refuse_credential_file_arming(secrets: &HashMap<String, String>) -> Result<(), String> {
    let offenders = armed_settings_in(secrets);
    if offenders.is_empty() {
        return Ok(());
    }

    let mut out = String::from(
        "REFUSING TO START: the credential store carries a switch that arms REAL MONEY, or one \
         decision 0095 retired.\n\n\
         That file is the credential store. It is plaintext, it is read last-wins, and appending \
         one line to it must never be enough to move this process onto a live venue — so an \
         arming value there is refused rather than obeyed.\n\n",
    );
    for s in &offenders {
        out.push_str(&format!("  {} — {}\n", s.var, s.arms));
    }
    out.push_str(
        "\nRemove these rows from the credential store (`vike-cli secrets path` prints which one \
         this box reads). If you meant what they say:\n\n",
    );
    for s in &offenders {
        out.push_str(&format!("    {}\n", s.instead));
    }
    Err(out)
}

/// **A credential row carrying a declared venue field's LEGACY name refuses startup.**
///
/// Decision 0095's Task 7 retired the credential-map fold: every venue setting is read from the
/// `venue_setting` table through `vike_secrets::venue_setting::VenueSettings`, and nothing reads the
/// credential map for one any more. A row like `IBKR_DEMO_PORT` or `POLY_RATE_GATE` left in the
/// credential store would therefore be read by NOTHING — a value that looks configured and is
/// silently ignored, which spec §4 of decision 0095 forbids — so it stops the process instead, and
/// the refusal names each row, the setting it is, and the verb that moves it.
///
/// The set is `vike_secrets::venue_setting::stranded_venue_setting_names`, derived from the settings
/// catalog: a labelled `__<LABEL>` spelling of a TIER-scoped field is stranded too, because those
/// settings are one per machine and tier (ruling 10). `vike-cli secrets move-venue-config` moves
/// exactly this set — `crates/vike-bridge-core/tests/credential_classification.rs`'s
/// `the_boot_refusal_and_the_move_verb_agree_on_every_name` holds the two equal — so the verb the
/// message names can always clear it.
///
/// Names only, never values: a credential store's values are not printed on the strength of a name.
pub fn refuse_stranded_venue_settings(credentials: &HashMap<String, String>) -> Result<(), String> {
    match stranded_venue_settings_report(credentials) {
        None => Ok(()),
        Some(report) => Err(format!("REFUSING TO START: {report}")),
    }
}

/// [`refuse_stranded_venue_settings`]'s finding as a sentence with no disposition attached — `None`
/// when nothing is stranded. For a root that mounts no venue and REPORTS the finding instead of
/// refusing on it (`vike_boot::Credentials::LoadReportingStrandedSettings`). Names only.
#[must_use]
pub fn stranded_venue_settings_report(credentials: &HashMap<String, String>) -> Option<String> {
    let stranded = vike_secrets::venue_setting::stranded_venue_setting_names(
        credentials.keys().map(String::as_str),
    );
    if stranded.is_empty() {
        return None;
    }
    let mut out = String::from(
        "the credential store holds venue SETTINGS under their old credential names, and nothing \
         reads them there any more (decision 0095):\n\n",
    );
    for (name, key) in &stranded {
        out.push_str(&format!("    {name}  is the setting  {key}\n"));
    }
    out.push_str(
        "\n`vike-cli secrets move-venue-config --dry-run` shows the move and `vike-cli secrets \
         move-venue-config` performs it — a labelled `__<LABEL>` spelling included, because these \
         settings are one per machine and tier (ruling 10); where it reports two DIFFERENT values \
         for one setting, make them equal with `vike-cli secrets set` first. A box with no \
         settings database creates one first (`vike-cli secrets migrate`): venue settings live \
         only there.\n",
    );
    Some(out)
}

#[path = "arming_tests.rs"]
#[cfg(test)]
mod arming_tests;
