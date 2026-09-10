//! egui render of the credential-status grid + a live connection-status column + in-app masked
//! key editing.
//!
//! Connect/disconnect controls are still out of scope — see [`crate::status`] for the tested pure
//! credential-status logic and [`vike_model::feed_status`] for the tested pure live connection-state
//! model. **STATUS SOURCES**: the Status column reads whatever live sources vike-app threads in
//! via the `live` map — today that is every venue with a live feed producer
//! (binance/bybit/okx/aster/hyperliquid/polymarket, each parsed via
//! [`crate::parse_feed_status`]); a venue with no producer yet (deribit, the FX/broker
//! venues) is absent from the map and always renders `Unknown`. Extending coverage is purely a
//! matter of vike-app adding that venue's status handle to the map it passes here. Key editing IS
//! in scope here: each credential cell gets a small edit affordance that opens a masked (password)
//! form and writes through [`crate::env_write::save_credentials`].
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

/// Short label + dot color for a live [`ConnectionState`], rendered in the Status column.
/// `Disconnected` and `Unknown` intentionally share the same muted grey — the column can't tell
/// them apart visually (both mean "nothing live to show"), only the label text differs.
fn connection_state_label_color(state: ConnectionState) -> (&'static str, egui::Color32) {
    match state {
        ConnectionState::Connected => ("Connected", CONFIGURED_COLOR),
        ConnectionState::Connecting => ("Connecting", CONNECTING_COLOR),
        ConnectionState::Error => ("Error", ERROR_COLOR),
        ConnectionState::Disconnected => ("Disconnected", ABSENT_COLOR),
        ConnectionState::Unknown => ("Unknown", ABSENT_COLOR),
    }
}

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
/// [`status_cell`] renders the ✎ only when this list is non-empty, so an empty list is how a cell
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
        // loader reads them in. `status_cell` hovers `expected_key_name`, and
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

fn status_cell(
    ui: &mut egui::Ui,
    configured: bool,
    venue: &str,
    env_label: &str,
    state: &mut EditState,
) {
    // The account the tooltip must name is the one the grid is SHOWING — cloned before the
    // closure so the borrow of `state` inside it stays exclusive.
    let account = state.account.clone();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (glyph, color) =
            if configured { ("\u{25CF}", CONFIGURED_COLOR) } else { ("\u{25CB}", ABSENT_COLOR) };
        ui.label(egui::RichText::new(glyph).monospace().size(14.0).color(color))
            .on_hover_text(account_expected_key_name(venue, env_label, &account));

        if !edit_fields(venue, env_label).is_empty()
            && ui
                .small_button(egui::RichText::new("\u{270E}").size(10.0))
                .on_hover_text("edit credentials")
                .clicked()
        {
            state.open(venue, env_label);
        }
    });
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

/// Render the venues x Sim/Demo/Live credential-status grid (with a leading live Status column)
/// and a masked in-app edit form for adding/updating keys in the workspace `.env`.
///
/// `live` maps a venue name (matching [`crate::status::VENUES`]'s spelling, e.g. `"binance"`) to
/// its current [`ConnectionState`]; a venue absent from the map renders `Unknown`. vike-app
/// supplies a real state for every venue with a live feed producer today
/// (binance/bybit/okx/aster/hyperliquid/polymarket); venues without one (deribit, the FX/broker
/// venues) stay absent and render `Unknown`.
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
    creds: CredentialWrite<'_>,
    preselect: Option<AccountLabel>,
) {
    let state_id = ui.id().with("vike_connections_edit_state");
    let mut state: EditState = ui.data_mut(|d| d.get_temp(state_id)).unwrap_or_default();

    if let Some(account) = preselect {
        state.select_account(account);
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

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{25CF}").monospace().color(CONFIGURED_COLOR));
        ui.label(egui::RichText::new("configured").monospace().size(11.0));
        ui.add_space(10.0);
        ui.label(egui::RichText::new("\u{25CB}").monospace().color(ABSENT_COLOR));
        ui.label(egui::RichText::new("not set").monospace().size(11.0));
        ui.add_space(10.0);
        ui.label(egui::RichText::new("\u{270E}").monospace().size(11.0));
        ui.label(egui::RichText::new("edit").monospace().size(11.0));
    });
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "FX venues (oanda/ig/fxcm/dukascopy) use bespoke .env key names, detected via each \
             bridge's own var names (hover a dot for the exact key). Dukascopy has no Sim/Live \
             tier — only Demo (DEMO1/DEMO2) is ever configurable for it.",
        )
        .monospace()
        .size(10.0)
        .weak(),
    );
    ui.label(
        egui::RichText::new(
            "Status reflects the live feed, not the credential grid above: the streaming feeds \
             (binance/bybit/okx/aster/hyperliquid/polymarket) show their real connection state; \
             a venue with no live feed producer yet (deribit, the FX/broker venues) reads Unknown \
             until one is wired in.",
        )
        .monospace()
        .size(10.0)
        .weak(),
    );
    ui.separator();

    egui::Grid::new("vike_connections_grid")
        .num_columns(5)
        .spacing([18.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Venue").monospace().strong().size(12.0));
            ui.label(egui::RichText::new("Status").monospace().strong().size(12.0));
            ui.label(egui::RichText::new("Sim").monospace().strong().size(12.0));
            ui.label(egui::RichText::new("Demo").monospace().strong().size(12.0));
            ui.label(egui::RichText::new("Live").monospace().strong().size(12.0));
            ui.end_row();

            for s in statuses {
                ui.label(egui::RichText::new(&s.venue).monospace().size(12.0));
                let conn_state = live.get(&s.venue).copied().unwrap_or_default();
                let (label, color) = connection_state_label_color(conn_state);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(egui::RichText::new("\u{25CF}").monospace().size(12.0).color(color));
                    ui.label(egui::RichText::new(label).monospace().size(11.0).color(color));
                });
                status_cell(ui, s.sim, &s.venue, "SIM", &mut state);
                status_cell(ui, s.demo, &s.venue, "DEMO", &mut state);
                status_cell(ui, s.live, &s.venue, "LIVE", &mut state);
                ui.end_row();
            }
        });

    if let Some((is_error, text)) = &state.message {
        ui.add_space(4.0);
        let color = if *is_error { ERROR_COLOR } else { CONFIGURED_COLOR };
        ui.label(egui::RichText::new(text.as_str()).monospace().size(11.0).color(color));
    }

    render_edit_form(ui, &mut state, creds);

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
