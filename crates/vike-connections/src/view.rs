//! egui render of the Connections tool's **Credentials** tab — a venue RAIL, a per-venue DETAIL
//! pane, and in-app masked key editing.
//!
//! Connect/disconnect controls are NOT here: the live backend connection is process-level state
//! rendered by the tool's own ambient strip (`vike_app_core::tool_views::connections`), identical
//! on both tabs, so that one fact has one rendering.
//!
//! # ⚠ The grid is GONE, and what replaced each of its columns
//!
//! This panel used to be one 5-column `egui::Grid` — `Venue | Status | Sim | Demo | Live` — with a
//! ●/○ dot per credential cell and a hover naming the key. Three things were wrong with it and
//! each is now a distinct piece of this file:
//!
//! * **The `Status` column and the credential dots sat in one row of one table**, which is exactly
//!   the two-facts-one-glyph confusion [`crate::summary`]'s module doc argues against. Status is
//!   now a LABELLED cell in the detail pane, reading [`crate::summary::FeedFact`] — which, unlike
//!   the old `live.get(venue).copied().unwrap_or_default()`, does not render "no producer in this
//!   build" as the `Unknown` a silent producer gives.
//! * **A tier a venue does not HAVE rendered as the same hollow ring as an unset one.** The only
//!   difference was a missing ✎ and a tooltip naming a placeholder key. [`tier_row`] says
//!   `not configurable` in words and names the venue.
//! * **A hover tooltip could name ONE key**, and most venues' forms write two, three or four of
//!   them. The detail pane spells every key out, `(optional)` included, without opening the form.
//!
//! **STATUS SOURCES**: the detail pane's `Status` row reads whatever live sources the binary
//! threads in via the `live` map — today the venues with a live feed producer, each parsed via
//! [`crate::parse_feed_status`]. A venue with no producer is absent from the map and is reported
//! as having none. Extending coverage is purely a matter of the binary adding that venue's status
//! handle to the map it passes here. Key editing IS in scope: each configurable tier row gets a
//! small edit affordance that opens a masked (password) form and writes through
//! [`crate::env_write::save_credentials_journalled`].
//!
//! SECURITY (non-negotiable, see the crate's callers/tests too):
//! - An existing secret's plaintext is NEVER read back into the UI — edit fields always start
//!   empty; the status dot is the only feedback on "is something configured", never the value.
//! - Edit fields are `egui::TextEdit::password(true)` (masked).
//! - Only non-empty fields are written; an empty field means "leave unchanged".
//! - No secret value is ever passed to `tracing`/`println!`/`eprintln!`/`dbg!` — only the
//!   venue/env tier ("saved credentials for binance/live").
//!
//! # The ACCOUNT dimension
//!
//! A venue may hold more than one account (`vike_model::account_keys`), and this grid is the
//! credential EDITOR — so until it could reach a labelled account, an operator could SEE one on the
//! Data Manager's Venues tab and create or edit one nowhere at all, leaving hand-editing
//! `<project>/settings/secrets.env` as the only way to add a second.
//!
//! It arrives as a SELECTOR above the grid rather than as a third axis inside it; `account_strip`
//! carries the whole argument — why not more rows, why not more columns, how an account is created,
//! and why there is no removal affordance. The two properties to carry away from here:
//!
//! * **Every key name is composed through `vike_model::account_keys::account_key`**
//!   (`account_fields` / `account_expected_key_name`), which returns the DEFAULT account's names
//!   unchanged. A box with no labelled account therefore reads, renders and writes exactly what it
//!   did — `crates/vike-connections/tests/account_editor.rs`'s
//!   `a_store_with_no_labelled_account_renders_the_panel_that_shipped` pins the panel and
//!   `crates/vike-connections/tests/credential_write_journal.rs` still gates the write end to end,
//!   unchanged.
//! * **No per-venue table grew a column.** The grammar appends the label after the WHOLE of today's
//!   key, so every bespoke suffix below is carried along without being enumerated.

use std::collections::HashMap;

use vike_model::account_keys::{
    ACCOUNT_SEPARATOR, AccountLabel, MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL, account_key,
};
use vike_model::change_journal::Actor;

use crate::ConnectionState;
use crate::env_write::{CredentialWrite, save_credentials_journalled};
use crate::status::{AccountGrids, VenueCredStatus};
use crate::summary::{FeedFact, StoreHealth, TIERS, TierState, tier_state};

// ⚠ These four are LOCAL NAMES for `vike_ui_theme::palette::status`, not values. Three of them used
// to be private literals — `(224,176,62)`, `(224,96,96)` and `from_gray(110)` — a few points off the
// bottom status strip's amber, red and grey for the SAME `ConnectionState`, which an operator could
// see: one feed, two panels, two ambers. The shared module's doc carries the table of what differed
// and why the strip's set won; these three colours therefore MOVED when it landed. The names stay
// local because each also paints something that is not a connection state at all (a credential dot,
// an inline error, a provisional account chip), which is exactly why the shared constants are colour
// roles rather than state names.
// Only CONFIGURED_COLOR is unchanged: `status::CONNECTED` is an ALIAS of the
// `vike_ui_theme::palette::ACCENT` this line already read. The other three repaint — and the red
// repaints TO `vike_ui_theme::palette::DOWN`, which `status::ERROR` also aliases, so this tool
// stops carrying a private near-copy of the canonical down-red.
const CONFIGURED_COLOR: egui::Color32 = vike_ui_theme::palette::status::CONNECTED;
const ERROR_COLOR: egui::Color32 = vike_ui_theme::palette::status::ERROR;
const ABSENT_COLOR: egui::Color32 = vike_ui_theme::palette::status::MUTED;
const CONNECTING_COLOR: egui::Color32 = vike_ui_theme::palette::status::CONNECTING;

// ⚠ TOMBSTONE — `connection_state_label_color` lived here. It painted the old grid's Status
// COLUMN, one cell per venue row, from `live.get(venue).copied().unwrap_or_default()`. Both halves
// of that went with the column: the colour choice now sits inline in `venue_detail` (one cell per
// SELECTED venue, not one per row), and the `unwrap_or_default()` — which rendered "nothing in
// this build produces a status for this venue" as the same `Unknown` a silent producer gives — is
// replaced by `crate::summary::FeedFact`, which keeps the two answers apart. `vike-ui-theme`'s
// `palette::status` doc still cites this name as the Connections tool's consumer of those four
// colours; it remains true of this file, through `venue_detail` and the three dot renderers.

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
/// [`tier_row`] renders the ✎ only when this list is non-empty, so an empty list is how a row
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
/// # Why this table is `pub`
///
/// It is the WRITE-side twin of [`crate::credential_status`], and it is gated the same way that
/// one is: from `crates/vike-connections/tests/`, where the settings registry's `src/` literal
/// sweep does not look (`crates/vike-connections/tests/editor_key_shapes.rs` carries the argument,
/// and `crate::status`' module doc carries the gate it is avoiding). A `#[cfg(test)]` block here
/// could not spell a composed key name like `ALPACA_SANDBOX_CLIENT_ID` without demanding a
/// `vike_ops::settings::SETTINGS` row asserting a read this crate does not perform.
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
        // name it reads (`_HOST`/`_PORT`/`_CLIENT_ID`/`_DATA_CLIENT_ID`/`_BACKEND`/`_CPAPI_URL`/
        // `_MKTDATA_TYPE`) has a default, and offering one here would let a present-but-unparseable
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
        "polymarket" => match env_label {
            "LIVE" => vec![
                ("Private Key", format!("POLY_{env_label}_PRIVATE_KEY")),
                ("Funder Address (optional)", format!("POLY_{env_label}_ADDRESS")),
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
/// composed through `vike_model::account_keys::account_key`.
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
        .map(|(label, key)| (label, account_key(&key, account)))
        .collect()
}

/// [`expected_key_name`] for ONE account — the hover tooltip's twin of [`account_fields`], so the
/// name a dot names is the name that cell's form would write.
pub fn account_expected_key_name(venue: &str, env_label: &str, account: &AccountLabel) -> String {
    account_key(&expected_key_name(venue, env_label), account)
}

/// Per-widget edit state, kept in egui's temp memory (keyed off the widget's own `Id`) rather
/// than threaded through the call site — see the module doc + the CLAUDE.md build note: "prefer
/// keeping edit state inside the connections widget where possible." Edit-field buffers ALWAYS
/// start empty (never pre-filled from an existing secret — rule 2).
#[derive(Clone, Default)]
struct EditState {
    /// WHICH ACCOUNT the grid is showing and the form would write.
    ///
    /// On a box with no labelled account, [`AccountLabel::Default`] is the only account the strip
    /// offers a CHIP for — the Add-account form is the way out of it, and what that selects is a
    /// label holding nothing until a credential is saved into it. So while this is `Default`, and
    /// it is until an operator deliberately leaves, every key name composed below is the key name
    /// that shipped: `vike_model::account_keys::account_key` returns each one unchanged.
    account: AccountLabel,
    /// WHICH VENUE the rail has selected and the detail pane is showing.
    ///
    /// Empty until the first frame resolves it — [`connections_ui`] pins it to the grid's FIRST
    /// row whenever this names a venue the grid being rendered does not contain, which covers
    /// both the empty default and a roster that shrank under a persisted selection. Storing the
    /// venue by NAME rather than by index is what makes that repair possible: an index would
    /// silently point at a different venue after any reorder.
    venue: String,
    /// The **Add account** field's buffer while that form is open; `None` when it is closed. A
    /// plain name, not a credential: rendered unmasked, deliberately, because an operator has to
    /// be able to read the label they are about to have to type into their policy file.
    add_label: Option<String>,
    /// The previous **Create** click's refusal, rendered under the field —
    /// `vike_model::account_keys::AccountKeyError`'s own `Display`, so the message names the rule
    /// that was broken rather than restating one here that could drift from the validator.
    label_error: Option<String>,
    /// (venue, env_label) currently open in the form, if any.
    target: Option<(String, String)>,
    /// One buffer per field of `edit_fields(venue, env_label)`, same order.
    buffers: Vec<String>,
    /// Non-secret status line shown after Save/Cancel — venue/env tier only, never a value.
    message: Option<(bool, String)>, // (is_error, text)
}

impl EditState {
    fn open(&mut self, venue: &str, env_label: &str) {
        let n = edit_fields(venue, env_label).len();
        self.buffers = vec![String::new(); n];
        self.target = Some((venue.to_string(), env_label.to_string()));
        self.message = None;
    }

    fn close(&mut self) {
        self.target = None;
        self.buffers.clear();
    }

    /// Switch the grid to another account.
    ///
    /// ⚠ **It CLOSES any open credential form, and that is a correctness requirement rather than
    /// tidiness.** The form's buffers are typed against the account that was selected when it
    /// opened; leaving it up across a switch would let a Save compose those characters into the
    /// NEW account's key names — a live key written into the wrong account, with the form giving
    /// no sign that anything moved. `crates/vike-connections/tests/account_editor.rs`'s
    /// `switching_account_closes_an_open_credential_form` is the gate.
    fn select_account(&mut self, account: AccountLabel) {
        if self.account == account {
            return;
        }
        self.account = account;
        self.close();
        self.message = None;
    }

    /// Switch the rail to another venue.
    ///
    /// ⚠ **It CLOSES any open credential form, for the same reason [`Self::select_account`]
    /// does.** The buffers were typed against the venue that was selected when the form opened,
    /// and [`account_fields`] composes them into whatever venue is selected at SAVE time — so a
    /// switch with a form left open is a live key written under another venue's key names.
    /// This file's own `switching_venue_closes_an_open_form` is the gate, beside
    /// `crates/vike-connections/tests/account_editor.rs`'s
    /// `switching_account_closes_an_open_credential_form` — the same rule on the other axis.
    fn select_venue(&mut self, venue: &str) {
        if self.venue == venue {
            return;
        }
        self.venue = venue.to_string();
        self.close();
        self.message = None;
    }

    /// Accept an operator-typed account label, or refuse it with the validator's own reason.
    ///
    /// ⚠ The ONLY repair applied is a `trim`, and the line between that and the repairs
    /// `AccountLabel::parse` deliberately refuses (it will not uppercase `alt`) is that whitespace
    /// is not part of ANY label's spelling — nobody is learning a wrong name from a stripped
    /// trailing space off a paste, whereas a silently uppercased label is a spelling that then
    /// does not match what they wrote in their policy file.
    fn create_account(&mut self, text: &str) {
        match AccountLabel::parse(text.trim()) {
            Ok(label) => {
                self.select_account(label);
                self.add_label = None;
                self.label_error = None;
            }
            Err(e) => self.label_error = Some(e.to_string()),
        }
    }
}

/// The width below which the rail/detail pair collapses to ONE column and the rail becomes a
/// wrapped chip row. A tool window opens at 560pt wide, so the one-column shape is the ordinary
/// one and the two-column shape is what a widened window buys; both must work, and the narrow
/// shape must still work at ~400pt.
const RAIL_DETAIL_MIN_W: f32 = 820.0;

/// The rail's fixed column width in the two-column shape — a venue name plus three dots under an
/// `S  D  L` header, and nothing else, so the detail pane keeps everything a narrow window has.
const RAIL_W: f32 = 168.0;

/// ⚠ The dimming applied to the marks that are NOT a measurement — the no-such-tier mark and the
/// unreadable-store one. Applied to [`ABSENT_COLOR`] rather than being further literal colours, so
/// they cannot drift from the muted grey the rest of this file and the bottom status strip share.
const NOT_CONFIGURABLE_DIM: f32 = 0.45;

/// ⚠ **The rail's four glyphs, and why the last two are not dots.**
///
/// The filled `●` and the hollow `○` are the two MEASUREMENTS — every key set, and none set.
/// The other two states are not measurements at all and must not be spelled with the same shape:
///
/// * [`TierState::NotConfigurable`] used to render a DIM FILLED `●`, which is a filled dot at the
///   glyph level however low its opacity. That mattered beyond aesthetics: `scripts/qa_shots.sh`'s
///   `05-connections` pose tells a human judge that on the credential-free QA root *a single
///   FILLED dot means the isolation broke — report it immediately*, and a dim `●` is what dukascopy
///   SIM/LIVE, aster SIM, hyperliquid SIM, alpaca SIM and polymarket SIM/DEMO all rendered there.
///   Every clean capture false-positived that instruction, and a tree-reading test could not tell
///   the two apart either. A middle dot is the "dim, low-opacity mark" the design asks for and is
///   the same shape no measurement uses.
/// * [`TierState::Unknown`] is a QUESTION MARK, because the store did not open and nothing about
///   this cell was measured. It is the one glyph here that is not grey.
const GLYPH_CONFIGURED: &str = "\u{25CF}";
const GLYPH_NOT_SET: &str = "\u{25CB}";
const GLYPH_NOT_CONFIGURABLE: &str = "\u{00B7}";
const GLYPH_UNKNOWN: &str = "?";

/// One rail dot for one (venue, tier) cell — CREDENTIAL PRESENCE, never a feed state. The four
/// glyphs are the four [`TierState`]s and the hover text names the exact key this cell's form
/// would write, for the account being shown.
fn tier_dot(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    tier: &str,
    account: &AccountLabel,
    health: &StoreHealth,
) {
    let state = tier_state(row, tier, health);
    let (glyph, color) = match state {
        TierState::Configured => (GLYPH_CONFIGURED, CONFIGURED_COLOR),
        TierState::NotSet => (GLYPH_NOT_SET, ABSENT_COLOR),
        TierState::NotConfigurable => {
            (GLYPH_NOT_CONFIGURABLE, ABSENT_COLOR.gamma_multiply(NOT_CONFIGURABLE_DIM))
        }
        TierState::Unknown => (GLYPH_UNKNOWN, CONNECTING_COLOR),
    };
    let hover = match state {
        TierState::NotConfigurable => {
            format!("{} has no {} tier", row.venue, tier.to_lowercase())
        }
        // ⚠ NOT the key name: naming a key beside a mark that measured nothing invites the reading
        // that the key was looked for and found missing. It was not looked for at all.
        TierState::Unknown => {
            "the credential store could not be opened — nothing about this cell was measured"
                .to_string()
        }
        // ⚠ The KEY NAME, never its value: `account_expected_key_name` composes a name out of the
        // venue, the tier and the account label and has no access to the store at all.
        _ => account_expected_key_name(&row.venue, tier, account),
    };
    // ⚠ A FIXED cell width, matching the `S  D  L` header's, so the three dots sit in columns
    // rather than drifting with each glyph's own advance — a rail whose dots do not line up is a
    // rail you cannot read down.
    ui.add_sized(
        [DOT_W, 16.0],
        egui::Label::new(egui::RichText::new(glyph).monospace().size(13.0).color(color))
            .selectable(false),
    )
    .on_hover_text(hover);
}

/// The width of one rail dot cell, and of one `S`/`D`/`L` header letter — ONE constant, because
/// the header and the dots are two separate layout passes and a second value is how they stop
/// lining up.
const DOT_W: f32 = 11.0;

/// One rail ROW: the venue name plus its three dots in `S D L` order.
///
/// ⚠ **The SELECTED row is a `Label`, not a `Button`** — the same idiom (and the same argument)
/// as [`account_chip`]: "which venue am I looking at" stays answerable from the accessibility
/// tree by ROLE alone, and the current selection cannot be re-picked into a no-op frame. That is
/// this widget's honest spelling of the mockup's `aria-current`.
fn rail_row(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    selected: bool,
    account: &AccountLabel,
    health: &StoreHealth,
    pick: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // ⚠ A LEFT-ALIGNED fixed cell, not `add_sized` — that helper lays its widget out
        // centred-and-justified, which would centre each venue name in its column and leave a
        // rail of ragged first letters nobody can scan down.
        ui.allocate_ui_with_layout(
            egui::vec2(NAME_W, 16.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(NAME_W);
                let name = egui::RichText::new(&row.venue).monospace().size(12.0);
                if selected {
                    ui.add(
                        egui::Label::new(name.strong().color(CONFIGURED_COLOR)).selectable(false),
                    );
                } else if ui
                    .add(egui::Button::new(name).frame(false))
                    .on_hover_text("show this venue's credential tiers")
                    .clicked()
                {
                    *pick = Some(row.venue.clone());
                }
            },
        );
        for tier in TIERS {
            tier_dot(ui, row, tier, account, health);
        }
    });
}

/// The rail's venue-name column width, shared by the rows and the `Venue` header above them —
/// same one-constant rule as [`DOT_W`], for the same reason.
const NAME_W: f32 = 86.0;

/// The rail in the NARROW shape: one wrapped chip per venue, the name and its three dots on one
/// line. Same selection rule as [`rail_row`] (selected = a label), same dots, same hovers — only
/// the layout differs, so the accessibility tree a test reads is the same in both shapes.
fn rail_chips(
    ui: &mut egui::Ui,
    rows: &[VenueCredStatus],
    selected: &str,
    account: &AccountLabel,
    health: &StoreHealth,
    pick: &mut Option<String>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for row in rows {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                let name = egui::RichText::new(&row.venue).monospace().size(11.0);
                if row.venue == selected {
                    ui.label(name.strong().color(CONFIGURED_COLOR));
                } else if ui
                    .add(egui::Button::new(name).frame(false))
                    .on_hover_text("show this venue's credential tiers")
                    .clicked()
                {
                    *pick = Some(row.venue.clone());
                }
                for tier in TIERS {
                    tier_dot(ui, row, tier, account, health);
                }
            });
        }
    });
}

/// The detail pane's second chip, when the venue has a feed producer at all. ⚠ A BUILD fact — see
/// [`venue_detail`], where the sentence this badge may not contradict is argued at the call.
const FEED_PRODUCER_BADGE: &str = "feed producer in this build";

/// A small outlined badge — the detail pane's `Venue` / [`FEED_PRODUCER_BADGE`] chips.
fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).monospace().size(10.0).color(color));
        });
}

/// One `LABEL  value` cell of the detail pane's meta row.
fn meta_cell(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(egui::RichText::new(label).monospace().size(9.0).color(ABSENT_COLOR));
        ui.label(egui::RichText::new(value).monospace().size(11.0).color(color));
    });
}

/// The venue's KEY FAMILY, derived from [`edit_fields`] rather than written down.
///
/// Two halves, both computed: the longest common `_`-terminated prefix of every key name this
/// venue's forms write, and whether those forms are the GENERIC `{VENUE}_{TIER}_API_*` trio or a
/// shape read out of that bridge's own config loader. ⚠ Deliberately derived: a hand-written list
/// of "which venues are bespoke" is the per-venue roster this file's own module docs have twice
/// recorded rotting, and `edit_fields` is already the authority.
#[must_use]
pub fn key_family(venue: &str) -> String {
    let keys: Vec<String> = TIERS
        .iter()
        .flat_map(|tier| edit_fields(venue, tier).into_iter().map(|(_, key)| key))
        .collect();
    if keys.is_empty() {
        return "no configurable tier".to_string();
    }
    let v = venue.to_uppercase();
    let generic = TIERS.iter().all(|tier| {
        let fields = edit_fields(venue, tier);
        if fields.is_empty() {
            return true;
        }
        let want = [
            format!("{v}_{tier}_API_KEY"),
            format!("{v}_{tier}_API_SECRET"),
            format!("{v}_{tier}_API_PASSPHRASE"),
        ];
        fields.len() == want.len() && fields.iter().zip(&want).all(|((_, k), w)| k == w)
    });
    let shape = if generic { "generic" } else { "bespoke" };
    format!("{}* ({shape})", common_key_prefix(&keys))
}

/// The longest `_`-terminated common prefix of a venue's key names — `BINANCE_`, `POLY_LIVE_`,
/// `CTRADER_`. Falls back to the whole first key when the set shares no `_` boundary at all.
fn common_key_prefix(keys: &[String]) -> String {
    let first = keys[0].as_str();
    let mut end = first.len();
    for key in &keys[1..] {
        let shared =
            first.as_bytes().iter().zip(key.as_bytes()).take_while(|(a, b)| a == b).count();
        end = end.min(shared);
    }
    let head = &first[..end];
    match head.rfind('_') {
        Some(i) => head[..=i].to_string(),
        None => first.to_string(),
    }
}

/// ONE TIER ROW of the detail pane: the tier name, its state IN WORDS, the exact `.env` key names
/// the form writes (with `(optional)` carried through from [`edit_fields`]'s own UI labels), and
/// the edit affordance.
///
/// ⚠ **A tier that does not exist says so in words** — `not configurable`, plus the sentence
/// naming the venue — and offers no button. The old grid rendered it as a hollow ring
/// indistinguishable from "unset", which pointed an operator at a form that does not exist.
///
/// ⚠ **The key NAMES are rendered; no value ever is.** The strings on this row come from
/// [`account_fields`], which composes them out of the venue, the tier and the account label. This
/// function is never handed the store.
fn tier_row(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    tier: &str,
    health: &StoreHealth,
    state: &mut EditState,
) {
    let account = state.account.clone();
    let fields = account_fields(&row.venue, tier, &account);
    let cell = tier_state(row, tier, health);
    let mut open = false;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(
            [46.0, 16.0],
            egui::Label::new(egui::RichText::new(title_tier(tier)).monospace().size(12.0).strong())
                .selectable(false),
        );
        let (glyph, color) = match cell {
            TierState::Configured => (GLYPH_CONFIGURED, CONFIGURED_COLOR),
            TierState::NotSet => (GLYPH_NOT_SET, ABSENT_COLOR),
            // An em dash rather than the rail's middle dot: this cell has the WORDS beside it, so
            // the mark can be the wider one that reads as "no such thing" at text size.
            TierState::NotConfigurable => {
                ("\u{2014}", ABSENT_COLOR.gamma_multiply(NOT_CONFIGURABLE_DIM))
            }
            TierState::Unknown => (GLYPH_UNKNOWN, CONNECTING_COLOR),
        };
        ui.add_sized(
            [104.0, 16.0],
            egui::Label::new(
                egui::RichText::new(format!("{glyph} {}", cell.label()))
                    .monospace()
                    .size(11.0)
                    .color(color),
            )
            .selectable(false),
        );
        if cell == TierState::NotConfigurable {
            ui.label(
                egui::RichText::new(format!(
                    "{} has no {} tier — nothing to configure here",
                    row.venue,
                    tier.to_lowercase()
                ))
                .monospace()
                .size(10.0)
                .weak(),
            );
            return;
        }
        // ⚠ The UNKNOWN cell keeps its ✎ and its key names: the write is an in-place upsert of
        // NAMED keys and is a separate act from the read that failed, so refusing the form here
        // would remove the one affordance that might be the operator's way out. What is removed is
        // any claim about what is stored — the glyph above says `unknown`, and this line says why.
        if cell == TierState::Unknown {
            ui.label(
                egui::RichText::new("the store could not be opened — this cell was not measured")
                    .monospace()
                    .size(10.0)
                    .color(CONNECTING_COLOR),
            );
        }
        if ui
            .small_button(egui::RichText::new("\u{270E}").size(10.0))
            .on_hover_text("edit credentials")
            .clicked()
        {
            open = true;
        }
        // The exact key names, spelled out — this is the mockup's whole reason for a detail pane:
        // a hover tooltip could name ONE key, and most venues' forms write two or four.
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (label, key) in &fields {
                let optional = label.ends_with("(optional)");
                let text = if optional { format!("{key}  (optional)") } else { key.clone() };
                ui.label(egui::RichText::new(text).monospace().size(10.0).color(if optional {
                    ABSENT_COLOR
                } else {
                    egui::Color32::GRAY
                }));
            }
        });
    });
    if open {
        state.open(&row.venue, tier);
    }
}

/// `SIM` → `Sim`. The tier strings are the store's spelling; the detail pane reads better in
/// title case, and the account/tier vocabulary elsewhere in this file already lowercases them.
fn title_tier(tier: &str) -> String {
    let mut c = tier.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// The DETAIL pane for the selected venue: name, badges, the meta row, then one row per tier with
/// the inline editor expanding under whichever row was opened.
fn venue_detail(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    feed: FeedFact,
    health: &StoreHealth,
    state: &mut EditState,
    creds: CredentialWrite<'_>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(&row.venue)
                .monospace()
                .strong()
                .size(15.0)
                .color(vike_ui_theme::palette::TEXT),
        );
        ui.add_space(4.0);
        badge(ui, "Venue", ABSENT_COLOR);
        // ⚠ **A BUILD FACT, spelled as one, in the colour of one.** It renders exactly when the
        // binary handed this widget a feed-status handle for this venue, and never otherwise —
        // `FeedFact::NoProducer` is the absent case and most of the roster is in it.
        //
        // ⚠ It used to read `Streaming feed` in the CONNECTING amber, and both halves were wrong
        // beside the row underneath. `has_producer()` is true for every `FeedFact::State`,
        // `Disconnected` included, so `Streaming feed` could sit a few pixels above
        // `Status (live feed)  Disconnected` — a present-tense claim of activity contradicting the
        // measurement next to it — and the amber implied a live state this badge never reads.
        // The name now asserts only what it knows (a producer EXISTS in this build) and the muted
        // colour leaves the live half entirely to the labelled `Status` cell below, which is the
        // only thing here that reads the feed.
        if feed.has_producer() {
            badge(ui, FEED_PRODUCER_BADGE, ABSENT_COLOR);
        }
    });
    ui.add_space(3.0);

    let tiers: Vec<String> = TIERS
        .iter()
        .filter(|t| tier_state(row, t, health) != TierState::NotConfigurable)
        .map(|t| title_tier(t))
        .collect();
    let tiers = if tiers.is_empty() { "none".to_string() } else { tiers.join(" · ") };
    let feed_color = match feed {
        FeedFact::State(ConnectionState::Connected) => CONFIGURED_COLOR,
        FeedFact::State(ConnectionState::Connecting) => CONNECTING_COLOR,
        FeedFact::State(ConnectionState::Error) => ERROR_COLOR,
        _ => ABSENT_COLOR,
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        // ⚠ `Status` is the LIVE FEED's, and the label below the pane says so: it is a different
        // fact from the dots, with a different producer. See `crate::summary`'s module doc.
        meta_cell(ui, "Status (live feed)", feed.label(), feed_color);
        meta_cell(ui, "Tiers", &tiers, egui::Color32::GRAY);
        meta_cell(ui, "Key family", &key_family(&row.venue), egui::Color32::GRAY);
    });
    ui.add_space(6.0);

    for tier in TIERS {
        tier_row(ui, row, tier, health, state);
        // The inline editor expands UNDER the tier row it belongs to, so the heading, the fields
        // and the row that opened them are one block rather than a form at the foot of the pane.
        if state.target.as_ref().is_some_and(|(v, t)| v == &row.venue && t == tier) {
            render_edit_form(ui, state, creds);
        }
        ui.add_space(2.0);
    }
}

/// Render the masked edit form for `state.target`, if any. No-op when nothing is open.
///
/// `creds` carries the resolved store path and the durable ledger — see [`CredentialWrite`] for why
/// BOTH are parameters rather than walks this function performs for itself.
fn render_edit_form(ui: &mut egui::Ui, state: &mut EditState, creds: CredentialWrite<'_>) {
    let Some((venue, env_label)) = state.target.clone() else { return };
    let account = state.account.clone();
    // ⚠ `account_fields`, not `edit_fields`: the KEY NAMES this form writes carry the account, and
    // for the DEFAULT account they are the identical `String`s the venue-wide form always wrote.
    let fields = account_fields(&venue, &env_label, &account);

    ui.separator();
    // The title names the account only when there IS one to name — `AccountLabel::Default` has no
    // text BY DESIGN (its keys carry no label), so rendering one would invent a spelling that
    // appears in no file, and the default account's heading is byte-identical to before.
    let heading = match account.text() {
        None => format!("Edit {venue} / {}", env_label.to_lowercase()),
        Some(l) => format!("Edit {venue} / {} · account {l}", env_label.to_lowercase()),
    };
    ui.label(egui::RichText::new(heading).monospace().strong().size(13.0));
    ui.label(
        egui::RichText::new(
            "Fields are masked and start empty. Leave a field blank to keep its current value \
             unchanged; the existing secret is never shown here.",
        )
        .size(10.0)
        .weak(),
    );

    // Two venues pack a field list that is really TWO groups, and a thin divider between them
    // makes that visually obvious. Dukascopy's DEMO form carries two independent demo accounts
    // (DEMO1, DEMO2); cTrader's carries this tier's OAuth grant and then the tier-less Spotware
    // app registration, which is shared by all three of its cells and is the one thing about that
    // form an operator has to notice. Both groups otherwise render through the exact same generic
    // loop below (no special-cased widgets), and every other venue gets no divider at all.
    let group_break = match (venue.as_str(), env_label.as_str()) {
        ("dukascopy", "DEMO") | ("ctrader", _) => Some(2),
        _ => None,
    };

    for (i, (label, key)) in fields.iter().enumerate() {
        if group_break == Some(i) {
            ui.add_space(4.0);
            ui.separator();
        }
        ui.horizontal(|ui| {
            ui.add_sized(
                [160.0, 18.0],
                egui::Label::new(egui::RichText::new(*label).monospace().size(12.0)),
            );
            let buf = state.buffers.get_mut(i).expect("buffers sized to fields in open()");
            ui.add(
                egui::TextEdit::singleline(buf)
                    .password(true)
                    .hint_text(key.as_str())
                    .desired_width(240.0),
            );
        });
    }

    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            let updates: Vec<(String, String)> = fields
                .iter()
                .zip(state.buffers.iter())
                .filter_map(|((_, key), value)| {
                    let trimmed = value.trim();
                    if trimmed.is_empty() { None } else { Some((key.clone(), trimmed.to_string())) }
                })
                .collect();

            if updates.is_empty() {
                state.message = Some((true, "nothing entered — no changes saved".to_string()));
            } else {
                // The store write AND its durable record, together — see
                // [`save_credentials_journalled`]. `venue` is the grid row and `env_label` the
                // COLUMN the form was opened from, which is what an operator clicked; ⚠ the KEYS are
                // the authority on what was actually written, and for several venues the two
                // genuinely differ: aster's and alpaca's DEMO columns write the bridges' own
                // `ASTER_TESTNET_*` / `ALPACA_SANDBOX_*` vars, dukascopy's DEMO column writes
                // `DUKASCOPY_DEMO1_*`/`DEMO2_*`, and polymarket's LIVE column writes `POLY_*`
                // rather than `POLYMARKET_*`. `edit_fields` is where every one of those mappings
                // lives — deliberately NOT counted here, because a count is exactly the claim this
                // file has already watched rot. The record carries both cells, so neither reading
                // is lost; nothing here re-derives a tier from the key names.
                match save_credentials_journalled(creds, Actor::Gui, &venue, &env_label, &updates) {
                    Ok(()) => {
                        // The console/journald copy STAYS. It is not a duplicate of the ledger — it
                        // is the line an operator tailing the app sees now, and the ledger is what
                        // survives a rotation they will read months later. Both are built from the
                        // same two cells, so they cannot disagree; the ledger additionally carries
                        // the KEY NAMES, which this line has never had and which is exactly what a
                        // `kind`-less "saved credentials" cannot answer.
                        // NEVER log the key/secret/passphrase value — venue/env tier only.
                        //
                        // ⚠ TWO arms rather than one `account = %account` field, and the reason is
                        // the contract this change is held to: the DEFAULT account's line must be
                        // byte-identical to the one that shipped, field for field. `AccountLabel`'s
                        // `Display` renders the default account as the reserved spelling, so a
                        // single always-present field would have added a cell to every
                        // single-account box's log line to say nothing. The label itself is
                        // `[A-Z0-9]` by construction and names an ACCOUNT, never a value.
                        match account.text() {
                            None => tracing::info!(
                                kind = "credential_write",
                                venue = %venue,
                                env = %env_label.to_lowercase(),
                                "saved credentials"
                            ),
                            Some(l) => tracing::info!(
                                kind = "credential_write",
                                venue = %venue,
                                env = %env_label.to_lowercase(),
                                account = %l,
                                "saved credentials"
                            ),
                        }
                        let msg = match account.text() {
                            None => {
                                format!(
                                    "saved credentials for {venue}/{}",
                                    env_label.to_lowercase()
                                )
                            }
                            Some(l) => format!(
                                "saved credentials for {venue}/{} (account {l})",
                                env_label.to_lowercase()
                            ),
                        };
                        state.close();
                        state.message = Some((false, msg));
                        ui.ctx().request_repaint();
                    }
                    Err(e) => {
                        // io::Error's Display is an OS message about the path, never file content.
                        state.message = Some((true, format!("save failed: {e}")));
                    }
                }
            }
        }
        if ui.button("Cancel").clicked() {
            state.close();
        }
    });
}

/// One account chip. The SELECTED one is a label, not a button — so "which account am I looking
/// at" is answerable from the accessibility tree by role alone, and so the current account cannot
/// be re-picked into a no-op frame.
fn account_chip(
    ui: &mut egui::Ui,
    text: &str,
    label: &AccountLabel,
    selected: &AccountLabel,
    pick: &mut Option<AccountLabel>,
) {
    if label == selected {
        let chip = egui::RichText::new(format!("\u{25CF} {text}"))
            .monospace()
            .size(12.0)
            .strong()
            .color(CONFIGURED_COLOR);
        ui.label(chip);
    } else if ui.button(egui::RichText::new(text).monospace().size(12.0)).clicked() {
        *pick = Some(label.clone());
    }
}

/// **The account dimension: a SELECTOR above the grid, not a third axis inside it.**
///
/// # Why the grid does not grow
///
/// The table below is already the whole bridge roster times three tiers, with a Status column and
/// an edit affordance in every cell. An account dimension has two other homes and both are worse:
///
/// * **More ROWS** (one row per venue per account, the shape the Data Manager's Venues tab uses)
///   turns a fifteen-row table into a fifteen-times-N one in which, on a real box, fourteen out of
///   every fifteen rows are the default account's — the second account is what the operator came
///   for and it is what gets lost. That tab can afford the shape because its rows are ARMING rows:
///   a handful, produced by `vike_run::venue_arming` from the policy file, not the full roster.
/// * **More COLUMNS** (Sim/Demo/Live per account) is three-times-N columns in a pane that already
///   carries five, and the columns that fall off the right edge are silently unreachable.
///
/// A selector keeps the table's shape CONSTANT in N. That is not only a legibility argument: it is
/// what makes "a box with no labelled account is unchanged" a structural property. With no labelled
/// account there is one chip, it is the one already selected, and every path below composes key
/// names through [`AccountLabel::Default`], which returns them unchanged — so the grid, its
/// tooltips, its forms, the keys it writes and the record it journals are the ones that shipped.
///
/// Hummingbot's is the same shape reached from the other direction: a named account container with
/// credentials submitted against an (account, connector) pair, one account in view at a time
/// (`vike_model::account_keys`' module doc surveys it, along with the two competitors that answer
/// differently).
///
/// # Creating one
///
/// **Add account** takes a label and nothing else, and it WRITES NOTHING. An account comes into
/// existence when its first credential is saved, which is the same rule the read side already
/// obeys — [`AccountGrids::from_vars`] derives the label set from the key names in the store, so
/// there is no registry a Create could add a row to and nothing that could disagree with the
/// store. Until then the label is selected, its grid reads all-absent, and it is marked as such.
///
/// ⚠ **NOT `vike_model::account_keys::accounts_in_store`**, and the difference is load-bearing for
/// the venues [`edit_fields`] repaired. That enumerator classifies a key only when it parses as
/// `{VENUE}_{TIER}{SUFFIX}` over the canonical roster and tier lists, so it answers `None` for
/// alpaca's `SANDBOX` tier token and for polymarket's `POLY_` prefix — the exact names this
/// editor's alpaca `DEMO` and polymarket `LIVE` forms now write. Were the strip built on it, an
/// operator could save a labelled credential for either venue, watch its dot light, and find the
/// chip gone on the next load. [`AccountGrids::from_vars`] is built on `split_account_key` plus
/// "does this account's grid light anything" instead, precisely so that cannot happen; its own doc
/// carries the full argument and `crates/vike-connections/tests/account_grids.rs` measures the gap.
///
/// The label is validated by `AccountLabel::parse` BEFORE it is accepted, and the refusal rendered
/// is that validator's own `Display` — see [`EditState::create_account`].
///
/// # Removing one
///
/// **There is no affordance, and that is the design.** The root `CLAUDE.md` rule is that nothing in
/// this workspace deletes, moves, truncates or wholesale-rewrites the credential store: it is the
/// operator's only copy of live venue keys, and the one sanctioned write is
/// `vike_secrets::save_credentials`' in-place upsert of NAMED keys. A Remove button could only be
/// implemented by deleting lines from that file. So this renders an INSTRUCTION naming the file and
/// the line shape instead, which is the same answer `vike-cli secrets path` gives for creating the
/// store in the first place.
fn account_strip(
    ui: &mut egui::Ui,
    grids: &AccountGrids,
    state: &mut EditState,
    store: &std::path::Path,
) {
    let selected = state.account.clone();
    let mut pick: Option<AccountLabel> = None;
    let mut open_add = false;

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Account").monospace().strong().size(12.0));
        // The default account is ALWAYS offered and always first: it is the account a
        // single-account box has, not the absence of one.
        account_chip(ui, "default", &AccountLabel::Default, &selected, &mut pick);
        for label in grids.labels() {
            let text = label.text().unwrap_or_default().to_string();
            account_chip(ui, &text, label, &selected, &mut pick);
        }
        // A label the operator has just NAMED has no keys yet, so it is in no store enumeration —
        // it is rendered from the selection itself, marked, so the strip does not silently drop the
        // account they are in the middle of filling in.
        if grids.grid_for(&selected).is_none()
            && let Some(l) = selected.text()
        {
            let chip = egui::RichText::new(format!("\u{25CF} {l} (new)"))
                .monospace()
                .size(12.0)
                .strong()
                .color(CONNECTING_COLOR);
            ui.label(chip);
        }
        if ui.button(egui::RichText::new("Add account").size(12.0)).clicked() {
            open_add = true;
        }
    });

    if let Some(label) = pick {
        state.select_account(label);
    }
    if open_add {
        // The two forms are mutually exclusive modes of one panel: leaving a half-typed credential
        // form up while the account it would be written INTO is being renamed is the one state in
        // which a Save could land somewhere the operator is not looking.
        state.close();
        state.add_label = Some(String::new());
        state.label_error = None;
    }

    let mut create = false;
    let mut cancel = false;
    if let Some(buf) = state.add_label.as_mut() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("new account label").monospace().size(11.0));
            // ⚠ NO `char_limit`: it would TRUNCATE an over-long paste to a legal label the
            // operator did not choose, and they would then write the name they meant into
            // `policy.toml` and find it matching nothing. That is the same repair
            // `AccountLabel::parse` refuses when it declines to uppercase `alt` — a spelling
            // the program fixes on your behalf is a spelling nobody learns. The validator
            // REFUSES, naming the length it was given.
            ui.add(egui::TextEdit::singleline(buf).hint_text("ALT").desired_width(160.0));
            create = ui.button("Create").clicked();
            cancel = ui.button("Cancel").clicked();
        });
        let rule = format!(
            "A-Z and 0-9 only, at most {MAX_LABEL_LEN} characters, and not \
             `{RESERVED_DEFAULT_LABEL}` (that is the account an unlabelled key already addresses). \
             Creating one writes nothing — the account exists once you save its first credential \
             below."
        );
        ui.label(egui::RichText::new(rule).monospace().size(10.0).weak());
    }
    if cancel {
        state.add_label = None;
        state.label_error = None;
    } else if create {
        let text = state.add_label.clone().unwrap_or_default();
        state.create_account(&text);
    }
    if let Some(err) = &state.label_error {
        ui.label(egui::RichText::new(err.as_str()).monospace().size(10.0).color(ERROR_COLOR));
    }

    // The two things only a LABELLED account needs said: that it may hold nothing yet, and how it
    // is removed. A default-account box renders neither, so its panel is the panel that shipped
    // plus the one strip above.
    if let Some(l) = state.account.text() {
        if grids.grid_for(&state.account).is_none() {
            let empty = format!(
                "account {l} holds no credentials in this store yet — the dots below read `not \
                 set` for that reason, not because a key is missing from an account that exists."
            );
            ui.label(egui::RichText::new(empty).monospace().size(10.0).weak());
        }
        let removal = format!(
            "To REMOVE account {l}, delete its `{ACCOUNT_SEPARATOR}{l}` lines from {} by hand. \
             Nothing in vike deletes a credential: the store is your only copy of live venue keys, \
             so the one write this app performs is an in-place upsert of the named keys it was \
             asked to save.",
            store.display()
        );
        ui.label(egui::RichText::new(removal).monospace().size(10.0).weak());
    }
    ui.separator();
}

/// The egui temp slot this panel's whole selection + form state lives in, keyed off the `Ui` it is
/// rendered on. ONE derivation, so [`shown_account`] and [`connections_ui`] cannot key differently.
fn state_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("vike_connections_edit_state")
}

/// **Which account [`connections_ui`] will render, asked BEFORE it renders.**
///
/// The tool's segmented control and its status line carry a COUNT of this panel's dots, and they
/// are drawn above the panel — so the count has to be derivable from outside without waiting a
/// frame for the panel to report it. A stale-by-one-frame count is exactly the "plausible number"
/// this tool is not allowed to render.
///
/// ⚠ **Call it with the SAME `Ui` you then hand to [`connections_ui`].** Both read [`state_id`],
/// which is derived from `ui.id()`; adding widgets between the two calls does not change that id,
/// but putting the panel inside a container would, and the two would then answer about different
/// slots. This function READS the slot and never writes it, and it applies `preselect` the way the
/// panel will (through the same equality check `EditState::select_account` makes), so the answer
/// is the account the very next call renders.
#[must_use]
pub fn shown_account(ui: &egui::Ui, preselect: Option<&AccountLabel>) -> AccountLabel {
    if let Some(p) = preselect {
        return p.clone();
    }
    let state: EditState = ui.data(|d| d.get_temp(state_id(ui))).unwrap_or_default();
    state.account
}

/// Render the Credentials tab: the account strip, the venue RAIL, the selected venue's DETAIL
/// pane (badges, meta row, one row per tier spelling its `.env` key names out) and the masked
/// inline edit form that writes them.
///
/// ⚠ **Two shapes, one accessibility tree.** Above [`RAIL_DETAIL_MIN_W`] the rail is a fixed
/// column beside the detail pane; below it the rail becomes a wrapped chip row above the detail
/// pane. Both render the same dots with the same hovers and the same
/// selected-venue-is-a-`Label` rule, so nothing a test reads off the tree depends on the width.
///
/// `live` maps a venue name (matching [`crate::status::VENUES`]'s spelling, e.g. `"binance"`) to
/// its current [`ConnectionState`]; a venue absent from the map is reported as having NO PRODUCER
/// rather than as `Unknown` ([`crate::summary::FeedFact`]). The binary
/// supplies a real state for every venue with a live feed producer today
/// (binance/bybit/okx/aster/hyperliquid/polymarket); venues without one (deribit, the FX/broker
/// venues) stay absent, and the detail pane says there is no producer rather than inventing one.
///
/// `creds` is where a **Save** click lands and where it is RECORDED — the credential store path and
/// the change journal, both resolved by the binary's one boot walk. See [`CredentialWrite`]; the
/// store path used to be found here by a `$VIKE_SETTINGS_DIR`-blind walk, which is also why this
/// widget's Save arm was untestable (it wrote the developer's real credential store).
///
/// `grids` carries ONE credential grid per account the store holds — [`AccountGrids`], whose
/// `labelled` half is EMPTY on a single-account box, in which case this renders the default
/// account's `Vec` and nothing else changes. The account being shown is picked in the strip at the
/// top (`account_strip`, which carries the whole design argument for a selector rather than more
/// rows or more columns) and lives in the widget's own temp memory, like the edit form's buffers.
///
/// `preselect` is a ONE-SHOT override of that selection, and it exists for exactly one caller: the
/// `connections-account` QA capture arm (`vike_app_core::startup`), which needs this panel to open
/// on a LABELLED account so that the chip strip and the removal-instruction line are on a contact
/// sheet at all. `None` — every other frame, and every frame of every other launch — leaves the
/// selection entirely to the temp state, so the panel behaves exactly as it did before the
/// parameter existed.
///
/// ⚠ **It is applied through [`EditState::select_account`], not by assignment**, so a preselect
/// arriving while a credential form is open closes that form: the buffers were typed against the
/// account that was selected when it opened, and composing them into another account's key names
/// is the one way this panel could write a live key somewhere nobody is looking. Being one-shot is
/// the CALLER's job (`ToolView::connections_account` is `take`n) — this function honours whatever
/// it is handed, every frame it is handed one.
pub fn connections_ui(
    ui: &mut egui::Ui,
    grids: &AccountGrids,
    live: &HashMap<String, ConnectionState>,
    health: &StoreHealth,
    creds: CredentialWrite<'_>,
    preselect: Option<AccountLabel>,
) {
    let state_id = state_id(ui);
    let mut state: EditState = ui.data_mut(|d| d.get_temp(state_id)).unwrap_or_default();

    if let Some(account) = preselect {
        state.select_account(account);
    }

    // ⚠ **THE BANNER THAT MUST COME FIRST.** `grids` was folded from the map the loader returned,
    // and that loader is INFALLIBLE: a store that exists and cannot be opened logs an error and
    // returns an EMPTY map, byte-identical to an absent one. Every bool below is then `false` for
    // a reason that is not "no credentials". The dots say `unknown` for themselves
    // (`crate::summary::TierState::Unknown`) — this says WHY, once, at the top, with the fault
    // verbatim, because the per-cell mark cannot carry a path and an OS reason.
    //
    // `SecretsError`'s `Display` is a path and an errno and never file contents (its own doc is
    // the authority), so rendering it here cannot render a credential.
    if let StoreHealth::Unreadable(why) = health {
        ui.label(
            egui::RichText::new(format!(
                "⚠ the credential store could not be opened — nothing below was measured: {why}"
            ))
            .monospace()
            .size(11.0)
            .color(ERROR_COLOR),
        );
        ui.add_space(4.0);
    }

    account_strip(ui, grids, &mut state, creds.store);

    // The rows the grid renders are THIS ACCOUNT's. A label with nothing in the store yet — one an
    // operator has just named — renders all-absent rather than the default account's dots, which is
    // `credential_status_for_account`'s own no-borrowing rule carried up into the view.
    let absent;
    let statuses: &[VenueCredStatus] = match grids.grid_for(&state.account) {
        Some(rows) => rows,
        None => {
            absent = grids.absent_grid();
            &absent
        }
    };

    // The selection is stored by NAME, so it survives a reorder and can be repaired when it names
    // a venue this grid does not carry (the default empty string on frame one, or a roster that
    // shrank under a persisted selection). Assigned DIRECTLY rather than through `select_venue`:
    // this is the resolution of an unset selection, not an operator switching venues, and running
    // the switch path would close a form on the frame it was opened.
    if !statuses.iter().any(|s| s.venue == state.venue)
        && let Some(first) = statuses.first()
    {
        state.venue = first.venue.clone();
        state.close();
    }

    let mut pick: Option<String> = None;
    let selected = state.venue.clone();
    let row = statuses.iter().find(|s| s.venue == selected).cloned();

    // ⚠ The two shapes differ in LAYOUT only. Both render the same rail dots with the same hovers
    // and the same selected-row-is-a-Label rule, so an accessibility-tree assertion holds in
    // either — which is what lets the headless tests drive one width and mean both.
    let two_column = ui.available_width() >= RAIL_DETAIL_MIN_W;
    if two_column {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(RAIL_W, ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.allocate_ui_with_layout(
                            egui::vec2(NAME_W, 14.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(NAME_W);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Venue")
                                            .monospace()
                                            .strong()
                                            .size(10.0)
                                            .color(ABSENT_COLOR),
                                    )
                                    .selectable(false),
                                );
                            },
                        );
                        for t in ["S", "D", "L"] {
                            ui.add_sized(
                                [DOT_W, 14.0],
                                egui::Label::new(
                                    egui::RichText::new(t)
                                        .monospace()
                                        .strong()
                                        .size(10.0)
                                        .color(ABSENT_COLOR),
                                )
                                .selectable(false),
                            );
                        }
                    });
                    ui.separator();
                    for s in statuses {
                        rail_row(ui, s, s.venue == selected, &state.account, health, &mut pick);
                    }
                },
            );
            ui.separator();
            ui.vertical(|ui| {
                if let Some(s) = &row {
                    venue_detail(ui, s, FeedFact::of(&s.venue, live), health, &mut state, creds);
                }
            });
        });
    } else {
        rail_chips(ui, statuses, &selected, &state.account, health, &mut pick);
        ui.separator();
        if let Some(s) = &row {
            venue_detail(ui, s, FeedFact::of(&s.venue, live), health, &mut state, creds);
        }
    }

    if statuses.is_empty() {
        ui.label(
            egui::RichText::new("no venue rows to show").monospace().size(11.0).color(ABSENT_COLOR),
        );
    }

    if let Some(venue) = pick {
        state.select_venue(&venue);
    }

    if let Some((is_error, text)) = &state.message {
        ui.add_space(4.0);
        let color = if *is_error { ERROR_COLOR } else { CONFIGURED_COLOR };
        ui.label(egui::RichText::new(text.as_str()).monospace().size(11.0).color(color));
    }

    // ------------------------------------------------------------------------------------------
    // The rail's footer — the two sentences that keep it honest.
    // ------------------------------------------------------------------------------------------
    ui.add_space(6.0);
    ui.separator();
    // ⚠ THE CORRECTED NOTE. This panel used to carry "Status reflects the live feed, not the
    // credential grid above", which was true OF A COLUMN THAT NO LONGER EXISTS: the grid's Status
    // column was the feed's, and the Sim/Demo/Live dots were the store's. The rail's dots are the
    // STORE's and only the store's, so repeating the old sentence here would attach the feed
    // claim to the wrong glyph. The feed fact keeps its own labelled cell in the detail pane
    // instead, and `crate::summary`'s module doc carries why the two may never share one dot.
    ui.label(
        egui::RichText::new(
            "The three rail marks are CREDENTIAL PRESENCE in this store, for the account selected \
             above — ● every key that tier's form writes is set, ○ the tier exists and is unset, · \
             this venue has no such tier, ? the store could not be opened and nothing was \
             measured. Only the first two are measurements, which is why only they are dots. They \
             say nothing about whether a venue is reachable or armed: the LIVE FEED's state is the \
             detail pane's own `Status` row, it comes from a different producer, and a venue with \
             no feed producer in this build reports none rather than reading Unknown.",
        )
        .monospace()
        .size(10.0)
        .weak(),
    );
    ui.label(
        egui::RichText::new(
            "FX venues (oanda/ig/fxcm/dukascopy) use bespoke .env key names, detected via each \
             bridge's own var names — the detail pane spells every key out. Dukascopy has no \
             Sim/Live tier; only Demo (DEMO1/DEMO2) is ever configurable for it.",
        )
        .monospace()
        .size(10.0)
        .weak(),
    );

    ui.data_mut(|d| d.insert_temp(state_id, state));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A dot's tooltip names what that cell's form would write**, account included.
    ///
    /// `expected_key_name` and `edit_fields` are two independent tables, and the account dimension
    /// is applied to each by its own wrapper — so a wrapper that forgot to compose the label would
    /// leave a labelled cell hovering the DEFAULT account's key name over a form writing the
    /// labelled one, which is the tooltip lying about which account is about to be rotated.
    ///
    /// Folded over the whole roster and every tier rather than spot-checked, and stated as the
    /// agreement between the two functions rather than against a written-out key name — a fixture
    /// spelling a labelled key would have to live in `tests/`, and stating it this way needs no
    /// literal at all.
    #[test]
    fn a_cells_tooltip_names_the_same_account_key_its_form_writes() {
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        for &venue in crate::status::VENUES {
            for tier in ["SIM", "DEMO", "LIVE"] {
                for account in [&AccountLabel::Default, &alt] {
                    let fields = account_fields(venue, tier, account);
                    let Some((_, first)) = fields.first() else {
                        continue; // no tier for this venue — the cell offers no form at all
                    };
                    assert_eq!(
                        &account_expected_key_name(venue, tier, account),
                        first,
                        "{venue}/{tier} for account {account}: the hovered key name and the \
                         form's first field must be the same key"
                    );
                }
            }
        }
    }

    /// Dukascopy's DEMO edit form must offer both DEMO1 and DEMO2 field groups (four fields
    /// total, DEMO1 first) — the whole point of this change: today's form only ever wrote DEMO1,
    /// leaving no in-app way to set DEMO2 (`DUKASCOPY_DEMO2_LOGIN`/`_PASSWORD`, the EU demo
    /// account) even though `crate::status` already detects it.
    #[test]
    fn dukascopy_demo_edit_fields_cover_both_accounts() {
        let fields = edit_fields("dukascopy", "DEMO");
        let keys: Vec<&str> = fields.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "DUKASCOPY_DEMO1_LOGIN",
                "DUKASCOPY_DEMO1_PASSWORD",
                "DUKASCOPY_DEMO2_LOGIN",
                "DUKASCOPY_DEMO2_PASSWORD",
            ]
        );
    }

    /// Dukascopy has no SIM/LIVE tier — those two cells must still offer no edit affordance at
    /// all (unchanged behavior from before this change).
    #[test]
    fn dukascopy_sim_and_live_have_no_edit_fields() {
        assert!(edit_fields("dukascopy", "SIM").is_empty());
        assert!(edit_fields("dukascopy", "LIVE").is_empty());
    }

    /// Every other venue's field set is untouched by this change (spot-check one generic venue
    /// and one bespoke FX venue).
    #[test]
    fn non_dukascopy_venues_unaffected() {
        let binance = edit_fields("binance", "LIVE");
        let keys: Vec<&str> = binance.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_SECRET", "BINANCE_LIVE_API_PASSPHRASE"]
        );

        let fxcm = edit_fields("fxcm", "DEMO");
        let keys: Vec<&str> = fxcm.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(keys, vec!["FXCM_DEMO_USER", "FXCM_DEMO_PASSWORD"]);
    }

    /// Aster's `DEMO` cell writes the bridge's `TESTNET`-prefixed vars (not `ASTER_DEMO_*`), plus
    /// an optional `Signer` field alongside the required `User`/`Private Key` pair.
    #[test]
    fn aster_edit_fields_demo_uses_testnet_tier() {
        let fields = edit_fields("aster", "DEMO");
        let keys: Vec<&str> = fields.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["ASTER_TESTNET_USER", "ASTER_TESTNET_PRIVATE_KEY", "ASTER_TESTNET_SIGNER"]
        );
        let labels: Vec<&str> = fields.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, vec!["User", "Private Key", "Signer (optional)"]);
    }

    /// Aster's `LIVE` cell maps 1:1 to `ASTER_LIVE_*`.
    #[test]
    fn aster_edit_fields_live_uses_live_tier() {
        let fields = edit_fields("aster", "LIVE");
        let keys: Vec<&str> = fields.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(keys, vec!["ASTER_LIVE_USER", "ASTER_LIVE_PRIVATE_KEY", "ASTER_LIVE_SIGNER"]);
    }

    /// Aster has no `SIM` tier — that cell must offer no edit affordance.
    #[test]
    fn aster_sim_has_no_edit_fields() {
        assert!(edit_fields("aster", "SIM").is_empty());
    }

    /// `expected_key_name`'s hover tooltip follows the same `DEMO`→`TESTNET` tier mapping.
    #[test]
    fn aster_expected_key_name_uses_testnet_for_demo() {
        assert_eq!(expected_key_name("aster", "DEMO"), "ASTER_TESTNET_USER");
        assert_eq!(expected_key_name("aster", "LIVE"), "ASTER_LIVE_USER");
    }

    /// Hyperliquid's edit form must write the bridge's real vars (`_PRIVATE_KEY` +
    /// optional `_ACCOUNT_ADDRESS`) — the generic fallback would write
    /// `HYPERLIQUID_*_API_KEY`/`_API_SECRET`, which `vike_hyperliquid::config::load`
    /// never reads.
    #[test]
    fn hyperliquid_edit_fields_write_private_key_shape() {
        for tier in ["DEMO", "LIVE"] {
            let fields = edit_fields("hyperliquid", tier);
            let keys: Vec<&str> = fields.iter().map(|(_, k)| k.as_str()).collect();
            assert_eq!(
                keys,
                vec![
                    format!("HYPERLIQUID_{tier}_PRIVATE_KEY"),
                    format!("HYPERLIQUID_{tier}_ACCOUNT_ADDRESS"),
                ]
            );
        }
    }

    #[test]
    fn hyperliquid_sim_has_no_edit_fields() {
        assert!(edit_fields("hyperliquid", "SIM").is_empty());
    }

    #[test]
    fn hyperliquid_expected_key_name_is_private_key() {
        assert_eq!(expected_key_name("hyperliquid", "DEMO"), "HYPERLIQUID_DEMO_PRIVATE_KEY");
        assert_eq!(expected_key_name("hyperliquid", "LIVE"), "HYPERLIQUID_LIVE_PRIVATE_KEY");
    }

    /// ⚠ **`key_family`'s own per-venue assertions live in
    /// `crates/vike-connections/tests/editor_key_shapes.rs`, not here**, and it is FORCED — the
    /// same rule that put `edit_fields`' fixtures there. Spelling a venue key PREFIX in a `src/`
    /// test region (`BINANCE_`, `POLY_`, `ASTER_`, `CTRADER_`) makes
    /// `crates/vike-ops/tests/settings_registry.rs`'s `every_read_variable_is_declared` harvest it
    /// as an env-var-shaped literal and demand a `vike_ops::settings::SETTINGS` row asserting a
    /// read this crate does not perform. MEASURED: five such prefixes reddened that gate before
    /// the assertions moved. What stays here is only what spells no such literal.
    ///
    /// The prefix-cutting HELPER is private, so its own edge cases have to be tested here — with
    /// inputs that are deliberately not venue-shaped.
    #[test]
    fn the_common_prefix_is_cut_on_an_underscore_boundary() {
        assert_eq!(common_key_prefix(&["ONLY_ONE".to_string()]), "ONLY_");
        assert_eq!(
            common_key_prefix(&["ABC_DEF".to_string(), "ABC_XYZ".to_string()]),
            "ABC_",
            "the cut lands on the shared `_` boundary, never mid-token"
        );
        // No shared `_` boundary at all: the whole first key rather than an empty string.
        assert_eq!(common_key_prefix(&["AB".to_string(), "XY".to_string()]), "AB");
    }

    /// `SIM` renders as `Sim` in the detail pane's tier column.
    #[test]
    fn tier_names_render_in_title_case() {
        assert_eq!(title_tier("SIM"), "Sim");
        assert_eq!(title_tier("DEMO"), "Demo");
        assert_eq!(title_tier("LIVE"), "Live");
        assert_eq!(title_tier(""), "");
    }

    /// ⚠ Switching VENUE closes an open form, exactly as switching ACCOUNT does — the buffers
    /// were typed against the venue selected when the form opened, and `account_fields` composes
    /// them against whatever venue is selected at Save time.
    #[test]
    fn switching_venue_closes_an_open_form() {
        let mut state = EditState { venue: "binance".to_string(), ..EditState::default() };
        state.open("binance", "LIVE");
        assert!(state.target.is_some());

        state.select_venue("binance");
        assert!(state.target.is_some(), "re-selecting the same venue is a no-op");

        state.select_venue("bybit");
        assert!(state.target.is_none(), "the form must close on a venue switch");
        assert!(state.buffers.is_empty());
    }

    /// `EditState::open` sizes its buffer vec to `edit_fields`'s length — with DEMO2 added that
    /// must now be 4 empty buffers for dukascopy/DEMO, not 2. Every buffer starts empty (rule:
    /// an existing secret's plaintext is never read back into the UI).
    #[test]
    fn edit_state_open_sizes_buffers_for_dukascopy_demo() {
        let mut state = EditState::default();
        state.open("dukascopy", "DEMO");
        assert_eq!(state.buffers.len(), 4);
        assert!(state.buffers.iter().all(String::is_empty));
        assert_eq!(state.target, Some(("dukascopy".to_string(), "DEMO".to_string())));
    }
}
