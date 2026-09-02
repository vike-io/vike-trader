//! **Every venue's FORM composes the key names its own bridge reads** — the write-side twin of
//! `crates/vike-connections/tests/venue_key_shapes.rs`, and the byte-identity guard over the venues
//! this branch did not touch.
//!
//! # The defect this closes
//!
//! `crates/vike-connections/src/status.rs` grew per-venue arms so alpaca/ctrader/ibkr/polymarket
//! report credential status from the keys their loaders actually read. The WRITE side kept falling
//! through to the generic `{VENUE}_{TIER}_API_KEY`/`_API_SECRET` grid, so the editor offered an
//! `ALPACA_SANDBOX_API_KEY` field an operator could fill in, save, and watch light nothing —
//! honest, now that the dots read the right keys, and still the wrong form. Before the read-side
//! repair the two halves were coherently wrong TOGETHER (write generic, read generic, dot lights),
//! which is why this was invisible until that landed.
//!
//! # Why an integration test rather than a `#[cfg(test)]` block in `view.rs`
//!
//! `crates/vike-ops/tests/settings_registry.rs`'s `every_read_variable_is_declared` harvests
//! env-var-shaped literals out of `src/` and demands a `vike_ops::settings::SETTINGS` row for each.
//! A key a form COMPOSES (`format!("ALPACA_{tier}_CLIENT_ID")`) appears as no literal anywhere, so
//! `SETTINGS` carries `ALPACA_LIVE_CLIENT_ID`, `IBKR_DEMO_ACCOUNT`, `CTRADER_SIM_ACCESS_TOKEN` and
//! `POLY_LIVE_PRIVATE_KEY` and no `vike-connections` row for any of them. Spelling one of those in
//! a `src/` fixture would demand a row asserting a read this crate does not perform — it composes
//! the name too. A `tests/` file is a test region, where that literal sweep deliberately does not
//! look. `status.rs`' module doc carries the same argument for the read side, and
//! `crates/vike-connections/src/view.rs`'s `edit_fields` is `pub` for exactly this reason.
//!
//! # What is gated
//!
//! 1. **The four repaired venues** compose their loaders' key names, per TIER and per LABELLED
//!    account, one test each, each citing the loader it was verified against.
//! 2. **The generic grid is gone for those four** — the defect itself, stated as the property that
//!    fails without the fix, and doubling as the proof that [`baseline`] is a different function.
//! 3. **The tiers that must offer NO form at all** — alpaca `SIM`, polymarket `SIM`/`DEMO`.
//! 4. **Every other venue is byte-identical**, as an EQUALITY against [`baseline`] — a frozen copy
//!    of the pre-repair tables. Not a pin of today's answer: both sides are computed.
//! 5. **The round trip** — what the editor composes for (venue, tier, account) is what the grid
//!    then reads for that same (venue, tier, account), and for nothing else.

use std::collections::HashMap;

use vike_connections::view::{account_expected_key_name, account_fields};
use vike_connections::{credential_status_for_account, VenueCredStatus, VENUES};
use vike_model::account_keys::{account_key, AccountLabel};

/// The venues this branch changed. Every other roster venue must compose exactly what [`baseline`]
/// composes, and that is the whole content of [`untouched_venues_compose_the_same_keys`].
const REPAIRED: &[&str] = &["alpaca", "ctrader", "ibkr", "polymarket"];

/// The three tier labels the grid renders, in column order.
const TIERS: &[&str] = &["SIM", "DEMO", "LIVE"];

/// **The FROZEN pre-repair `expected_key_name` / `edit_fields`**, copied arm for arm from
/// `batch/accounts-status-capture` so the byte-identity claim is an equality between two
/// computations rather than a table of expected strings somebody could have written to match
/// whatever the code now does.
///
/// ⚠ It is a BASELINE, not a duplicate implementation: its job is to state what the editor composed
/// BEFORE, permanently. A future PR that deliberately changes an untouched venue's arm must edit
/// the matching arm here in the same commit — the failure is the point, because "a venue nobody
/// meant to touch moved" is exactly what these tests exist to notice.
mod baseline {
    pub fn expected_key_name(venue: &str, env_label: &str) -> String {
        match venue {
            "fxcm" => format!("FXCM_{env_label}_USER"),
            "oanda" => format!("OANDA_{env_label}_API_KEY"),
            "ig" => format!("IG_{env_label}_API_KEY"),
            "dukascopy" if env_label == "DEMO" => "DUKASCOPY_DEMO1_LOGIN".to_string(),
            "aster" => {
                let tier = if env_label == "DEMO" { "TESTNET" } else { env_label };
                format!("ASTER_{tier}_USER")
            }
            "hyperliquid" => format!("HYPERLIQUID_{env_label}_PRIVATE_KEY"),
            _ => format!("{}_{}_API_KEY", venue.to_uppercase(), env_label),
        }
    }

    pub fn edit_fields(venue: &str, env_label: &str) -> Vec<(&'static str, String)> {
        match venue {
            "fxcm" => vec![
                ("User", format!("FXCM_{env_label}_USER")),
                ("Password", format!("FXCM_{env_label}_PASSWORD")),
            ],
            "oanda" => vec![
                ("API Key", format!("OANDA_{env_label}_API_KEY")),
                ("Account ID", format!("OANDA_{env_label}_ACCOUNT_ID")),
            ],
            "ig" => vec![
                ("API Key", format!("IG_{env_label}_API_KEY")),
                ("Identifier", format!("IG_{env_label}_IDENTIFIER")),
                ("Password", format!("IG_{env_label}_PASSWORD")),
            ],
            "dukascopy" if env_label == "DEMO" => vec![
                ("Login (DEMO1)", "DUKASCOPY_DEMO1_LOGIN".to_string()),
                ("Password (DEMO1)", "DUKASCOPY_DEMO1_PASSWORD".to_string()),
                ("Login (DEMO2)", "DUKASCOPY_DEMO2_LOGIN".to_string()),
                ("Password (DEMO2)", "DUKASCOPY_DEMO2_PASSWORD".to_string()),
            ],
            "dukascopy" => Vec::new(),
            "aster" => match env_label {
                "DEMO" => vec![
                    ("User", "ASTER_TESTNET_USER".to_string()),
                    ("Private Key", "ASTER_TESTNET_PRIVATE_KEY".to_string()),
                    ("Signer (optional)", "ASTER_TESTNET_SIGNER".to_string()),
                ],
                "LIVE" => vec![
                    ("User", "ASTER_LIVE_USER".to_string()),
                    ("Private Key", "ASTER_LIVE_PRIVATE_KEY".to_string()),
                    ("Signer (optional)", "ASTER_LIVE_SIGNER".to_string()),
                ],
                _ => Vec::new(),
            },
            "hyperliquid" => match env_label {
                "DEMO" | "LIVE" => vec![
                    ("Private Key", format!("HYPERLIQUID_{env_label}_PRIVATE_KEY")),
                    (
                        "Account Address (optional)",
                        format!("HYPERLIQUID_{env_label}_ACCOUNT_ADDRESS"),
                    ),
                ],
                _ => Vec::new(),
            },
            _ => vec![
                ("API Key", format!("{}_{}_API_KEY", venue.to_uppercase(), env_label)),
                ("API Secret", format!("{}_{}_API_SECRET", venue.to_uppercase(), env_label)),
                (
                    "Passphrase (optional)",
                    format!("{}_{}_API_PASSPHRASE", venue.to_uppercase(), env_label),
                ),
            ],
        }
    }
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The two accounts every table below is folded over: the one a single-account box has, and a
/// labelled one. A key that forgot the label is the defect the account grammar exists to prevent,
/// and it is invisible from the default account alone.
fn accounts() -> Vec<AccountLabel> {
    vec![AccountLabel::Default, alt()]
}

/// Just the key names a cell's form would write, in field order.
fn keys(venue: &str, tier: &str, account: &AccountLabel) -> Vec<String> {
    account_fields(venue, tier, account).into_iter().map(|(_, k)| k).collect()
}

/// The same, for [`baseline`] — the label composed by the SAME grammar, so the equality below is
/// about the TABLE and not about the account dimension (which `account_editor.rs` already gates).
fn baseline_keys(venue: &str, tier: &str, account: &AccountLabel) -> Vec<String> {
    baseline::edit_fields(venue, tier).into_iter().map(|(_, k)| account_key(&k, account)).collect()
}

fn row(grid: &[VenueCredStatus], venue: &str) -> VenueCredStatus {
    grid.iter()
        .find(|s| s.venue == venue)
        .unwrap_or_else(|| panic!("{venue} is not in the roster"))
        .clone()
}

/// One venue's three columns, in `Sim`/`Demo`/`Live` order — the shape every expectation below is
/// written in, so a wrong column cannot be read as the right one.
fn tiers(s: &VenueCredStatus) -> (bool, bool, bool) {
    (s.sim, s.demo, s.live)
}

/// **THE byte-identity gate.** Every venue outside [`REPAIRED`] must compose exactly the key names
/// the pre-repair tables composed — every tier, both the tooltip's name and the form's field list,
/// for the default account and for a labelled one.
///
/// An EQUALITY between two computations, deliberately: a table of expected strings would be a pin
/// of whatever this code does today and would survive a rewrite that changed every one of them
/// together.
#[test]
fn untouched_venues_compose_the_same_keys() {
    let mut compared_fields = 0usize;
    for &venue in VENUES {
        if REPAIRED.contains(&venue) {
            continue;
        }
        for &tier in TIERS {
            for account in accounts() {
                assert_eq!(
                    account_expected_key_name(venue, tier, &account),
                    account_key(&baseline::expected_key_name(venue, tier), &account),
                    "{venue}/{tier} (account {account}): the tooltip's key name moved"
                );
                let now = keys(venue, tier, &account);
                assert_eq!(
                    now,
                    baseline_keys(venue, tier, &account),
                    "{venue}/{tier} (account {account}): the form's field list moved"
                );
                compared_fields += now.len();
            }
        }
    }

    // ...and the comparison must not have been two empty lists all the way down. Every untouched
    // venue offers a form on at least one tier, and the total field count is non-trivial — without
    // this, deleting every arm above would read green.
    for &venue in VENUES {
        if REPAIRED.contains(&venue) {
            continue;
        }
        assert!(
            TIERS.iter().any(|&t| !keys(venue, t, &AccountLabel::Default).is_empty()),
            "{venue} offers a form on no tier at all — the equality above proves nothing for it"
        );
    }
    assert!(
        compared_fields > 50,
        "only {compared_fields} field name(s) were compared — the fold has gone quiet and the \
         byte-identity claim rests on almost nothing"
    );
}

/// **The defect itself.** None of the four composes a generic `{VENUE}_{TIER}_API_KEY`/
/// `_API_SECRET`/`_API_PASSPHRASE` name any more — those names are read by no loader under
/// `crates/bridges/{alpaca,ctrader,vike-ibkr,polymarket}/`, so a form offering one asks an operator
/// for a key nothing will ever read.
///
/// Doubles as the anti-vacuity half of [`untouched_venues_compose_the_same_keys`]: it shows the
/// pre-repair tables composing exactly those names, so [`baseline`] is demonstrably a DIFFERENT
/// function and not an accidental alias of the new one.
#[test]
fn the_generic_api_key_grid_is_gone_for_the_four() {
    for &venue in REPAIRED {
        for &tier in TIERS {
            let generic = format!("{}_{tier}_API_KEY", venue.to_uppercase());
            let now = keys(venue, tier, &AccountLabel::Default);
            assert!(
                !now.contains(&generic),
                "{venue}/{tier} still offers {generic}, a name its bridge loader never reads"
            );
            assert!(
                now.iter().all(|k| !k.contains("_API_SECRET") && !k.contains("_API_PASSPHRASE")),
                "{venue}/{tier} still offers a generic API secret/passphrase field: {now:?}"
            );

            let before = baseline_keys(venue, tier, &AccountLabel::Default);
            assert!(
                before.contains(&generic),
                "the pre-repair table must have offered {generic} — otherwise this test is not \
                 measuring the defect it names"
            );
        }
    }
}

/// **A tier the grid can never light must offer no form at all.** `status_cell` renders the ✎ only
/// when the field list is non-empty, so an empty list is how a cell says *there is no such tier*.
/// Offering one is the read-side defect in reverse: the operator fills it in, saves, and no dot
/// appears — which is exactly as misleading as a dot beside a venue that will stay on paper.
///
/// alpaca `SIM`: `vike_alpaca::config::alpaca_tier` sends both `Sim` and `Demo` to the single
/// `SANDBOX` token, so `Sim` is a second SPELLING of demo, not a tier of its own.
/// polymarket `SIM`/`DEMO`: that venue has no testnet — every key it reads signs real money on
/// Polygon mainnet (`crates/bridges/polymarket/CLAUDE.md`).
///
/// Both mirror `crate::status`' own answers, and the pairing is asserted rather than described:
/// each cell named here reads `false` in the grid under a store that configures the venue as fully
/// as it can be configured.
#[test]
fn a_tier_the_grid_can_never_light_offers_no_form() {
    for (venue, tier) in [("alpaca", "SIM"), ("polymarket", "SIM"), ("polymarket", "DEMO")] {
        for account in accounts() {
            assert!(
                account_fields(venue, tier, &account).is_empty(),
                "{venue}/{tier} (account {account}) offers a form for a tier that cannot light"
            );
        }
    }

    // ...and the grid really cannot light them, under a store holding every key either venue reads.
    let vars = vars_of(&[
        ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
        ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
        ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
        ("ALPACA_LIVE_CLIENT_ID", "cid"),
        ("ALPACA_LIVE_CLIENT_SECRET", "csec"),
        ("ALPACA_LIVE_ACCOUNT_ID", "acct"),
        ("POLY_LIVE_PRIVATE_KEY", "0xk"),
        ("POLY_PRIVATE_KEY", "0xk"),
    ]);
    let grid = credential_status_for_account(&vars, &AccountLabel::Default);
    assert!(!row(&grid, "alpaca").sim, "alpaca SIM must not light — the form offering none is why");
    assert_eq!(tiers(&row(&grid, "polymarket")), (false, false, true), "polymarket is LIVE-only");
}

fn vars_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// Alpaca: the OAuth2 client-credentials pair plus the PINNED account, under the BRIDGE's own tier
/// token (`SANDBOX` for the app's Demo column, `LIVE` for Live) — verified against
/// `vike_alpaca::config::load_alpaca_config_for_account` / `alpaca_tier`
/// (`crates/bridges/alpaca/src/config.rs`), whose live gate is exactly "all three present and
/// non-blank". `_API_KEY`/`_API_SECRET` are names that loader never asks for.
#[test]
fn alpaca_composes_the_oauth_trio_under_its_own_tier_token() {
    for (cell, tier) in [("DEMO", "SANDBOX"), ("LIVE", "LIVE")] {
        assert_eq!(
            keys("alpaca", cell, &AccountLabel::Default),
            vec![
                format!("ALPACA_{tier}_CLIENT_ID"),
                format!("ALPACA_{tier}_CLIENT_SECRET"),
                format!("ALPACA_{tier}_ACCOUNT_ID"),
            ],
            "alpaca/{cell} must write the bridge's own {tier} tier"
        );
        assert_eq!(
            keys("alpaca", cell, &alt()),
            vec![
                format!("ALPACA_{tier}_CLIENT_ID__ALT"),
                format!("ALPACA_{tier}_CLIENT_SECRET__ALT"),
                format!("ALPACA_{tier}_ACCOUNT_ID__ALT"),
            ],
            "alpaca/{cell}: a labelled account's keys carry the label"
        );
    }

    // The DEMO cell must not write `ALPACA_DEMO_*` — the app's own tier spelling, which that
    // loader reads under no circumstances.
    assert!(
        keys("alpaca", "DEMO", &AccountLabel::Default)
            .iter()
            .all(|k| !k.starts_with("ALPACA_DEMO")),
        "the DEMO cell must write the SANDBOX tier, not the app's own label"
    );
}

/// cTrader: the TIER-LESS Spotware app registration plus THIS tier's OAuth grant — verified against
/// `vike_ctrader::config::CtraderConfig::from_vars_with_store_for_account`
/// (`crates/bridges/ctrader/src/config.rs`), which takes exactly those four through `?`.
/// `CTRADER_{TIER}_ACCOUNT_ID` is OPTIONAL there (discovered at connect) and is deliberately not
/// offered.
///
/// ⚠ **The app pair is LABELLED too, with no fallback.** That loader's doc argues it: an account is
/// configured by its own keys, all of them, so an operator running two cTrader accounts through one
/// app registration writes the pair twice. A form that wrote the UNLABELLED pair from a labelled
/// cell would configure the DEFAULT account instead — which is the trap a tier-less key sets, and
/// the reason this venue is the one that needed the mechanism thought about rather than reused.
#[test]
fn ctrader_composes_the_app_pair_plus_the_per_tier_grant() {
    for &tier in TIERS {
        assert_eq!(
            keys("ctrader", tier, &AccountLabel::Default),
            vec![
                format!("CTRADER_{tier}_ACCESS_TOKEN"),
                format!("CTRADER_{tier}_REFRESH_TOKEN"),
                "CTRADER_CLIENT_ID".to_string(),
                "CTRADER_CLIENT_SECRET".to_string(),
            ],
            "ctrader/{tier}"
        );
        assert_eq!(
            keys("ctrader", tier, &alt()),
            vec![
                format!("CTRADER_{tier}_ACCESS_TOKEN__ALT"),
                format!("CTRADER_{tier}_REFRESH_TOKEN__ALT"),
                "CTRADER_CLIENT_ID__ALT".to_string(),
                "CTRADER_CLIENT_SECRET__ALT".to_string(),
            ],
            "ctrader/{tier}: the TIER-LESS app pair takes the label too, or the form configures \
             the default account from a labelled cell"
        );
    }
}

/// IBKR: the account number, and nothing else — verified against
/// `vike_ibkr::config::load_ibkr_config_for_account` (`crates/bridges/vike-ibkr/src/config.rs`),
/// whose only `?` on an absent value is `IBKR_{TIER}_ACCOUNT`. The socket API has no in-crate auth
/// (the Gateway holds the login), so `_API_KEY`/`_API_SECRET` name nothing on this venue; every
/// other name that loader reads has a default, so offering one could only turn a working mount into
/// a paper one. All three tiers are real: it spells `Environment::as_str` verbatim.
#[test]
fn ibkr_composes_the_account_number_and_nothing_else() {
    for &tier in TIERS {
        assert_eq!(
            keys("ibkr", tier, &AccountLabel::Default),
            vec![format!("IBKR_{tier}_ACCOUNT")],
            "ibkr/{tier}"
        );
        assert_eq!(
            keys("ibkr", tier, &alt()),
            vec![format!("IBKR_{tier}_ACCOUNT__ALT")],
            "ibkr/{tier}: a labelled account's key carries the label"
        );
    }
}

/// Polymarket: the L1 Ethereum private key that signs and derives the L2 trio, under the venue's
/// own `POLY_` prefix — verified against `vike_polymarket::config::load_polymarket_creds_for_account`
/// / `load_poly_tier` (`crates/bridges/polymarket/src/config.rs`), whose gate is exactly "the
/// private key is present and non-blank". The funder/deposit-wallet address is optional there.
///
/// ⚠ The form writes the TIER-SUFFIXED spelling (`POLY_LIVE_PRIVATE_KEY`), which is that loader's
/// FIRST rung. Its other two rungs — the legacy `POLY_MAINNET_*` tier and the tier-less
/// `POLY_PRIVATE_KEY` the real store uses — are read-compatibility fallbacks, and writing one of
/// them from here would make the editor produce the pre-rename spelling on a fresh box.
///
/// ⚠ The prefix is `POLY_`, not `POLYMARKET_`: the roster slug and the credential prefix disagree,
/// which is exactly why this venue fell through to a `POLYMARKET_{TIER}_API_KEY` form nothing in
/// the tree writes or reads.
#[test]
fn polymarket_composes_the_l1_private_key_on_the_live_cell_only() {
    assert_eq!(
        keys("polymarket", "LIVE", &AccountLabel::Default),
        vec!["POLY_LIVE_PRIVATE_KEY".to_string(), "POLY_LIVE_ADDRESS".to_string()],
    );
    assert_eq!(
        keys("polymarket", "LIVE", &alt()),
        vec!["POLY_LIVE_PRIVATE_KEY__ALT".to_string(), "POLY_LIVE_ADDRESS__ALT".to_string()],
        "the tier-less fallback rung is a TIER fallback, never an ACCOUNT one — a labelled cell \
         must never write a name another account reads"
    );
    for &tier in &["SIM", "DEMO"] {
        assert!(keys("polymarket", tier, &AccountLabel::Default).is_empty(), "{tier} has no form");
    }
}

/// **THE ROUND TRIP, and the test that actually proves the two halves agree.** For every venue,
/// every tier and both accounts: fill in exactly the fields the editor offers, hand the result to
/// the grid, and require that cell — and only that cell — to light.
///
/// ⚠ The other end of the chain is `crates/vike-connections/tests/venue_key_shapes.rs`, which pins
/// each of the grid's per-venue arms against the BRIDGE LOADER it was read from. This file
/// deliberately does not reach for those loaders itself: three of the four sit above this crate in
/// the layer order and one of them (`vike-polymarket`) is an EMPTY crate without a cargo feature,
/// so a dev-dependency here would drag that venue's whole feed plane into every roster build of
/// vike-connections to assert something the read-side file already asserts. Editor → grid here,
/// grid → loader there; neither link is assumed.
///
/// Every offered field is filled, optional ones included. That is not laziness about which are
/// required — `venue_key_shapes.rs` proves each required key one at a time — it is what makes the
/// claim "a form an operator fills in lights its dot" true for OKX, whose passphrase the shared
/// `load_credentials_from` requires while the generic form still labels it optional.
#[test]
fn what_the_editor_composes_is_what_the_grid_reads() {
    let mut round_trips = 0usize;
    for &venue in VENUES {
        for &tier in TIERS {
            for account in accounts() {
                let fields = account_fields(venue, tier, &account);
                if fields.is_empty() {
                    continue; // no such tier — `a_tier_the_grid_can_never_light_offers_no_form`
                }
                let vars: HashMap<String, String> =
                    fields.iter().map(|(_, k)| (k.clone(), "filled".to_string())).collect();

                let grid = credential_status_for_account(&vars, &account);
                let lit = tiers(&row(&grid, venue));
                let expected = (tier == "SIM", tier == "DEMO", tier == "LIVE");
                assert_eq!(
                    lit,
                    expected,
                    "{venue}/{tier} (account {account}): a store holding exactly what this cell's \
                     form writes must light that cell and no other. Composed: {:?}",
                    vars.keys().collect::<Vec<_>>()
                );

                // ...and a LABELLED form must not have configured the DEFAULT account, which is
                // what writing an unlabelled key name from a labelled cell would look like.
                if !account.is_default() {
                    let default_grid = credential_status_for_account(&vars, &AccountLabel::Default);
                    assert_eq!(
                        tiers(&row(&default_grid, venue)),
                        (false, false, false),
                        "{venue}/{tier}: the ALT form's keys configured the DEFAULT account"
                    );
                }
                round_trips += 1;
            }
        }
    }
    assert!(
        round_trips > 30,
        "only {round_trips} (venue, tier, account) cell(s) round-tripped — the fold has gone quiet"
    );
}

/// **A cell that offers NO form must name a key no form anywhere writes** — the property
/// `expected_key_name`'s own doc claims for those cells, asserted rather than described.
///
/// # The hole this closes, and how it was found
///
/// Every other assertion about a tooltip name is reached through one of two doors, and a formless
/// cell of a REPAIRED venue fits through neither. `view.rs`'
/// `a_cells_tooltip_names_the_same_account_key_its_form_writes` compares the hovered name to the
/// form's FIRST field and `continue`s when there is no field to compare against — precisely these
/// cells. [`untouched_venues_compose_the_same_keys`] does assert every tier's tooltip, form or no
/// form, but skips [`REPAIRED`]. So alpaca `SIM` and polymarket `SIM`/`DEMO` had their hovered key
/// name asserted by nothing at all, and a mutation run proved it: pointing polymarket's two dead
/// cells at the real `POLY_LIVE_PRIVATE_KEY`, and alpaca `SIM` at demo's real
/// `ALPACA_SANDBOX_CLIENT_ID`, each left the whole crate green.
///
/// # Why that is worth a test rather than a shrug
///
/// A dot that can never light, hovering the name of a key that IS read, invites the operator to
/// write a live key from a cell whose form would not have offered it — the `SIM` column of a venue
/// that has no sandbox naming the mainnet signing key. It is the read/write mismatch this whole
/// branch is about, wearing the tooltip instead of the form.
///
/// # The mechanism, stated twice
///
/// A placeholder is pinned two independent ways, so neither can be satisfied by accident: it
/// carries the CELL'S OWN tier token (a name that cannot be mistaken for a neighbour's), and it
/// appears in NO form's field list anywhere in the table, for any venue, tier or account (so it
/// cannot be a key any loader is reachable through). Folded over the whole roster, not just
/// [`REPAIRED`] — the property belongs to the table, and the untouched venues' four dead cells
/// (dukascopy `SIM`/`LIVE`, aster `SIM`, hyperliquid `SIM`) hold it for the same reason.
#[test]
fn a_formless_cells_tooltip_names_a_placeholder_no_form_writes() {
    // Every key name any cell's form would write, anywhere in the table.
    let mut written: Vec<String> = Vec::new();
    for &venue in VENUES {
        for &tier in TIERS {
            for account in accounts() {
                written.extend(keys(venue, tier, &account));
            }
        }
    }
    assert!(
        written.len() > 50,
        "only {} written key name(s) collected — the set a placeholder is checked against has \
         gone empty, and this test would pass on anything",
        written.len()
    );

    let mut checked = 0usize;
    for &venue in VENUES {
        for &tier in TIERS {
            for account in accounts() {
                if !account_fields(venue, tier, &account).is_empty() {
                    continue; // a live cell — the tooltip==first-field invariant covers it
                }
                let hovered = account_expected_key_name(venue, tier, &account);
                assert!(
                    hovered.contains(tier),
                    "{venue}/{tier} (account {account}) offers no form yet hovers {hovered}, \
                     which does not carry this cell's own tier — a dead cell must not wear a \
                     neighbouring tier's name"
                );
                assert!(
                    !written.contains(&hovered),
                    "{venue}/{tier} (account {account}) offers no form yet hovers {hovered}, a \
                     key some cell's form really writes — a dot that can never light must not \
                     name a key an operator could be led to set from it"
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 14,
        "only {checked} formless cell(s) were examined — the known dead cells are alpaca SIM, \
         polymarket SIM/DEMO, dukascopy SIM/LIVE, aster SIM and hyperliquid SIM, over two \
         accounts each, so anything less means the fold stopped seeing them"
    );
}
