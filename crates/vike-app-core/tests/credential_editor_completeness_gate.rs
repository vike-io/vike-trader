//! **EVERY KEY THE CREDENTIAL STORE CAN HOLD IS EITHER REACHABLE FROM THE CONNECTIONS EDITOR OR
//! CARRIES A WRITTEN ROW SAYING WHY IT MUST NOT BE.**
//!
//! # The defect, measured on a real store
//!
//! `vike_connections::view::edit_fields` is a hand-written `match venue` table beside an
//! authority that can be derived. The failure such a table has is not a WRONG key — every name it
//! composes is pinned against the loader that reads it by
//! `crates/vike-connections/tests/editor_key_shapes.rs` — it is a key the store really holds that
//! appears in NO arm at all, because nothing about an arm says which of its venue's names are
//! missing from it. MEASURED: the owner's `vike-cli secrets list` held
//! `DUKASCOPY_DEMO1_SERVER` and `DUKASCOPY_DEMO2_SERVER`; the DEMO form offered four fields and
//! neither of those; so two keys already in use could be changed from nowhere in the app. That is
//! the same shape as every other *control that silently does nothing*, one level up: the control
//! was never drawn.
//!
//! # Why this file lives in `vike-app-core`
//!
//! It needs BOTH `vike_connections::view::edit_fields` (layer 75) and
//! `vike_ops::settings::SETTINGS` (layer 55), and `vike-app-core` is the lowest crate that depends
//! on both — the same placement argument `crates/vike-cli/tests/node_cli.rs`'s
//! `the_platform_key_table_is_the_servers_own_spelling` makes for the platform-key table.
//! `vike-connections` deliberately does NOT depend on `vike-ops` (its manifest carries the reason:
//! that edge drags vike-core/vike-data/vike-alerting behind it), so the gate cannot live beside
//! the table it gates.
//!
//! # The universe, and why it is the registry rather than the key GRID
//!
//! `vike_model::credential_keys::credential_keys` is the GENERIC grid —
//! `{VENUE}_{TIER}_API_{KEY,SECRET,PASSPHRASE}` folded over the roster — and a gate over that
//! alone would have been green on the very defect it is named for: `DUKASCOPY_DEMO1_SERVER` is a
//! BESPOKE name and the grid cannot produce it. So the universe is
//! `vike_ops::settings::SETTINGS`'s whole `Scope::Venue` set (every venue-owned name this
//! workspace is declared to read, bespoke ones included), unioned with
//! `credential_keys::lookup_keys` (the grid plus the attribution family, which is where the grid
//! rows come from anyway) and `credential_keys::PLATFORM_KEYS` (which are `Scope::Vike` and would
//! otherwise be silently outside the question rather than deliberately excluded from it).
//!
//! ⚠ **DELIBERATELY NO `Layer` FILTER.** Dropping `Layer::TestOnly` rows would be one line and
//! would shrink the exemption table by a dozen names, and it would also be a blind spot with no
//! floor: a name's layer records where the SCANNER saw it, and several names the bridges genuinely
//! read are declared `TestOnly` because a fixture is their only sighting
//! (`CTRADER_DEMO_ACCOUNT_ID`, `OANDA_SIM_ACCOUNT_ID`). Excluding by layer would have excused those
//! without anybody writing down that they were excused. The smoke-only inputs pay for that with
//! two rows below, which is the cheaper end of the trade.
//!
//! # ⚠ THE DECLARED RESIDUAL: two names this gate structurally cannot see
//!
//! `vike_fxcm::config::fxcm_env_var_names` composes FOUR names and `load_fxcm_config_from` reads
//! all four — `FXCM_{TIER}_USER`, `_PASSWORD`, `_URL` and `_CONNECTION`. The form offers the first
//! two. The other two appear as no literal anywhere and no fixture spells them, so
//! `vike_ops::settings::SETTINGS` carries no row for either and the universe above does not contain
//! them: this gate would be green on them however they were handled, which is why they are written
//! down here instead of being given a [`NOT_EDITABLE`] row that
//! [`no_written_exemption_is_stale`] would immediately delete.
//!
//! **They are deliberately not offered**, on the same argument as the `IBKR_DEMO_*` row below and
//! one notch sharper: `_CONNECTION` selects the FXCM connection NAME, whose default is `Real` on
//! Live and `Demo` otherwise (`vike_fxcm::config::default_connection`). The TIER is chosen by the
//! CELL the form was opened from, so a field that can override it would let a form labelled `demo`
//! point demo credentials at the live platform — the one mistake a credential panel must not make
//! reachable. `_URL` is the same knob wearing an endpoint.
//!
//! ⚠ Dukascopy's `_SERVER` IS offered and is the same shape of knob, and the asymmetry is
//! deliberate rather than overlooked: that name was in the owner's real store, so the choice there
//! was between an editable field and a key nobody could change at all. What pays for it is
//! `vike_connections::view::form_note`, which states the condition and the residual beside the
//! field — see that function.
//!
//! # What the gate accepts as an answer
//!
//! Five DERIVED rules and one written table. The rules are derived because each is a fact the tree
//! already states somewhere else and a hand list of them would be one more copy to rot; the table
//! is written because "this is not a credential" is a judgement, and a judgement with no reason
//! beside it is indistinguishable from an omission. [`Rule`] is the enumeration, and every name
//! the universe holds must be answered by exactly one of them, by [`NOT_EDITABLE`], or by being
//! reachable.

use std::collections::{BTreeMap, BTreeSet};

use vike_config::arming::CREDENTIAL_FILE_ARMING_REFUSED;
use vike_connections::view::{Sensitivity, edit_fields, key_sensitivity};
use vike_model::VENUES;
// ⚠ `credential_key` is deliberately NOT imported, and this file deliberately never calls it.
// `crates/vike-ops/tests/settings_registry.rs`'s `generated_key_sites` reads a call to one of the
// grid builders as *this crate READS the whole grid* and then demands a `SETTINGS` row for every
// one of its several hundred names — of `vike-app-core`, which reads none of them. The generic-grid
// membership test below is therefore spelled as a prefix plus a suffix, which is the same string
// that builder would have produced.
use vike_model::credential_keys::{
    CREDENTIAL_SUFFIXES, CREDENTIAL_TIERS, LEGACY_CREDENTIAL_TIERS, PLATFORM_KEYS, lookup_keys,
};
use vike_ops::settings::{SETTINGS, Scope};

/// The three tier labels the Connections grid renders, in column order.
///
/// ⚠ A `static`, not a `const`: every mention of a `const` array materialises a fresh temporary, so
/// `TIERS.iter()` inside a closure borrows a value that dies with the closure body and the iterator
/// it returns cannot escape. `vike_connections::TIERS` is the panel's own spelling and is pinned
/// equal to `vike_model::credential_keys::CREDENTIAL_TIERS`, which is asserted below.
static TIERS: [&str; 3] = ["SIM", "DEMO", "LIVE"];

/// **The venue key PREFIXES.** A roster venue's store names begin with its uppercased id — except
/// polymarket, whose credential prefix is `POLY_` while its roster slug is `polymarket`. That
/// disagreement is not this file's invention: `vike_polymarket::config::load_poly_tier` composes
/// `POLY_{TIER}_…`, `vike_connections::view::edit_fields`' polymarket arm says so in its own
/// comment, and it is why that venue once had a form writing `POLYMARKET_{TIER}_API_KEY`, a name
/// nothing in this tree reads or writes. Both spellings are live in the store, so both are
/// prefixes.
fn prefixes_for(venue: &str) -> Vec<String> {
    let mut out = vec![venue.to_uppercase()];
    if venue == "polymarket" {
        out.push("POLY".to_string());
    }
    out
}

/// Which derived rule excuses a key, when one does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// **The LEGACY tier spelling.** `vike_model::credential_keys::LEGACY_CREDENTIAL_TIERS` —
    /// `MAINNET` where `LIVE` is written now. The loaders still fall back to it so a pre-rename
    /// store keeps working, which is exactly why the grid contains it; the EDITOR must never
    /// author it, for the same reason `credential_keys::starter_keys` excludes it — teaching a
    /// spelling we are trying to retire to somebody setting up a fresh box is a different kind of
    /// wrong from not offering it at all.
    LegacyTier,
    /// **A tier this venue does not HAVE.** `edit_fields` returns an empty list, which is how a row
    /// says *there is no such tier*: `tier_row` then renders no ✎ and `crate::summary::TierState`
    /// renders a middot rather than a hollow ring. A key naming that (venue, tier) can never be
    /// reached because the cell itself does not exist. [`NO_SUCH_TIER`] pins the set both ways, so
    /// this rule cannot quietly widen.
    NoSuchTier,
    /// **The generic grid, SUPERSEDED by a bespoke form.** `load_credentials_from` probes
    /// `{VENUE}_{TIER}_API_{KEY,SECRET,PASSPHRASE}` for every roster venue, so the grid declares
    /// those names everywhere — but ten venues' bridges read something else entirely, and the
    /// editor offering an `ALPACA_SANDBOX_API_KEY` field was the defect
    /// `crates/vike-connections/tests/editor_key_shapes.rs` closed: an operator fills it in, saves,
    /// and it lights nothing because no loader consults it.
    ///
    /// ⚠ The test is per KEY and not per venue, which is what keeps it from excusing a real gap:
    /// a generic key is excused only when that venue's form AT THAT TIER writes at least one name
    /// from OUTSIDE the grid. binance/bybit/okx/deribit write nothing but grid names, so all three
    /// of their generic keys must be reachable and a deleted one reddens; ig and oanda write
    /// `_IDENTIFIER`/`_ACCOUNT_ID` beside a real `_API_KEY`, so only their unused `_API_SECRET`
    /// and `_API_PASSPHRASE` are excused.
    GenericGridSuperseded,
    /// **An ARMING switch, which this file REFUSES rather than merely does not offer.**
    /// `vike_config::arming::CREDENTIAL_FILE_ARMING_REFUSED` is the set whose presence in
    /// `secrets.env` fails startup outright, because that file is plaintext and parsed last-wins
    /// and an appended line must never be enough to put a process on a live venue. A form that
    /// wrote one would not be a useless control — it would be a control that stops the daemon from
    /// booting.
    CredentialFileRefusesIt,
    /// **A PLATFORM key: not a venue credential at all.** `credential_keys::PLATFORM_KEYS` — the
    /// `vike-tradehub` and `vike-datahub` node pairs. They are MINTED from the CSPRNG by
    /// `vike-cli backend setup` / `datahub setup` and pasted by `backend connect`, and
    /// `vike-cli secrets set` refuses both names by name. A venue credential FORM is the wrong
    /// door for a key nobody should ever type: accepting a hand-typed 256-bit HMAC key preserves
    /// the invention step and adds a way to paste a truncated one that fails as an opaque auth
    /// denial. `docs/decisions/0051-node-keys-live-in-their-own-store.md` is the placement record.
    PlatformKey,
}

/// **The (venue, tier) cells that have no form, pinned.** [`Rule::NoSuchTier`] is only safe because
/// this set is fixed: without it, deleting a venue's whole arm would EXCUSE every one of its keys
/// instead of reddening.
///
/// Each row is a cell `crate::status` can never light, with the reason it cannot:
const NO_SUCH_TIER: [(&str, &str, &str); 7] = [
    ("dukascopy", "SIM", "JForex has no sim platform — this venue has one demo tier and no other"),
    ("dukascopy", "LIVE", "the live JForex account is not wired; the bridge mounts demo only"),
    ("aster", "SIM", "aster has a TESTNET and a LIVE tier; `SIM` is neither"),
    ("hyperliquid", "SIM", "hyperliquid has a DEMO (testnet) and a LIVE tier; `SIM` is neither"),
    (
        "alpaca",
        "SIM",
        "`vike_alpaca::config::alpaca_tier` sends BOTH Sim and Demo to the single `SANDBOX` token, \
         so Sim is a second SPELLING of demo rather than a tier of its own",
    ),
    (
        "polymarket",
        "SIM",
        "this venue has no testnet at all — every key it reads signs real money on Polygon mainnet",
    ),
    ("polymarket", "DEMO", "the same: no testnet, so a DEMO form would promise a sandbox"),
];

/// One reason a store key is NOT reachable from the credential editor.
struct NotEditable {
    /// The exact key names this row excuses.
    keys: &'static [&'static str],
    /// Why they must not be a field. A reason, never a restatement of the key name.
    why: &'static str,
}

/// **THE WRITTEN EXEMPTIONS.** Thirteen rows; the declared length IS the count, so a fourteenth
/// cannot arrive without this line changing.
///
/// ⚠ Read the rows, not this paragraph, for what is excused — every prose summary of a table in
/// this repository has rotted. What the rows have in common is the criterion: a key belongs in a
/// venue CREDENTIAL form when writing it configures WHO this deployment is to that venue. A key
/// that configures WHERE the process sends things, WHETHER a behaviour is on, or WHAT a test
/// harness should do does not, however venue-shaped its name is.
const NOT_EDITABLE: [NotEditable; 13] = [
    NotEditable {
        keys: &[
            "BINANCE_BUILDER_CODE",
            "BYBIT_BUILDER_CODE",
            "OKX_BUILDER_CODE",
            "ASTER_BROKER_CODE",
            "HYPERLIQUID_BROKER_CODE",
            "POLYMARKET_BROKER_CODE",
        ],
        why: "THE ALTERNATE SPELLING of a tag the form already offers. \
              `vike_bridge_core::credentials::attribution_code_from` tries `_BROKER_CODE` and then \
              falls back to `_BUILDER_CODE`, so BOTH names are looked up for every mechanised \
              venue and `credential_keys::attribution_keys` declares both — but they are one tag. \
              The form offers the one the venue's MECHANIC is named for (a `SignedBuilder` venue's \
              builder code, a CEX venue's broker code); a second field would be two controls over \
              one value, where filling the wrong one loses silently to the other.",
    },
    NotEditable {
        keys: &["DUKASCOPY_DEMO1_", "DUKASCOPY_DEMO2_", "HYPERLIQUID_DEMO", "HYPERLIQUID_LIVE"],
        why: "NOT A KEY. Each is the `{VENUE}_{ACCOUNT}` PREFIX a loader `format!`s a suffix onto \
              (`vike_dukascopy::config::DukascopyAccount::key_prefix`, hyperliquid's `Env` \
              prefix); the registry's literal harvest saw the fragment and declared it. There is \
              no value to type into any of them.",
    },
    NotEditable {
        keys: &["DUKASCOPY_LOGIN", "DUKASCOPY_PASSWORD", "DUKASCOPY_JNLP"],
        why: "OUTBOUND, not stored. `vike_dukascopy::exec`'s `DukascopyExec::spawn_with_program` \
              SETS these three in the Java sidecar's child environment from the config it already \
              resolved; nothing reads them from the credential store. Offering them would be a \
              form writing a second, unnumbered copy of DEMO1's login that no loader consults.",
    },
    NotEditable {
        keys: &["JFOREX_BRIDGE_JAR"],
        why: "A PATH to the sidecar jar, and the escape hatch that names one outright when the \
              `<project>/bin/jforex` rung is not what you want (README.md documents it for the \
              offline case). A filesystem path is not a credential and does not belong in a store \
              this workspace refuses to rewrite wholesale.",
    },
    NotEditable {
        keys: &["CTRADER_REDIRECT_URI", "CTRADER_SCOPE", "CTRADER_TOKEN_FILE"],
        why: "The OAuth FLOW's own parameters plus the on-disk token cache path, read by \
              `src/bin/ctrader_authorize.rs` while MINTING the grant. The grant is what \
              authenticates and the form offers it (`_ACCESS_TOKEN`/`_REFRESH_TOKEN`) beside the \
              Spotware app pair; the flow that produced it is a one-shot tool's business.",
    },
    NotEditable {
        keys: &["CTRADER_DEMO_ACCOUNT_ID"],
        why: "OPTIONAL and DISCOVERED. \
              `vike_ctrader::config::CtraderConfig::from_vars_with_store_for_account` treats it as \
              optional and resolves the account at connect, so a form offering it would invite an \
              operator to PIN an account id the venue would otherwise hand them — and a pinned \
              wrong one routes orders to another book. `edit_fields`' ctrader arm carries the same \
              argument at the table.",
    },
    NotEditable {
        keys: &[
            "IBKR_DEMO_HOST",
            "IBKR_DEMO_PORT",
            "IBKR_DEMO_CPAPI_URL",
            "IBKR_DEMO_BACKEND",
            "IBKR_DEMO_CLIENT_ID",
            "IBKR_DEMO_DATA_CLIENT_ID",
            "IBKR_DEMO_MKTDATA_TYPE",
        ],
        why: "WHICH GATEWAY, not which account. `vike_ibkr::config::load_ibkr_config_for_account` \
              `?`s on `IBKR_{TIER}_ACCOUNT` alone — the form's one field — and every name here has \
              a DEFAULT, so a present-but-unparseable value turns a working mount into a paper \
              one. They also retarget the connection itself, and the TIER is chosen by the cell \
              the form was opened from: a host typed into the demo cell pointing at a live \
              gateway is the one failure this panel must not make reachable.",
    },
    NotEditable {
        keys: &[
            "POLY_PRIVATE_KEY",
            "POLY_ADDRESS",
            "POLY_FUNDER",
            "POLY_API_KEY",
            "POLY_PASSPHRASE",
            "POLY_RELAYER_API_KEY",
            "POLY_RELAYER_API_KEY_ADDRESS",
        ],
        why: "The TIER-LESS fallback rung of names the form already writes. \
              `vike_polymarket::config::load_poly_tier` reads `POLY_{TIER}_X` FIRST and falls back \
              to `POLY_X` for a pre-tier store; the editor writes the first rung, and authoring \
              the fallback on a fresh box would mint the spelling the fallback exists to retire. \
              (`POLY_FUNDER` is a second fallback again — the alternate spelling of `_ADDRESS`.)",
    },
    NotEditable {
        keys: &["POLY_LIVE_PK"],
        why: "A DIFFERENT NAME for a key the form already writes: `vike-run`'s paper-maker bin \
              reads this abbreviated spelling, while the venue's own loader reads \
              `POLY_LIVE_PRIVATE_KEY`, which is the one the form offers. Two spellings of one \
              secret is a defect to converge, not a second field to type it into twice.",
    },
    NotEditable {
        keys: &["POLY_SIGNATURE", "POLY_SIGNATURE_TYPE", "POLY_NONCE", "POLY_TIMESTAMP"],
        why: "PER-REQUEST signing material, not stored identity: a signature, its type, a nonce \
              and a timestamp are computed for one order and are meaningless in a credential \
              store a week later. They are declared because a one-shot bin can be handed them.",
    },
    NotEditable {
        keys: &[
            "POLY_PROXY_ENABLED",
            "POLY_PROXY_HOST",
            "POLY_PROXY_PORT",
            "POLY_SOCKS_PROXY",
            "POLY_WS_PROXY_ENABLED",
            "POLY_CHAIN_PROXY",
            "POLY_CHAIN_RPC_URL",
            "POLY_EGRESS_PROBE_URL",
            "POLY_EXPECT_EGRESS_COUNTRY",
            "POLY_GEOBLOCK_OVERRIDE",
        ],
        why: "EGRESS: where this process's packets leave from and what it expects to find when \
              they arrive. Deployment topology, identical for every account on the box, and \
              nothing an operator configures per venue account.",
    },
    NotEditable {
        keys: &[
            "POLY_AUTO_REDEEM",
            "POLY_REDEEM_HALT",
            "POLY_HEARTBEAT",
            "POLY_RATE_GATE",
            "POLY_PRESUBMIT_REGISTER",
            "POLY_EXEC_MARKETS",
            "POLY_REWARD_WEIGHT",
            "POLY_WS_TOKENS_PER_SOCKET",
            "POLY_CHAIN_WATCH",
            "POLY_CHAIN_MAX_SPAN",
            "HYPERLIQUID_HIP3",
        ],
        why: "BEHAVIOUR toggles and tuning: what the venue plane DOES once it is armed, not who \
              it is. `vike_config::arming` deliberately does not refuse these (a refusal list that \
              fires on harmless lines trains operators around it) and this table deliberately does \
              not offer them (a credential form that doubles as a settings screen teaches that \
              `secrets.env` is where behaviour is configured, which is what `settings/*.toml` is \
              for).",
    },
    NotEditable {
        keys: &[
            "ASTER_SMOKE_ORDER",
            "ASTER_SOAK_ALLOW_MAINNET",
            "DUKASCOPY_SMOKE_ACCOUNT",
            "EOD_SMOKE",
            "PMXT_SMOKE",
            "POLY_FILL_SMOKE",
            "POLY_GAMMA_SMOKE",
            "POLY_REDEEM_SMOKE",
            "POLY_CANCEL_ORDER_ID",
            "POLY_CHAIN_FROM",
            "POLY_CHAIN_TO",
        ],
        why: "SMOKE-HARNESS inputs — which ignored test to run, which order id to cancel, which \
              block range to sweep. They are venue-scoped names read from `tests/` and one-shot \
              bins; an operator configuring a venue has no use for any of them. This row is what \
              the `Layer::TestOnly` filter this gate refuses to apply would have hidden.",
    },
];

/// Every key name the credential editor can write, over the whole roster and every tier.
fn reachable() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for venue in VENUES {
        for tier in TIERS {
            out.extend(edit_fields(venue, tier).into_iter().map(|(_, k)| k));
        }
    }
    out
}

/// The universe this gate folds over — see the module doc for why it is the registry and not the
/// key grid, and why no `Layer` filter is applied.
fn universe() -> BTreeSet<String> {
    let mut out: BTreeSet<String> =
        SETTINGS.iter().filter(|s| s.scope == Scope::Venue).map(|s| s.name.to_string()).collect();
    out.extend(lookup_keys());
    out.extend(PLATFORM_KEYS.iter().map(|k| (*k).to_string()));
    out
}

/// The derived rule that excuses `key`, if any.
fn rule_for(key: &str) -> Option<Rule> {
    if PLATFORM_KEYS.contains(&key) {
        return Some(Rule::PlatformKey);
    }
    if CREDENTIAL_FILE_ARMING_REFUSED.iter().any(|s| s.var == key) {
        return Some(Rule::CredentialFileRefusesIt);
    }
    for venue in VENUES {
        for prefix in prefixes_for(venue) {
            // ⚠ The trailing `_` is load-bearing: `HYPERLIQUID_MAINNET` is an ARMING SWITCH, not a
            // legacy-tier key, and without it this rule would swallow the switch and rob
            // `Rule::CredentialFileRefusesIt` of the name it exists for.
            if LEGACY_CREDENTIAL_TIERS.iter().any(|t| key.starts_with(&format!("{prefix}_{t}_"))) {
                return Some(Rule::LegacyTier);
            }
            for tier in CREDENTIAL_TIERS {
                if !key.starts_with(&format!("{prefix}_{tier}_")) {
                    continue;
                }
                let fields = edit_fields(venue, tier);
                if fields.is_empty() {
                    return Some(Rule::NoSuchTier);
                }
                let head = format!("{}_{tier}", venue.to_uppercase());
                let in_grid = |k: &str| {
                    k.strip_prefix(&head).is_some_and(|rest| CREDENTIAL_SUFFIXES.contains(&rest))
                };
                if in_grid(key) && fields.iter().any(|(_, k)| !in_grid(k)) {
                    return Some(Rule::GenericGridSuperseded);
                }
            }
        }
    }
    None
}

/// Every key excused by a written [`NOT_EDITABLE`] row.
fn written_exemptions() -> BTreeSet<String> {
    NOT_EDITABLE.iter().flat_map(|r| r.keys.iter().map(|k| (*k).to_string())).collect()
}

/// **THE GATE.** Every name the store can hold is reachable from the editor, excused by a derived
/// [`Rule`], or excused by a written [`NOT_EDITABLE`] row — and nothing else is an answer.
///
/// The failure message names each offender with its venue prefix, because the repair is always one
/// of two things and the name says which: add it to `edit_fields`' arm for that venue, or add it to
/// [`NOT_EDITABLE`] with the reason it must not be a field.
#[test]
fn every_store_key_is_reachable_from_the_editor_or_excused_in_writing() {
    let reachable = reachable();
    let written = written_exemptions();
    let mut orphans: Vec<String> = Vec::new();
    for key in universe() {
        if reachable.contains(&key) || written.contains(&key) || rule_for(&key).is_some() {
            continue;
        }
        orphans.push(key);
    }
    assert!(
        orphans.is_empty(),
        "{} store key(s) are reachable from NO credential form and excused by nothing. Either add \
         each to `vike_connections::view::edit_fields`' arm for its venue, or add it to \
         `NOT_EDITABLE` with the reason it must not be a field:\n  {}",
        orphans.len(),
        orphans.join("\n  ")
    );
}

/// **The other direction: no STALE row.** A `NOT_EDITABLE` entry naming a key the store can no
/// longer hold, or one the editor has since started offering, is a reason nobody reads attached to
/// nothing — and the second case is worse than untidy, because the row would go on excusing a
/// field that had been silently deleted again.
#[test]
fn no_written_exemption_is_stale() {
    let universe = universe();
    let reachable = reachable();
    let mut stale: Vec<String> = Vec::new();
    for row in &NOT_EDITABLE {
        for key in row.keys {
            if !universe.contains(*key) {
                stale.push(format!(
                    "{key}: no longer a declared venue-scoped name — delete the row"
                ));
            }
            if reachable.contains(*key) {
                stale.push(format!(
                    "{key}: the editor now OFFERS this key — delete the row, it excuses nothing"
                ));
            }
        }
    }
    assert!(stale.is_empty(), "stale `NOT_EDITABLE` row(s):\n  {}", stale.join("\n  "));
}

/// **Every row excuses a DISTINCT key, and the reasons are real sentences.** A duplicated key would
/// make the count above meaningless (two rows, one fact) and an empty reason would make a row a
/// silent omission wearing a table's clothes.
#[test]
fn every_exemption_row_carries_a_distinct_key_and_a_reason() {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, row) in NOT_EDITABLE.iter().enumerate() {
        assert!(!row.keys.is_empty(), "row {i} excuses nothing");
        assert!(row.why.len() > 60, "row {i}'s reason is too short to be one: {:?}", row.why);
        for key in row.keys {
            let first = seen.insert(*key, i);
            assert!(first.is_none(), "{key} is excused twice — rows {first:?} and {i}");
        }
    }
}

/// **`NO_SUCH_TIER` is pinned BOTH WAYS.** [`Rule::NoSuchTier`] excuses every key naming a cell
/// with no form, so a deleted `edit_fields` arm would turn a whole venue's keys into "excused"
/// rather than into a failure. Holding the pinned set equal to the computed one is what stops that:
/// a new empty cell reddens here until somebody writes down why the tier does not exist, and a
/// pinned row whose form came back reddens too.
#[test]
fn the_cells_with_no_form_are_exactly_the_pinned_ones() {
    let pinned: BTreeSet<(&str, &str)> = NO_SUCH_TIER.iter().map(|(v, t, _)| (*v, *t)).collect();
    let mut computed: BTreeSet<(&str, &str)> = BTreeSet::new();
    for venue in VENUES {
        for tier in TIERS {
            if edit_fields(venue, tier).is_empty() {
                computed.insert((*venue, tier));
            }
        }
    }
    assert_eq!(
        computed, pinned,
        "the (venue, tier) cells offering no form moved. A cell that gained a form leaves a stale \
         row; a cell that LOST one is a venue whose keys just became silently excused — write down \
         why the tier does not exist, or put the arm back."
    );
    for (venue, tier, why) in NO_SUCH_TIER {
        assert!(why.len() > 30, "{venue}/{tier} carries no real reason: {why:?}");
    }
    // ...and this file's own tier spelling is the shared one, so every fold above walks the same
    // three columns the panel and the loaders do.
    assert_eq!(TIERS.to_vec(), CREDENTIAL_TIERS.to_vec(), "the tier labels are the shared table's");
}

/// **THE MEASURED DEFECT, stated as a test.** Both dukascopy `_SERVER` names were in the owner's
/// real store and reachable from no form. This is the assertion that reddens if either is removed
/// from the table again — the gate above would too, but it would name a key and not a story.
#[test]
fn the_dukascopy_server_names_that_were_unreachable_are_reachable() {
    let keys: Vec<String> = edit_fields("dukascopy", "DEMO").into_iter().map(|(_, k)| k).collect();
    for want in ["DUKASCOPY_DEMO1_SERVER", "DUKASCOPY_DEMO2_SERVER"] {
        assert!(
            keys.contains(&want.to_string()),
            "{want} is in the owner's store and in `vike_ops::settings::SETTINGS`, and the DEMO \
             form does not offer it: {keys:?}"
        );
    }
}

/// **SENSITIVITY IS DECIDED FOR EVERY FIELD THE EDITOR OFFERS, and it FAILS CLOSED.**
///
/// ⚠ The check that matters is the second one: a key whose suffix appears in neither of
/// `vike_connections::view`'s tables is classified `Secret`, so a field added tomorrow is masked
/// and never read back until somebody deliberately classifies it. This test proves the default is
/// reached rather than trusting the constant — an unknown name really does come back `Secret`.
///
/// The first one pins the two ends an operator would notice: an API secret is masked, and a server
/// name and an account number are not. Those are the fields the owner's report was about — a masked
/// field that starts empty is right for a key and is exactly how an endpoint set six months ago
/// becomes unknowable from inside the app that wrote it.
#[test]
fn the_sensitivity_rule_fails_closed_and_puts_names_on_the_readable_side() {
    for (key, want) in [
        ("BINANCE_LIVE_API_KEY", Sensitivity::Secret),
        ("BINANCE_LIVE_API_SECRET", Sensitivity::Secret),
        ("DUKASCOPY_DEMO1_PASSWORD", Sensitivity::Secret),
        ("POLY_LIVE_PRIVATE_KEY", Sensitivity::Secret),
        ("CTRADER_LIVE_ACCESS_TOKEN", Sensitivity::Secret),
        ("ALPACA_LIVE_CLIENT_SECRET", Sensitivity::Secret),
        ("POLY_LIVE_RELAYER_API_KEY", Sensitivity::Secret),
        // ...and the readable half: a server, an account, an address, an attribution tag.
        ("DUKASCOPY_DEMO1_SERVER", Sensitivity::Public),
        ("DUKASCOPY_DEMO1_LOGIN", Sensitivity::Public),
        ("IBKR_LIVE_ACCOUNT", Sensitivity::Public),
        ("ALPACA_LIVE_CLIENT_ID", Sensitivity::Public),
        ("HYPERLIQUID_LIVE_ACCOUNT_ADDRESS", Sensitivity::Public),
        ("POLY_LIVE_RELAYER_API_KEY_ADDRESS", Sensitivity::Public),
        ("BINANCE_BROKER_CODE", Sensitivity::Public),
        ("HYPERLIQUID_BUILDER_CODE", Sensitivity::Public),
        ("ASTER_BUILDER_FEE_RATE", Sensitivity::Public),
        // ...and a LABELLED account's key answers the same way its unlabelled twin does.
        ("DUKASCOPY_DEMO1_SERVER__ALT", Sensitivity::Public),
        ("BINANCE_LIVE_API_KEY__ALT", Sensitivity::Secret),
    ] {
        assert_eq!(key_sensitivity(key), want, "{key}");
    }

    // FAIL CLOSED: a name nobody has classified is a secret, not a readable field.
    for unknown in ["BINANCE_LIVE_SOMETHING_NEW", "VENUE_TIER_UNCLASSIFIED", "WAT"] {
        assert_eq!(
            key_sensitivity(unknown),
            Sensitivity::Secret,
            "{unknown} must fail CLOSED — an unclassified field may not be rendered in the clear"
        );
    }
}
