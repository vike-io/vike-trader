//! The per-venue credential WRITE key tables and the Sensitivity classifier — pure, no egui.

use vike_model::accounts::account_keys::{AccountLabel, account_key, split_account_key};
use vike_model::credential_keys;

/// The expected primary `.env` key name for a venue/env cell's hover tooltip, e.g.
/// `"BINANCE_LIVE_API_KEY"` or (FX venues) `"FXCM_DEMO_USER"`. Mirrors
/// `vike_bridge_core::credentials::names_for_prefix` (private to that crate) for the generic
/// shape, and [`crate::status`]'s per-venue overrides for every venue whose bridge reads a bespoke
/// shape instead — this is display-only, never used to actually read credentials.
///
/// ⚠ **Falling through to the generic `{VENUE}_{TIER}_API_KEY` grid is a CLAIM about the bridge**,
/// the same claim [`crate::status`] documents on the read side: the name a dot names is the name
/// that cell's form writes, so a venue whose loader reads none of those names is a form asking for
/// a key nothing will ever read. alpaca/ctrader/ibkr/polymarket each sat in this fallback while
/// their loaders read an OAuth client-credentials trio, an OAuth app pair plus a per-tier grant,
/// one account number, and one Ethereum private key respectively — which is what put them in the
/// `match` below beside the FX venues, aster and hyperliquid.
///
/// A cell whose venue has no such tier at all names a NEVER-SET placeholder rather than the
/// neighbouring tier's real key — dukascopy's `SIM`/`LIVE`, aster's/hyperliquid's `SIM`, alpaca's
/// `SIM` (whose loader spells demo's own `SANDBOX` token) and polymarket's `SIM`/`DEMO` (that venue
/// has no testnet). Naming a real key beside a dot that can never light would point an operator at
/// a key they must not write from that cell.
pub fn expected_key_name(venue: &str, env_label: &str) -> String {
    match venue {
        "fxcm" => format!("FXCM_{env_label}_USER"),
        "oanda" => format!("OANDA_{env_label}_API_KEY"),
        "ig" => format!("IG_{env_label}_API_KEY"),
        "dukascopy" if env_label == "DEMO" => "DUKASCOPY_DEMO1_LOGIN".to_string(),
        "aster" => {
            // App `Demo` reads the bridge's `TESTNET` tier; `Live` maps 1:1. `Sim` has no real
            // tier for this venue — falls back to a never-set placeholder, same spirit as
            // dukascopy's SIM/LIVE cells above.
            let tier = if env_label == "DEMO" { "TESTNET" } else { env_label };
            format!("ASTER_{tier}_USER")
        }
        // Hyperliquid's bespoke shape is a private key, not an API key/secret pair — see
        // `crate::status::hyperliquid_configured`. No `SIM` tier: that label falls back to a
        // never-set placeholder, same spirit as dukascopy's SIM/LIVE cells above.
        "hyperliquid" => format!("HYPERLIQUID_{env_label}_PRIVATE_KEY"),
        // Alpaca's OAuth2 client-credentials trio; the CLIENT ID is the primary of the three —
        // `vike_alpaca::config::load_alpaca_config_for_account`. App `Demo` reads the bridge's own
        // `SANDBOX` tier token (`vike_alpaca::config::alpaca_tier`) and `Live` maps 1:1; `Sim` is a
        // SECOND SPELLING of demo in that same function, so it takes the never-set `ALPACA_SIM_*`
        // placeholder rather than demo's real key — see `crate::status::alpaca_configured`.
        "alpaca" => {
            let tier = if env_label == "DEMO" { "SANDBOX" } else { env_label };
            format!("ALPACA_{tier}_CLIENT_ID")
        }
        // cTrader's per-tier OAuth grant — the half that distinguishes one tier from another. The
        // `CTRADER_CLIENT_ID`/`_CLIENT_SECRET` app pair `edit_fields` also writes is TIER-LESS, so
        // it could never name a cell. All three tiers are real for this venue — see
        // `crate::status::ctrader_configured` for why `SIM` is a distinct grant rather than a
        // second spelling of demo's, verified against
        // `vike_ctrader::config::CtraderConfig::from_vars_with_store_for_account`.
        "ctrader" => format!("CTRADER_{env_label}_ACCESS_TOKEN"),
        // IBKR's ONLY required name: the account number every order is placed in. The Gateway holds
        // the login, so this venue has no in-crate API key at all —
        // `vike_ibkr::config::load_ibkr_config_for_account`, and `crate::status::ibkr_configured`.
        "ibkr" => format!("IBKR_{env_label}_ACCOUNT"),
        // Polymarket's L1 signing key, under the venue's own `POLY_` prefix — the roster slug and
        // the credential prefix disagree, which is what made the generic `POLYMARKET_*` grid name a
        // key nothing in this tree writes or reads. `LIVE` is the only real cell (no testnet), so
        // `SIM`/`DEMO` name never-set placeholders — `crate::status::polymarket_configured`.
        "polymarket" => format!("POLY_{env_label}_PRIVATE_KEY"),
        _ => format!("{}_{}_API_KEY", venue.to_uppercase(), env_label),
    }
}

/// The editable `.env` fields for one (venue, env) credential cell: a UI label + the exact
/// `.env` key name each field writes. Mirrors each bridge's own config-loader var names — the
/// SAME shapes [`crate::status`]'s per-venue `*_configured` functions read (verified there
/// against each `crates/bridges/<venue>/src/config.rs`), so a value saved here is a value the
/// venue bridge actually picks up. Returns an empty list for (dukascopy, SIM|LIVE) — that venue
/// has no such tier, so no edit affordance is offered for those two cells. Dukascopy's `DEMO`
/// cell is special: it has TWO independent demo accounts (`DEMO1` the default, `DEMO2` the EU
/// demo — see `crate::status::dukascopy_configured`), so this one form offers both field groups
/// rather than picking one; either group alone is enough to save (each row is only written when
/// its own field is non-empty, same as every other venue). Aster similarly has no `SIM` tier
/// (empty list) and its `DEMO` cell writes the bridge's `TESTNET`-prefixed vars, with an
/// optional `SIGNER` field alongside the required `USER`/`PRIVATE_KEY` pair — see
/// `crate::status::aster_configured`.
///
/// # ⚠ An EMPTY list is a statement, and it is the one that keeps this table honest
///
/// `tier_row` renders the ✏ only when this list is non-empty, so an empty list is how a row
/// says *there is no such tier to configure*. It must be returned for exactly the cells
/// [`crate::status`] can never light — otherwise the form is the read-side defect in reverse: an
/// operator fills it in, saves, and no dot appears because no loader reads what was written.
/// Today that is (dukascopy, `SIM`|`LIVE`), (aster, `SIM`), (hyperliquid, `SIM`), (alpaca, `SIM` —
/// `vike_alpaca::config::alpaca_tier` sends it to demo's own `SANDBOX` token, so it is a second
/// spelling rather than a tier) and (polymarket, `SIM`|`DEMO` — that venue has no testnet at all).
///
/// # ⚠ A TIER-LESS key still takes the account label
///
/// cTrader's `CTRADER_CLIENT_ID`/`_CLIENT_SECRET` are an APP-level Spotware registration with no
/// tier token in the name, and they are the first key in this table with that shape. They need no
/// special handling and get none: [`account_fields`] appends the label after the WHOLE of the key,
/// so `ALT`'s app pair is `CTRADER_CLIENT_ID__ALT` — which is exactly what
/// `vike_ctrader::config::CtraderConfig::from_vars_with_store_for_account` looks up, and that
/// loader deliberately offers NO fallback to the unlabelled pair (its own doc argues why). Writing
/// the unlabelled name from a labelled form would therefore configure a DIFFERENT account.
///
/// # ⚠ It is a HAND-WRITTEN table beside a derivable authority, and that is now GATED
///
/// The failure this table can have is not a wrong key — `crates/vike-connections/tests/
/// editor_key_shapes.rs` pins every name it composes against the loader that reads it — it is a
/// key the store really holds that appears in NO arm at all, which an operator then cannot set
/// or change from anywhere in the app. That is invisible from inside this file: nothing about an
/// arm says which of its venue's names are missing from it.
///
/// `crates/vike-app-core/tests/credential_editor_completeness_gate.rs` closes it. That crate is
/// the lowest one that can see BOTH this table and `vike_ops::settings::SETTINGS` (the registry
/// of every store/environment name this workspace reads), so it folds the registry's whole
/// venue-scoped set against this table and fails on a name that is neither reachable here nor
/// carries a written row saying why it must not be. MEASURED before it existed:
/// `DUKASCOPY_DEMO1_SERVER` and `DUKASCOPY_DEMO2_SERVER` were in the owner's real store and in
/// the registry, and reachable from no form. (They left both with decision 0095's Task 7: the
/// server is the `venue.dukascopy.demo.server` setting now, written by `vike-cli config set`.)
///
/// # Why this table is `pub`
///
/// It is the WRITE-side twin of [`crate::credential_status`], and it is gated the same way that
/// one is: from `crates/vike-connections/tests/`, where the settings registry's `src/` literal
/// sweep does not look (`crates/vike-connections/tests/key_shapes/write_shapes.rs` carries the argument,
/// and `crate::status`' module doc carries the gate it is avoiding). A `#[cfg(test)]` block here
/// could not spell a composed key name like `ALPACA_SANDBOX_CLIENT_ID` without demanding a
/// `vike_ops::settings::SETTINGS` row asserting a read this crate does not perform.
pub fn edit_fields(venue: &str, env_label: &str) -> Vec<(&'static str, String)> {
    let mut fields = credential_fields(venue, env_label);
    // ⚠ Appended ONLY to a cell that already has a form. An empty list is how a row says *there is
    // no such tier* ([`tier_row`] renders the ✏ from it), so growing one here would resurrect a
    // form for aster's `SIM` or polymarket's `DEMO` — cells `crate::status` can never light.
    if !fields.is_empty() {
        fields.extend(attribution_fields(venue, env_label));
    }
    fields
}

/// **The VENUE-WIDE attribution tail**, appended by [`edit_fields`] to the LIVE cell of every venue
/// with an order-level mechanic — the affiliate/builder tag `vike_bridge_core::credentials::
/// attribution_code_from` stamps on outgoing orders, plus the one fee knob two of those venues
/// carry.
///
/// ⚠ **Three properties, and each of them is a decision rather than a detail.**
///
/// * **LIVE only.** These keys carry NO tier token — one name serves all three cells — so a field
///   on each would be three controls over one value, and an operator editing `demo` would be
///   changing what a real order is tagged with. It is rendered where real orders are configured,
///   and every label says `venue-wide` so the scope is on screen rather than inferred.
/// * **NOT account-scoped**, and [`account_fields`] is where that is honoured. `attribution_code_from`
///   reads the BARE `{VENUE}_BROKER_CODE` with no label grammar at all — it takes no
///   `AccountLabel` — so composing `BINANCE_BROKER_CODE__ALT` from a labelled cell would write a
///   name nothing in this tree reads, which is precisely the defect
///   `crates/vike-connections/tests/key_shapes/write_shapes.rs` exists to keep out of this table.
/// * **The SUFFIX is `vike_model::credential_keys::attribution_var_for`'s answer, never a
///   per-venue list here.** A `SignedBuilder` venue takes `_BUILDER_CODE` and the CEX mechanics
///   take `_BROKER_CODE` — the split `crates/vike-bridge-core/CLAUDE.md`'s attribution-codes
///   bullet names venue by venue, derived from the mechanic so a new mechanised venue is
///   classified by adding no row.
///   (`attribution_code_from` accepts either spelling, so the choice is about which name an
///   operator is taught, not about what is read.) ⚠ It lives in `vike-model` rather than here for
///   a second reason that is not taste: `crates/vike-ops/tests/settings/settings_registry/walk_and_grid.rs`'s
///   `generated_key_sites` reads a call to `attribution_key` as *this crate reads the whole grid*
///   and would demand several hundred `SETTINGS` rows of `vike-connections`. That function's own
///   doc carries the argument.
///
/// The two fee knobs are spelled out because they are spelled out in the tree: they are not one
/// grammar with two venues in it, they are two different names
/// (`HYPERLIQUID_BUILDER_FEE_TENTHS_BP` is tenths of a basis point, `ASTER_BUILDER_FEE_RATE` is a
/// rate string), each read by `vike-mount` at its own call site.
fn attribution_fields(venue: &str, env_label: &str) -> Vec<(&'static str, String)> {
    if env_label != "LIVE" {
        return Vec::new();
    }
    let Some(code) = credential_keys::attribution_var_for(venue) else {
        return Vec::new();
    };
    let label = if code.ends_with(credential_keys::BUILDER_CODE_SUFFIX) {
        "Builder Code (venue-wide, optional)"
    } else {
        "Broker Code (venue-wide, optional)"
    };
    let mut out = vec![(label, code)];
    match venue {
        "hyperliquid" => out.push((
            "Builder Fee, tenths of a bp (venue-wide, optional)",
            "HYPERLIQUID_BUILDER_FEE_TENTHS_BP".to_string(),
        )),
        "aster" => out.push((
            "Builder Fee Rate (venue-wide, optional)",
            "ASTER_BUILDER_FEE_RATE".to_string(),
        )),
        _ => {}
    }
    out
}

/// Is `key` a name that belongs to the VENUE rather than to one of its accounts?
///
/// The attribution family and nothing else — see [`attribution_fields`] for why, and
/// [`account_fields`] for what this changes. Matched on the SUFFIX so a labelled composition can
/// never be produced for one of these in the first place.
#[must_use]
pub fn is_venue_wide_key(key: &str) -> bool {
    key.ends_with(credential_keys::BROKER_CODE_SUFFIX)
        || key.ends_with(credential_keys::BUILDER_CODE_SUFFIX)
        || key.ends_with("_BUILDER_FEE_RATE")
        || key.ends_with("_BUILDER_FEE_TENTHS_BP")
}

/// **What knowing a field's value would let somebody DO** — the one axis that decides whether the
/// form masks it and whether the panel is allowed to show what the store already holds.
///
/// ⚠ This is not a taste question and it is not per-venue. THE RULE: a value is [`Self::Secret`]
/// when possessing it is enough to ACT AS the account — a key, a secret, a passphrase, a password,
/// a private key, an OAuth token. Everything else NAMES something — an account, a server, an
/// attribution tag — and is the half an operator has to be able to read back to answer *"which
/// account is this, and where is it pointing"*. A masked field that starts empty is exactly right
/// for the first kind and exactly wrong for the second: it is how a server name an operator put in
/// their store six months ago becomes unknowable from inside the app that wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    /// Masked, never prefilled, never read back into the UI under any circumstances.
    Secret,
    /// Rendered as ordinary text and PREFILLED from the store, so the operator can see and correct
    /// what is there.
    Public,
}

/// The suffixes that make a key a [`Sensitivity::Secret`], longest-first where two overlap.
///
/// ⚠ `_RELAYER_API_KEY` needs no entry: it ends with `_API_KEY`. Its ADDRESS sibling
/// (`_RELAYER_API_KEY_ADDRESS`) ends with `_ADDRESS` and is therefore public, which is the case
/// that makes SUFFIX matching — rather than `contains` — load-bearing here.
const SECRET_SUFFIXES: [&str; 9] = [
    "_API_SECRET",
    "_API_PASSPHRASE",
    "_API_KEY",
    "_PRIVATE_KEY",
    "_PASSWORD",
    "_PASSPHRASE",
    "_SECRET",
    "_ACCESS_TOKEN",
    "_REFRESH_TOKEN",
];

/// The suffixes that make a key a [`Sensitivity::Public`], each with the reason it is one.
///
/// * `_USER` / `_LOGIN` / `_IDENTIFIER` / `_ACCOUNT` / `_ACCOUNT_ID` / `_CLIENT_ID` — the half of a
///   credential pair that NAMES the account. Possessing it signs nothing, and it is the first
///   thing an operator checks when a mount reaches the wrong book.
/// * `_SERVER` / `_ADDRESS` / `_ACCOUNT_ADDRESS` — an endpoint or an on-chain address. Public by
///   construction: an Ethereum address is derived from the key, never the other way round.
/// * `_SIGNER` — aster's optional signer ADDRESS, "derived from the key"
///   (`crates/bridges/aster/src/signing.rs`), so it is the address and not the key.
/// * `_BROKER_CODE` / `_BUILDER_CODE` / the two builder-fee knobs — an affiliate tag and a fee
///   rate. They are stamped on outgoing orders in the clear, so they are not secret from anybody
///   who can see a fill.
const PUBLIC_SUFFIXES: [&str; 10] = [
    "_ACCOUNT_ADDRESS",
    "_ADDRESS",
    "_SERVER",
    "_ACCOUNT_ID",
    "_ACCOUNT",
    "_CLIENT_ID",
    "_IDENTIFIER",
    "_USER",
    "_LOGIN",
    "_SIGNER",
];

/// [`Sensitivity`] for one store key, account label and all.
///
/// ⚠ **Unclassified FAILS CLOSED**, and that is the property the whole thing rests on: a key whose
/// suffix appears in neither table is a [`Sensitivity::Secret`], so a name added to [`edit_fields`]
/// tomorrow is masked and unread until somebody deliberately classifies it. The opposite default
/// would make "forgot to think about it" mean "rendered on screen".
///
/// ⚠ The ACCOUNT LABEL is stripped first (`vike_model::accounts::account_keys::split_account_key`), because
/// `DUKASCOPY_DEMO1_SERVER__ALT` ends with the label and not with `_SERVER` — a labelled box's
/// public field would otherwise fail closed and mask itself.
#[must_use]
pub fn key_sensitivity(key: &str) -> Sensitivity {
    let base = split_account_key(key).map_or(key, |s| s.base);
    if SECRET_SUFFIXES.iter().any(|s| base.ends_with(s)) {
        return Sensitivity::Secret;
    }
    // The attribution family: the two `_CODE` spellings and the two builder-fee knobs, which end
    // in neither a code nor a name and so are reached through the venue-wide predicate instead.
    if PUBLIC_SUFFIXES.iter().any(|s| base.ends_with(s)) || is_venue_wide_key(base) {
        return Sensitivity::Public;
    }
    Sensitivity::Secret
}

/// The per-tier CREDENTIAL half of [`edit_fields`] — the hand-written per-venue table itself, with
/// the venue-wide tail factored out so an arm here is only ever about one (venue, tier) cell.
pub(super) fn credential_fields(venue: &str, env_label: &str) -> Vec<(&'static str, String)> {
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
        // TWO fields per account — the login pair `vike_dukascopy::config::dukascopy_env_var_names`
        // composes. ⚠ This arm carried a third, `_SERVER` (`JNLP URL`), until decision 0095's
        // Task 7: the JForex server is the demo tier's `venue.dukascopy.demo.server` SETTING now,
        // read by nothing in the credential store, and a row written there refuses the trading
        // daemon's start. [`form_note`] says where the server is set instead.
        "dukascopy" if env_label == "DEMO" => vec![
            ("Login (DEMO1)", "DUKASCOPY_DEMO1_LOGIN".to_string()),
            ("Password (DEMO1)", "DUKASCOPY_DEMO1_PASSWORD".to_string()),
            ("Login (DEMO2)", "DUKASCOPY_DEMO2_LOGIN".to_string()),
            ("Password (DEMO2)", "DUKASCOPY_DEMO2_PASSWORD".to_string()),
        ],
        "dukascopy" => Vec::new(), // no SIM/LIVE tier
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
            _ => Vec::new(), // no SIM tier
        },
        // Hyperliquid: a required signer private key + an optional master account address
        // (agent-wallet mode) — the exact vars `vike_hyperliquid::config::load` reads; see
        // `crate::status::hyperliquid_configured`. No SIM tier.
        "hyperliquid" => match env_label {
            "DEMO" | "LIVE" => vec![
                ("Private Key", format!("HYPERLIQUID_{env_label}_PRIVATE_KEY")),
                ("Account Address (optional)", format!("HYPERLIQUID_{env_label}_ACCOUNT_ADDRESS")),
            ],
            _ => Vec::new(), // no SIM tier
        },
        // Alpaca: the OAuth2 client-credentials pair plus the PINNED account, all three REQUIRED —
        // `vike_alpaca::config::load_alpaca_config_for_account` returns `None` unless every one is
        // present. The tier token is the bridge's own (`SANDBOX` for the app's Demo column, `LIVE`
        // for Live); `Sim` is that same token under a second spelling, so it offers no form.
        "alpaca" => match env_label {
            "DEMO" | "LIVE" => {
                let tier = if env_label == "DEMO" { "SANDBOX" } else { "LIVE" };
                vec![
                    ("Client ID", format!("ALPACA_{tier}_CLIENT_ID")),
                    ("Client Secret", format!("ALPACA_{tier}_CLIENT_SECRET")),
                    ("Account ID", format!("ALPACA_{tier}_ACCOUNT_ID")),
                ]
            }
            _ => Vec::new(), // `Sim` is a second spelling of `SANDBOX`, not a tier
        },
        // cTrader: THIS tier's OAuth grant plus the TIER-LESS Spotware app registration. All four
        // are required by `vike_ctrader::config::CtraderConfig::from_vars_with_store_for_account`,
        // and all three tiers are real. `CTRADER_{TIER}_ACCOUNT_ID` is deliberately absent: that
        // loader treats it as optional and discovers it at connect, so a form offering it would
        // invite an operator to pin an account id the venue would otherwise hand them.
        //
        // ⚠ The GRANT is first and the app pair second, which is the reverse of the order the
        // loader reads them in. the rail dot hovers `expected_key_name`, and
        // `a_cells_tooltip_names_the_same_account_key_its_form_writes` holds that name equal to
        // this list's FIRST field — a tier-less key at the top would make all three of this
        // venue's cells hover one name, which is the tooltip failing to say which cell it is on.
        // ⚠ And the app pair carries no tier at all — see this function's doc for what that means
        // for a labelled account, which is the one thing about it that is not obvious.
        "ctrader" => vec![
            ("Access Token", format!("CTRADER_{env_label}_ACCESS_TOKEN")),
            ("Refresh Token", format!("CTRADER_{env_label}_REFRESH_TOKEN")),
            ("Client ID (app)", "CTRADER_CLIENT_ID".to_string()),
            ("Client Secret (app)", "CTRADER_CLIENT_SECRET".to_string()),
        ],
        // IBKR: the account number, and nothing else.
        // `vike_ibkr::config::load_ibkr_config_for_account` `?`s on that name alone — every other
        // credential it reads (`_CLIENT_ID`/`_DATA_CLIENT_ID`/`_MKTDATA_TYPE`) has a default, and
        // the gateway is the tier's `venue.ibkr.<tier>.*` setting since decision 0095's Task 7;
        // offering one here would let a present-but-unparseable
        // value turn a working mount into a paper one. The Gateway holds the login, so there is no
        // API key or secret to write. All three tiers are real: that loader spells
        // `Environment::as_str` verbatim.
        "ibkr" => vec![("Account", format!("IBKR_{env_label}_ACCOUNT"))],
        // Polymarket: the L1 Ethereum key that signs orders and derives the L2 trio at connect, so
        // it is the only REQUIRED name —
        // `vike_polymarket::config::load_polymarket_creds_for_account` gates on it alone. The
        // funder/deposit-wallet address is optional there and is offered the way hyperliquid's
        // master-account address is, for the same reason (a key that signs FOR an account the
        // operator may need to name). ⚠ The prefix is `POLY_`, not `POLYMARKET_`, and `LIVE` is the
        // only cell: every key here signs real money on Polygon mainnet and this venue has no
        // testnet, so a SIM/DEMO form would promise a sandbox that does not exist.
        // ⚠ SEVEN fields, not two, and the five that arrived late are the rest of what
        // `load_poly_tier` actually reads. The L2 trio (`_API_KEY`/`_SECRET`/`_PASSPHRASE`) is
        // derived at connect via EIP-712 when blank — which is why it is optional, NOT why it was
        // missing; an operator holding a CLOB key pair issued by Polymarket's own UI could not
        // enter it here at all. The relayer pair is the EIP-1271 proxy-wallet submit/cancel path
        // (`vike_polymarket::exec_plane::client`'s `submit_order_relayer`/`cancel_order_relayer`), likewise
        // read by that loader and likewise unreachable. Every one is written at the loader's FIRST
        // rung (`POLY_{TIER}_…`); the tier-less `POLY_…` spelling is a read-compatibility fallback
        // this form must never author — see the doc above.
        "polymarket" => match env_label {
            "LIVE" => vec![
                ("Private Key", format!("POLY_{env_label}_PRIVATE_KEY")),
                ("Funder Address (optional)", format!("POLY_{env_label}_ADDRESS")),
                ("API Key (L2, optional)", format!("POLY_{env_label}_API_KEY")),
                ("API Secret (L2, optional)", format!("POLY_{env_label}_SECRET")),
                ("API Passphrase (L2, optional)", format!("POLY_{env_label}_PASSPHRASE")),
                ("Relayer API Key (optional)", format!("POLY_{env_label}_RELAYER_API_KEY")),
                ("Relayer Address (optional)", format!("POLY_{env_label}_RELAYER_API_KEY_ADDRESS")),
            ],
            _ => Vec::new(), // no testnet: this venue has no SIM or DEMO tier
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

/// [`edit_fields`] for ONE account — the same labels and the same field ORDER, with every key name
/// composed through `vike_model::accounts::account_keys::account_key`.
///
/// ⚠ **That composition is the whole of the account dimension on the write side**, and it is why
/// no per-venue table here needed a second column: the grammar appends the label after the WHOLE
/// of today's key, so a bespoke suffix (`OANDA_{TIER}_ACCOUNT_ID`, the FX `_USER`/`_PASSWORD`
/// pair, dukascopy's numbered login) is carried along without being enumerated, and
/// [`AccountLabel::Default`] returns each key UNCHANGED — a single-account box writes the same
/// `String`s it always wrote, by construction rather than by care.
pub fn account_fields(
    venue: &str,
    env_label: &str,
    account: &AccountLabel,
) -> Vec<(&'static str, String)> {
    edit_fields(venue, env_label)
        .into_iter()
        .map(|(label, key)| {
            // ⚠ The ONE exception to "every key name is composed through `account_key`", and it is
            // an exception because the READER has no account dimension: `attribution_code_from`
            // looks up the bare `{VENUE}_BROKER_CODE` and takes no `AccountLabel` at all. Applying
            // the grammar here would make a labelled cell write `BINANCE_BROKER_CODE__ALT` — a
            // name nothing in this tree reads, which is the whole defect class this table was
            // repaired for. See [`attribution_fields`].
            if is_venue_wide_key(&key) {
                return (label, key);
            }
            (label, account_key(&key, account))
        })
        .collect()
}

/// [`expected_key_name`] for ONE account — the hover tooltip's twin of [`account_fields`], so the
/// name a dot names is the name that cell's form would write.
pub fn account_expected_key_name(venue: &str, env_label: &str, account: &AccountLabel) -> String {
    account_key(&expected_key_name(venue, env_label), account)
}

/// A per-cell qualification rendered under [`crate::view::MASKED_FIELD_HINT`], or `None` for the venues that
/// need none.
///
/// ⚠ **Dukascopy's DEMO cell names a value this form does NOT write.** Its JForex server was a
/// third field here (`DUKASCOPY_DEMO{1,2}_SERVER`) until decision 0095's Task 7 made it the demo
/// tier's `venue.dukascopy.demo.server` SETTING — one per machine and tier (ruling 10), read from
/// the settings database by the mount and by nothing in the credential store, where a row under the
/// old name now refuses the trading daemon's start. So the cell says where the value lives instead
/// of offering a control that would write a row nothing reads.
///
/// ⚠ The `.jnlp` condition is carried over unchanged: `vike_dukascopy::exec`'s
/// `DukascopyExec::spawn_with_program` keeps the stored value ONLY when it ends in `.jnlp`, and
/// otherwise warns and hands the sidecar its own `DEFAULT_DEMO_JNLP`. Nothing exercises a stored
/// server end to end — `crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s `live_config`
/// force-blanks `cfg.server` before every login.
#[must_use]
pub fn form_note(venue: &str, env_label: &str) -> Option<&'static str> {
    match (venue, env_label) {
        ("dukascopy", "DEMO") => Some(
            "JNLP server: a venue setting — `vike-cli config set venue.dukascopy.demo.server \
             <url>`. Only a value ending in `.jnlp` is used; anything else is ignored and the \
             built-in demo JNLP is used instead. No live smoke exercises a stored JNLP URL.",
        ),
        _ => None,
    }
}

#[path = "keys_tests.rs"]
#[cfg(test)]
mod keys_tests;
