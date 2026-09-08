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
//! It deliberately leaves the ARMING RULE alone. `vike_bridge_core::mainnet` is a signed-off,
//! machine-gated convergence (its module doc calls it "STEP 2"): every `{VENUE}_MAINNET` flag arms
//! on the exact string `"1"` from the process env **or** the workspace credential map, and reading
//! BOTH was the whole point — it closed the repo-wide "the credential file is never exported to the
//! process env" trap, where a flag written in the file where every other venue setting lives parsed
//! as UNSET and silently kept the venue on demo. That doc explicitly considered and REJECTED
//! "process-env-only everywhere" for exactly that reason, and
//! `the_hl_vs_cex_divergence_is_converged` fails deliberately if either half is reverted.
//!
//! So this is not that reversal. The rule still reads both sources; a configuration that would arm
//! real money FROM THE CREDENTIAL FILE simply stops the process instead of running. That closes the
//! escalation (an appended line can no longer arm anything — it can only halt) while keeping the
//! property STEP 2 bought: the operator is never silently on the wrong network. Silence was the
//! original bug; a refusal is not silence.
//!
//! ## Why refuse rather than ignore
//!
//! Ignoring the file's value and dropping to demo would be the other fail-safe option, and it is
//! worse. An operator who wrote `BINANCE_MAINNET=1` believes they are on mainnet; a build that
//! quietly traded demo would have them reading demo fills as real ones. Silently changing which
//! venue tier you trade on is worse than either behaviour alone — so the process names the
//! variable and stops, and the operator moves it one file over.
//!
//! ## Residual, stated rather than implied
//!
//! This is a ROOT-level check, so it binds only the composition roots that call it. Three exposures
//! it deliberately does NOT cover, because each needs its own change:
//!
//! - **Credential PRESENCE as the live gate.** `vike-mount`'s `("aster", _)` arm tries
//!   `Environment::Live` FIRST and falls back to Demo, so `ASTER_LIVE_*` keys in the file arm
//!   mainnet with no flag involved at all. Aster is declared switchless in
//!   `vike_bridge_core::mainnet::mainnet_switch_for` — there is no `ASTER_MAINNET` to refuse. Same
//!   shape for the legacy `*_MAINNET_*` credential-name aliases `Environment::legacy_str` accepts.
//! - **Libraries that load the credential file THEMSELVES**, below any root:
//!   `vike_polymarket::exec`'s `dotenv_rate_gate` and `settlement/chain.rs`'s `dotenv_chain_vars`
//!   (`POLY_CHAIN_WATCH`) call `load_workspace_dotenv()` internally, so no caller-supplied map
//!   passes through here. These are already tracked by `CREDENTIAL_STORE_PIN`.
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
//! `vike_bridge_core::mainnet::mainnet_switch_for` calls SWITCHED have such a variable at all. The
//! every OTHER roster venue picks its tier from the credential PREFIX (`DERIBIT_LIVE_*` vs
//! `DERIBIT_DEMO_*`), from which gateway it logs into (ibkr), or not at all because it is
//! mainnet-only (polymarket) — so a box live on one of them read as UNARMED and its unreadable store
//! took the degrade — silently starting all-paper while every surface said live, which is the
//! set-but-unhonoured false belief the refusal exists to catch.
//!
//! ⚠ This paragraph used to name that set as "the other nine … deribit, oanda, ig, fxcm, dukascopy,
//! ctrader, alpaca, ibkr, aster". It was a hand-written roster and it had already rotted — the set
//! is TEN and the missing one was polymarket. `crates/vike-bridge-core/src/mainnet.rs`'s
//! `every_roster_venue_is_classified` is
//! the authority: it pins both partitions and asserts they sum to `vike_model::VENUES`. Do not
//! restate either list here; a roster in prose is what the root `CLAUDE.md` forbids, for this reason.
//!
//! ⚠ **Widening the table cannot fix that, and the reason is worth stating once.** For a switchless
//! venue "am I armed for live" is answerable only from the credential prefixes — inside the very
//! file that could not be read. The signal and the failure are THE SAME OBJECT, so any repair has
//! to come from a source that is independent of the store.
//!
//! [`TRADEHUB_LIVE_ARMING`] is that source. `flags.tradehub_live` — `<project>/settings/flags.toml`
//! or `VIKE_TRADEHUB_LIVE`, resolved by [`crate::load`] like any other setting — is the headless
//! daemon's live master gate, and it is **NODE-scoped**: it selects `crates/vike-tradehub/src/tradehub_cli.rs`'s
//! `live_mount` over the paper one, and that mount is `crates/vike-run/src/node.rs`'s `build_node`,
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
//! ### Two more things deliberately NOT folded in
//!
//! - **`flags.poly_exec` / `flags.poly_reconcile` as FILE evidence.** Both have a `Flags` field and
//!   neither is consumed: `crate::CONSUMPTION` records the real readers as
//!   `crates/bridges/polymarket/src/mount.rs`'s `poly_exec_enabled` and
//!   `crates/bridges/polymarket/src/recon_client.rs`'s `poly_reconcile_enabled`, a hybrid
//!   process-env-then-credential-map read the file layer does not reach. A `poly_exec = true` line
//!   in `flags.toml` therefore arms NOTHING today, and refusing a box over a setting with no effect
//!   is how a check teaches people to work around it. Their PROCESS-ENV spelling is already a table
//!   row and is counted there.
//!   `the_polymarket_flags_are_excluded_because_the_file_layer_arms_nothing` gates the fact rather
//!   than this paragraph: it fails the day either becomes consumed.
//! - **`VIKE_TRADEHUB_LIVE` as a [`CREDENTIAL_FILE_ARMING_REFUSED`] row.** That table refuses a line
//!   in the credential FILE, and `vike-tradehub` resolves this flag through [`crate::load`] over its
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
//! `vike-app` — the GUI — has no live gate at all: a venue mounts live iff its credentials are
//! present. On that box "armed for live" IS "the store has credentials", so an unreadable store
//! leaves the question unanswerable in the same way, and [`armed_for_live`] reports nothing for it
//! unless a `{VENUE}_MAINNET` flag happens to be exported. Closing that needs a live gate the GUI
//! does not have; it is an interactive program rather than an unattended unit, which is why the
//! residual is tolerated — not why it is absent.

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
}

/// Every variable whose presence in the credential file is refused, because an armed value there
/// turns PAPER or DEMO into REAL MONEY.
///
/// The set is deliberately the ARMING gates only, not every flag that is map-readable. A knob that
/// cannot by itself cause a real order — `POLY_PRESUBMIT_REGISTER` (a within-exec behaviour toggle
/// that does nothing unless `POLY_EXEC` already armed), `POLY_HEARTBEAT` — is not refused, because
/// a refusal list that fires on harmless lines trains operators to work around it.
///
/// `{VENUE}_MAINNET` rows are exactly the venues `vike_bridge_core::mainnet::mainnet_switch_for`
/// declares SWITCHED. Every other roster venue is declared switchless there and has no flag to
/// refuse — notably **aster**, whose tier is chosen by which credential PREFIX is present
/// (`ASTER_LIVE_*` vs `ASTER_TESTNET_*`), so there is nothing named `ASTER_MAINNET` for this table
/// to hold (see this module's residual note).
pub const CREDENTIAL_FILE_ARMING_REFUSED: &[ArmingSetting] = &[
    ArmingSetting {
        var: "BINANCE_MAINNET",
        arms: "binance MAINNET endpoints and the LIVE credential tier (real funds)",
    },
    ArmingSetting {
        var: "BYBIT_MAINNET",
        arms: "bybit MAINNET endpoints and the LIVE credential tier (real funds)",
    },
    ArmingSetting {
        var: "OKX_MAINNET",
        arms: "okx MAINNET endpoints and the LIVE credential tier (real funds)",
    },
    ArmingSetting {
        var: "HYPERLIQUID_MAINNET",
        arms: "hyperliquid MAINNET endpoints and the LIVE credential tier (real funds)",
    },
    ArmingSetting {
        var: "POLY_EXEC",
        arms: "Polymarket LIVE order placement — the venue has no testnet, so every order it \
               mounts is real money on Polygon mainnet",
    },
    ArmingSetting {
        var: "POLY_RECONCILE",
        arms: "Polymarket reconcile — authenticated MAINNET account reads",
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
        "REFUSING TO START: <project>/settings/secrets.env arms REAL MONEY.\n\n\
         That file is the credential store. It is plaintext, it is read last-wins, and appending \
         one line to it must never be enough to move this process onto a live venue — so an \
         arming value there is refused rather than obeyed.\n\n",
    );
    for s in &offenders {
        out.push_str(&format!("  {} arms {}\n", s.var, s.arms));
    }
    out.push_str(
        "\nRemove these lines from <project>/settings/secrets.env. If you DID mean to go live, \
         set them in the process environment instead — an exported shell variable, or a systemd \
         unit's Environment= / EnvironmentFile= line:\n\n",
    );
    for s in &offenders {
        out.push_str(&format!("    Environment={}=1\n", s.var));
    }
    out.push_str(
        "\nThe process environment is still read, and still arms these exactly as before; only \
         the credential file has stopped being a way to do it.\n",
    );
    Err(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// The SECOND question the table answers — "is this box armed for live?" — asked of a
    /// PROCESS-ENVIRONMENT map, and held to the same grammar as the refusal so the two can never
    /// disagree about what counts as armed. Its consumer is `vike-cli config check`, which uses it
    /// to decide whether an unreadable credential store is a degrade or a refusal.
    #[test]
    fn armed_settings_in_reads_the_same_table_with_the_same_grammar() {
        assert!(armed_settings_in(&map(&[])).is_empty());
        for s in CREDENTIAL_FILE_ARMING_REFUSED {
            let armed = armed_settings_in(&map(&[(s.var, "1")]));
            assert_eq!(armed.len(), 1, "{} must read as armed", s.var);
            assert_eq!(armed[0].var, s.var);
            // …and the disarming spellings the refusal also ignores.
            for v in ["0", "", "  ", "true", "# 1"] {
                assert!(
                    armed_settings_in(&map(&[(s.var, v)])).is_empty(),
                    "{v:?} arms nothing at any reader"
                );
            }
        }
        // The refusal is now this function plus a message, so the two are the same predicate.
        let both = map(&[("BINANCE_MAINNET", "1"), ("POLY_EXEC", "1  # arbdub only")]);
        assert_eq!(armed_settings_in(&both).len(), 2);
        assert!(refuse_credential_file_arming(&both).is_err());
    }

    /// THE ESCALATION: one appended line, and the process would have gone live. It now stops.
    #[test]
    fn an_arming_line_in_the_credential_file_refuses_to_start() {
        for s in CREDENTIAL_FILE_ARMING_REFUSED {
            let err = refuse_credential_file_arming(&map(&[(s.var, "1")]))
                .expect_err("an arming credential-file line must refuse");
            assert!(err.contains(s.var), "the refusal must NAME the variable, got {err}");
            assert!(
                err.contains("Environment="),
                "and must say where it belongs instead, got {err}"
            );
        }
    }

    /// The annotated spelling the real credential file uses, and which the Polymarket readers
    /// normalise with `first_token`, must not slip past — otherwise the refusal is evaded by a
    /// trailing comment while the venue still arms.
    #[test]
    fn a_trailing_comment_does_not_evade_the_refusal() {
        assert!(
            refuse_credential_file_arming(&map(&[("POLY_EXEC", "1  # real money, arbdub only")]))
                .is_err()
        );
        assert!(refuse_credential_file_arming(&map(&[("BINANCE_MAINNET", " 1 ")])).is_err());
    }

    /// A DISARMING line is not an offence. Refusing `BINANCE_MAINNET=0` would fire on a
    /// configuration that arms nothing at any reader, and a check that cries wolf gets worked
    /// around.
    #[test]
    fn a_disarming_or_absent_value_starts_normally() {
        assert!(refuse_credential_file_arming(&map(&[])).is_ok());
        for v in ["0", "", "  ", "true", "yes", "# 1"] {
            assert!(
                refuse_credential_file_arming(&map(&[("BINANCE_MAINNET", v)])).is_ok(),
                "{v:?} arms nothing and must not refuse"
            );
        }
    }

    /// Ordinary credentials are untouched — this must never fire on the file's actual contents.
    #[test]
    fn a_normal_credential_file_is_not_refused() {
        assert!(
            refuse_credential_file_arming(&map(&[
                ("BINANCE_DEMO_API_KEY", "k"),
                ("BINANCE_DEMO_API_SECRET", "s"),
                ("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xdead"),
                ("POLY_EXEC_MARKETS", "btc-updown-5m"),
            ]))
            .is_ok()
        );
    }

    /// Every offender is reported in ONE pass.
    #[test]
    fn all_offenders_are_named_at_once() {
        let err =
            refuse_credential_file_arming(&map(&[("BINANCE_MAINNET", "1"), ("POLY_EXEC", "1")]))
                .expect_err("two offenders");
        assert!(err.contains("BINANCE_MAINNET") && err.contains("POLY_EXEC"), "got {err}");
    }

    // -- "is this box armed for live?" — the two-source union -----------------------------------

    fn live(tradehub_live: bool) -> Flags {
        Flags { tradehub_live, ..Default::default() }
    }

    /// **The residual this function exists to close, driven directly.** A box whose ONLY arming is
    /// `flags.tradehub_live` reads as ARMED — with an EMPTY process environment, i.e. with no
    /// `{VENUE}_MAINNET` anywhere, which is exactly the shape of a box live on a SWITCHLESS venue
    /// (deribit/alpaca/aster/…). Before this source existed [`armed_settings_in`] answered "not
    /// armed" for it, and `vike-cli config check` degraded an unreadable credential store on a live
    /// node.
    #[test]
    fn the_node_scoped_source_arms_with_no_venue_variable_anywhere() {
        let v = armed_for_live(Some(live(true)), &map(&[]));
        assert_eq!(v, LiveArmingVerdict::Armed(vec![TRADEHUB_LIVE_ARMING]), "{v:?}");
        assert!(v.refuses());
        assert_eq!(v.evidence()[0].scope, ArmingScope::Node, "it speaks for the whole mount");
        // …and the venue-scoped half genuinely saw nothing, so this is the NEW source answering.
        assert!(armed_settings_in(&map(&[])).is_empty());
    }

    /// The flag OFF is the paper box `docs/decisions/0013-degrade-vs-refuse.md` protects: nothing
    /// armed, so an unreadable store stays a degrade. Same tree, one boolean apart — and
    /// `Unarmed` is the ONLY verdict that does not refuse.
    #[test]
    fn the_node_scoped_source_is_silent_on_a_paper_box() {
        let v = armed_for_live(Some(live(false)), &map(&[]));
        assert_eq!(v, LiveArmingVerdict::Unarmed);
        assert!(!v.refuses(), "a paper box must keep starting — ADR 0013's degrade");
        assert!(v.evidence().is_empty());
    }

    /// ⚠ **The hole one level up: a tree that did not LOAD is not a paper box.** `None` must never
    /// answer `Unarmed`, because that is the same "no signal ⇒ assume paper" inference the
    /// node-scoped source was added to end. It refuses instead.
    #[test]
    fn an_unresolvable_settings_tree_is_undetermined_and_still_refuses() {
        let v = armed_for_live(None, &map(&[]));
        assert_eq!(v, LiveArmingVerdict::Undetermined);
        assert!(v.refuses(), "'I could not tell' must not be spelled 'not armed'");
        assert!(v.evidence().is_empty(), "…and it must not invent evidence either");
        // A venue-scoped row still answers on its own — positive evidence needs no corroboration,
        // and naming it beats an anonymous refusal.
        let armer = CREDENTIAL_FILE_ARMING_REFUSED[0].var;
        let v = armed_for_live(None, &map(&[(armer, "1")]));
        assert_eq!(v.evidence().len(), 1, "{v:?}");
        assert!(v.refuses());
    }

    /// ⚠ `Flags::default()` is a REAL answer (an absent `flags.toml`), not a stand-in for "unknown"
    /// — the two are one `Option` apart and only one of them degrades. Pinned because passing the
    /// default for an unresolved tree is the single call-site mistake that reopens the hole.
    #[test]
    fn default_flags_are_an_answer_and_none_is_not() {
        assert_eq!(armed_for_live(Some(Flags::default()), &map(&[])), LiveArmingVerdict::Unarmed);
        assert_eq!(armed_for_live(None, &map(&[])), LiveArmingVerdict::Undetermined);
    }

    /// Both sources at once, in table order, each carrying its own scope — the shape a refusal
    /// message iterates so an operator fixing a box is handed the whole list.
    #[test]
    fn both_sources_are_reported_together_venue_rows_first() {
        let armer = CREDENTIAL_FILE_ARMING_REFUSED[0].var;
        let v = armed_for_live(Some(live(true)), &map(&[(armer, "1")]));
        let ev = v.evidence();
        assert_eq!(ev.len(), 2, "{v:?}");
        assert_eq!(ev[0], LiveArming::of_setting(&CREDENTIAL_FILE_ARMING_REFUSED[0]));
        assert_eq!(ev[0].scope, ArmingScope::Venue);
        assert_eq!(ev[1], TRADEHUB_LIVE_ARMING);
    }

    /// Every venue-scoped row still arms on its own, with the flag OFF — the pre-existing behaviour
    /// this change must not narrow while widening the signal.
    #[test]
    fn every_table_row_still_arms_without_the_node_scoped_flag() {
        for s in CREDENTIAL_FILE_ARMING_REFUSED {
            let v = armed_for_live(Some(live(false)), &map(&[(s.var, "1")]));
            assert_eq!(v.evidence().len(), 1, "{} must still arm alone: {v:?}", s.var);
            assert_eq!(v.evidence()[0].source, s.var);
            assert!(v.refuses());
        }
    }

    /// The node-scoped source must name BOTH layers it resolves from. An operator told only
    /// `VIKE_TRADEHUB_LIVE` greps a `flags.toml` that never mentioned it — or, on the CI box, greps
    /// `flags.toml` while the arming line sits in the unit's `EnvironmentFile=`.
    #[test]
    fn the_node_scoped_source_names_both_layers_it_resolves_from() {
        assert!(TRADEHUB_LIVE_ARMING.source.contains("tradehub_live"), "the file KEY");
        assert!(TRADEHUB_LIVE_ARMING.source.contains("flags.toml"), "…and the file");
        assert!(
            TRADEHUB_LIVE_ARMING.source.contains(crate::flags::TRADEHUB_LIVE_ENV),
            "…and the env"
        );
        assert!(TRADEHUB_LIVE_ARMING.arms.contains("SWITCHLESS"), "…and WHY it is the new source");
    }

    /// ⚠ **The two Polymarket flags are deliberately NOT file-layer evidence**, and this pins the
    /// FACT the exclusion rests on rather than the prose: `crate::CONSUMPTION` says nothing consumes
    /// `flags.poly_exec` / `flags.poly_reconcile`, so a `poly_exec = true` line in `flags.toml` arms
    /// nothing and refusing a box over it would fire on a setting with no effect. If either becomes
    /// consumed, this test fails and the exclusion has to be revisited — which is the whole point of
    /// gating it here instead of writing it down.
    #[test]
    fn the_polymarket_flags_are_excluded_because_the_file_layer_arms_nothing() {
        for key in ["flags.poly_exec", "flags.poly_reconcile"] {
            assert!(
                !crate::is_consumed(key),
                "{key} is CONSUMED now — the file layer reaches the mount, so it is real arming \
                 evidence and `armed_for_live` must count it (see this module's exclusion note)"
            );
        }
        // …so a flags-file-only Polymarket arm contributes nothing, while its PROCESS-ENV spelling
        // (a real table row) still does.
        let f = Flags { poly_exec: true, poly_reconcile: true, ..Default::default() };
        assert_eq!(armed_for_live(Some(f), &map(&[])), LiveArmingVerdict::Unarmed);
        assert_eq!(armed_for_live(Some(f), &map(&[("POLY_EXEC", "1")])).evidence().len(), 1);
    }

    /// `VIKE_TRADEHUB_LIVE` must NOT join the credential-file refusal table: `vike-tradehub`
    /// resolves it through `crate::load` over its own process-env sweep, the store is never merged
    /// in, so a line in `secrets.env` arms nothing — and a refusal list that fires on harmless lines
    /// trains operators to work around it (this module's table doc).
    #[test]
    fn the_node_scoped_flag_is_not_a_credential_file_offence() {
        assert!(
            !CREDENTIAL_FILE_ARMING_REFUSED
                .iter()
                .any(|s| s.var == crate::flags::TRADEHUB_LIVE_ENV),
            "the credential file cannot arm this flag, so it must not be refused there"
        );
        assert!(
            refuse_credential_file_arming(&map(&[(crate::flags::TRADEHUB_LIVE_ENV, "1")])).is_ok()
        );
    }

    /// The `{VENUE}_MAINNET` rows must stay in step with the venues
    /// `vike_bridge_core::mainnet::mainnet_switch_for` declares SWITCHED. This crate cannot depend
    /// on that one (vike-config sits below the bridge layer), so the pin is spelled here and the
    /// mirror test lives in `vike-bridge-core`, where both sides are visible.
    #[test]
    fn the_mainnet_rows_are_exactly_the_switched_venues() {
        let mut rows: Vec<&str> = CREDENTIAL_FILE_ARMING_REFUSED
            .iter()
            .map(|s| s.var)
            .filter(|v| v.ends_with("_MAINNET"))
            .collect();
        rows.sort_unstable();
        assert_eq!(
            rows,
            ["BINANCE_MAINNET", "BYBIT_MAINNET", "HYPERLIQUID_MAINNET", "OKX_MAINNET"],
            "the switched-venue set changed — see vike_bridge_core::mainnet::mainnet_switch_for"
        );
    }
}
