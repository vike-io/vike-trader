//! egui render of the Connections tool's **Credentials** tab — a venue RAIL, a per-venue DETAIL
//! pane, and in-app masked key editing.
//!
//! Connect/disconnect controls are NOT here: the live backend connection is process-level state
//! rendered by the tool's own ambient strip (`vike_app_core::tool_views::connections`), identical
//! on both tabs, so that one fact has one rendering.
//!
//! # ⚠⚠ THE TWO AXES ARE GOVERNED BY OPPOSITE RULES, AND BOTH WERE GOT WRONG ONCE
//!
//! A tool window AUTO-SIZES TO ITS CONTENT on BOTH axes — `egui-0.36.1`'s `Window::show_dyn`
//! builds its `Resize` with `.with_stroke(false)` and then `resizable(false)`, and `Resize::end`
//! then reports `size[d] = last_content_size[d]`, which `Resize::begin` feeds back as
//! `desired_size = desired_size.max(last_content_size)`. So the widest and the tallest thing this
//! file draws is not merely a layout choice: it is an INSTRUCTION about how big the Connections
//! window should be. The owner read the result off a live capture as *"i see ui is disbalanced"*
//! TWICE, and the two reports were opposite defects:
//!
//! * **Round one — content demanding a window it did not need.** A ~250-character legend SENTENCE
//!   as a wrapping `Label` (a `Label` wraps at `ui.available_width()`, so in a maximized window it
//!   laid out on ONE LINE, edge to edge, above a panel whose real content was ~700pt), and a rail
//!   allocated `egui::vec2(RAIL_W, ui.available_height())` with a vertical `ui.separator()` beside
//!   it taking the same height again. A ~1250pt window whose rail ended at y≈600 and whose detail
//!   pane ended at y≈400, with `connections_body`'s foot strip correctly pinned to a floor 600pt
//!   below the last thing it described. The strip was never the defect.
//! * **Round two — content REFUSING the window it was given.** The cure for round one was a
//!   `PANEL_MAX_W` measure over the whole panel, and that capped the column the approved design
//!   leaves uncapped: the grid is `minmax(190px, 232px) minmax(0, 1fr)`, so the RAIL is bounded
//!   and the DETAIL pane is `1fr`. Measured on a maximized 2560x1600 live capture: an ~866pt
//!   column hugging the top-left with every rule stopping a third of the way across. The
//!   tombstone above [`NOTE_W`] carries it.
//!
//! **The rule that falls out, and it is the one to carry into any future edit here:**
//!
//! * **WIDTH — FILL IT.** `ui.available_width()` is read outright by the panel container and by
//!   the detail column. That is a FIXED POINT of the sizer's feedback line rather than a runaway
//!   (`x.max(x) == x`), and it is verified by
//!   `crates/vike-connections/tests/connections_layout.rs`'s
//!   `the_windows_sizing_loop_converges_with_a_filling_detail_pane`, which drives the real
//!   `egui::Resize`. The things that may NOT fill it are PROSE ([`NOTE_W`]) and the RAIL
//!   ([`RAIL_MIN_W`]/[`RAIL_MAX_W`]) — a paragraph and a name column are content with a natural
//!   width, and a `Label` wrapping at the arena is the one shape that genuinely grows without
//!   bound.
//! * **HEIGHT — NEVER READ IT.** `ui.available_height()` appears nowhere in this file and may not.
//!   There is no vertical analogue of "fill": the panel is a FORM, its natural height is the sum
//!   of its rows, and a window taller than that is slack the foot strip's pinning already accounts
//!   for. A greedy height read is round one again.
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
//! carries the whole argument — why not more rows, why not more columns, how an account is
//! created, and what removal is (a ROW act, behind a typed confirm, refused while that row still
//! owns credentials — `account_rows_block`). The two properties to carry away from here:
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
    AccountLabel, MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL, account_key, split_account_key,
};
use vike_model::change_journal::Actor;
use vike_model::credential_keys;

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
/// the registry, and reachable from no form.
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
    let mut fields = credential_fields(venue, env_label);
    // ⚠ Appended ONLY to a cell that already has a form. An empty list is how a row says *there is
    // no such tier* ([`tier_row`] renders the ✎ from it), so growing one here would resurrect a
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
///   `crates/vike-connections/tests/editor_key_shapes.rs` exists to keep out of this table.
/// * **The SUFFIX is `vike_model::credential_keys::attribution_var_for`'s answer, never a
///   per-venue list here.** A `SignedBuilder` venue takes `_BUILDER_CODE` and the CEX mechanics
///   take `_BROKER_CODE` — the split the root `CLAUDE.md`'s attribution paragraph names venue by
///   venue, derived from the mechanic so a new mechanised venue is classified by adding no row.
///   (`attribution_code_from` accepts either spelling, so the choice is about which name an
///   operator is taught, not about what is read.) ⚠ It lives in `vike-model` rather than here for
///   a second reason that is not taste: `crates/vike-ops/tests/settings_registry.rs`'s
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
/// ⚠ The ACCOUNT LABEL is stripped first (`vike_model::account_keys::split_account_key`), because
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
fn credential_fields(venue: &str, env_label: &str) -> Vec<(&'static str, String)> {
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
        // ⚠ THREE fields per account, not two. `_SERVER` is the third name
        // `vike_dukascopy::config::dukascopy_env_var_names` composes and
        // `load_dukascopy_config_from` reads, and it was in the owner's real store while this
        // table could not reach it — the measured defect this arm was widened for. It is
        // OPTIONAL (that loader gates on login+password alone and leaves `server` blank) and it
        // is CONDITIONAL in a way no other field here is: see [`form_note`], which renders the
        // qualification beside the form rather than leaving the label to imply a control that
        // silently does nothing.
        "dukascopy" if env_label == "DEMO" => vec![
            ("Login (DEMO1)", "DUKASCOPY_DEMO1_LOGIN".to_string()),
            ("Password (DEMO1)", "DUKASCOPY_DEMO1_PASSWORD".to_string()),
            ("JNLP URL (DEMO1, optional)", "DUKASCOPY_DEMO1_SERVER".to_string()),
            ("Login (DEMO2)", "DUKASCOPY_DEMO2_LOGIN".to_string()),
            ("Password (DEMO2)", "DUKASCOPY_DEMO2_PASSWORD".to_string()),
            ("JNLP URL (DEMO2, optional)", "DUKASCOPY_DEMO2_SERVER".to_string()),
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
        // ⚠ SEVEN fields, not two, and the five that arrived late are the rest of what
        // `load_poly_tier` actually reads. The L2 trio (`_API_KEY`/`_SECRET`/`_PASSPHRASE`) is
        // derived at connect via EIP-712 when blank — which is why it is optional, NOT why it was
        // missing; an operator holding a CLOB key pair issued by Polymarket's own UI could not
        // enter it here at all. The relayer pair is the EIP-1271 proxy-wallet submit/cancel path
        // (`vike_polymarket::client`'s `submit_order_relayer`/`cancel_order_relayer`), likewise
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
    /// Has the PUBLIC half of [`Self::buffers`] been seeded from the store yet?
    ///
    /// ⚠ A latch and not a per-frame refresh, and that is the difference between an editable field
    /// and an unusable one: `render_edit_form` runs every frame, so re-seeding would overwrite each
    /// keystroke with the stored value and the field could never be changed. [`Self::open`] clears
    /// it, so every form gets exactly one seed. It says nothing about SECRETS — those are never
    /// seeded from anything, and the map this function is handed does not contain one.
    prefilled: bool,
    /// The last Save's verdict, and the tier row it belongs BESIDE. Non-secret — venue/env tier
    /// only, never a value. See [`Verdict`] for why it carries the cell rather than being a
    /// bare string.
    message: Option<Verdict>,
    /// The last ACCOUNT-ROW act's verdict, rendered in the strip beside the rows it is about.
    ///
    /// ⚠ A separate cell from [`EditState::message`] rather than a reuse of it, and the reason is
    /// [`Verdict`]'s own: that one carries the `(venue, tier)` CELL it belongs under, because a
    /// Save verdict rendered anywhere but beneath the row that was clicked was measured to be laid
    /// out below the window's floor and clipped. An account-ROW act belongs to no tier cell — its
    /// rows span venues — so it is rendered where it happens, in the strip.
    ///
    /// Non-secret by construction: it is either a verb plus an id, or `vike_secrets`' own account
    /// refusal, whose arms name ids, venues, tiers and credential key NAMES from statements with
    /// no `value` column in them.
    row_message: Option<String>,
    /// ⚠ **THE TYPED CONFIRM for a REMOVE, armed but never pre-filled** — `(row id, what the
    /// operator has typed)`.
    ///
    /// A DELETE is the one act on this panel the store cannot put back, and both of its siblings
    /// require the operator to type the row id for it (`vike-cli secrets account remove --confirm
    /// N`, and the node wire's `AccountRequest::confirm`). This is that ceremony in a GUI, in the
    /// shape `crates/vike-app-core/src/tool_views/backend_settings.rs`'s
    /// `SettingsEditState::can_save` already uses for a `policy.toml` write: the Remove button ARMS
    /// this cell and removes nothing, and the box starts EMPTY — pre-filling it from the id the
    /// panel already holds would reduce the ceremony to a click, which is precisely what the
    /// contract exists to prevent.
    ///
    /// Cleared by [`EditState::close`], so switching account or venue disarms it: the id typed
    /// against one selection must not stay armed against another.
    remove_confirm: Option<(i64, String)>,
    /// **The `account` rows, held across frames — because this panel is IMMEDIATE-MODE and the
    /// store is a FILE.**
    ///
    /// ⚠ [`account_rows_block`] used to call `vike_secrets::resolve_accounts_in` unconditionally in
    /// its render body, which runs once per frame while the panel is visible: a `backend_in` stat,
    /// a fresh `sqlite3_open`, a prepare and a full-table scan, **60–120 times a second on the GUI
    /// thread**. Every other datum on that panel arrives already resolved; this was the only store
    /// read in the render path. It also had a cost beyond this process — the store runs
    /// `journal_mode = DELETE` (no WAL; it verifies the engine's answer and refuses otherwise), so
    /// a reader holding SHARED is exactly what a concurrent `vike-cli secrets set` has to wait out
    /// through `BUSY_TIMEOUT`.
    ///
    /// `None` means *ask the store on the next frame*. It is invalidated by the acts that change
    /// the answer and by nothing else: a row write here, and [`EditState::close`] (a different
    /// selection may sit under a different settings directory). Deliberately NOT a timer — a
    /// panel that refreshes on a clock would disagree with the store for up to one tick for no
    /// reason anybody could name, and the set of writers is small and known.
    ///
    /// ⚠ It holds the **`Result`**, with the error stringified so it can live across frames — never
    /// a flattened `Accounts`. *A store that exists and will not open* and *a store with nothing to
    /// say* are different answers, and the render body renders them differently.
    account_rows: Option<Result<vike_secrets::Accounts, String>>,
}

/// **A Save's verdict, and the cell it belongs to.**
///
/// ⚠ **The cell is the whole point of this type.** The verdict used to be a bare
/// `(is_error, String)` rendered by [`connections_ui`] AFTER the rail/detail block — and the
/// two-column arm allocates the rail `ui.available_height()` outright, so the one channel that
/// says `save failed: …` was laid out below the window's own floor and clipped. A failed Save was
/// therefore indistinguishable from a dead button, which is exactly what was reported. Carrying
/// the (venue, tier) lets [`venue_detail`] draw the verdict UNDER THE ROW THAT WAS CLICKED,
/// beside the ✎ and where the form just was, which is the only place it can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Verdict {
    /// `true` ⇒ the save did not happen; rendered in [`ERROR_COLOR`].
    is_error: bool,
    /// The sentence. Venue and tier only — a value never reaches it.
    text: String,
    /// The venue whose row this verdict belongs under.
    venue: String,
    /// The tier (`SIM`/`DEMO`/`LIVE`) whose row this verdict belongs under.
    tier: String,
}

impl EditState {
    fn open(&mut self, venue: &str, env_label: &str) {
        let n = edit_fields(venue, env_label).len();
        self.buffers = vec![String::new(); n];
        self.target = Some((venue.to_string(), env_label.to_string()));
        self.prefilled = false;
        self.message = None;
    }

    fn close(&mut self) {
        self.target = None;
        self.buffers.clear();
        self.prefilled = false;
        // ⚠ …and the REMOVE confirm, for the same reason the buffers above are cleared: it was
        // typed against the selection that is going away, and a confirm that outlived its
        // selection would be an armed DELETE against a row the operator is no longer looking at.
        self.remove_confirm = None;
        // ⚠ …and the cached account rows: a different selection may sit under a different settings
        // directory, so a list resolved for the old one must never be rendered against the new.
        self.account_rows = None;
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

    /// Accept an operator-typed account label and SELECT it, or refuse it with the validator's own
    /// reason.
    ///
    /// ⚠⚠ **IT WAS CALLED `create_account` AND IT CREATES NOTHING.** That name was the defect this
    /// rename fixes: it parses a string and changes a SELECTION — no row appears, no key is
    /// written, and the strip renders a `(new)` chip for a label no store enumeration knows about.
    /// The account the GRID means comes into existence when its first `__LABEL` credential is
    /// saved; the account the STORE means is a ROW, and creating one of those is
    /// `vike-cli secrets account add` (`vike_secrets::AccountEdit::Create`). A method named
    /// `create_account` in front of neither is a name a reader has to disprove.
    ///
    /// ⚠ The ONLY repair applied is a `trim`, and the line between that and the repairs
    /// `AccountLabel::parse` deliberately refuses (it will not uppercase `alt`) is that whitespace
    /// is not part of ANY label's spelling — nobody is learning a wrong name from a stripped
    /// trailing space off a paste, whereas a silently uppercased label is a spelling that then
    /// does not match what they wrote in their policy file.
    fn select_typed_account(&mut self, text: &str) {
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

/// ⚠ **The rail is a BOUNDED column, not a fixed one — and the bounds are the approved design's
/// (`minmax(190px, 232px)`).**
///
/// It was `const RAIL_W: f32 = 168.0`, and 168 is below the design's own floor: a venue name plus
/// three dots with nothing left over, which is what the owner's "ui is disbalanced" read against a
/// live capture reported as *cramped*. [`rail_w`] measures the widest venue name in THIS grid and
/// clamps the column into this range, so the rail is as wide as its content needs and never wider
/// than the design allows — the same derive-don't-pin rule [`name_col_w`] already obeyed for the
/// narrow arm's chips, applied to the column that holds them.
const RAIL_MIN_W: f32 = 190.0;
const RAIL_MAX_W: f32 = 232.0;

/// The gutter between the rail column and the detail pane, and the band the hairline dividing them
/// is painted down the middle of.
///
/// ⚠ **The divider is PAINTED rather than `ui.separator()`, and that is a sizing fact rather than a
/// styling one.** `egui-0.36.1`'s `Separator::ui` takes `ui.available_size_before_wrap()` — for a
/// VERTICAL separator that is the whole remaining HEIGHT — so a separator between these two columns
/// claims every pixel the window has left, exactly as the rail's own
/// `egui::vec2(RAIL_W, ui.available_height())` used to. See [`connections_ui`] for what that cost.
const RAIL_GUTTER: f32 = 14.0;

/// ⚠⚠ **TOMBSTONE — `DETAIL_MAX_W` (620.0) and `PANEL_MAX_W` lived here, and they were the WRONG
/// HALF of the design.**
///
/// The approved grid is `grid-template-columns: minmax(190px, 232px) minmax(0, 1fr)`. The RAIL is
/// the bounded column — [`RAIL_MIN_W`]/[`RAIL_MAX_W`], which is right and stays. The DETAIL column
/// is `1fr`: it FILLS what is left. Those two constants capped the WHOLE panel, and
/// [`connections_ui`] applied the cap to both columns at once, so the pane the design leaves
/// unbounded was bounded at 620pt.
///
/// MEASURED on a live capture of the thin client against the the CI box daemon, window MAXIMIZED at
/// 2560x1600: the content sat in an ~866pt column hugging the top-left, every separator stopping
/// a third of the way across, with the foot strip — correctly pinned to the floor — a long way
/// below it. That is the SAME "ui is disbalanced" picture the measure was added to fix, produced
/// by the opposite mechanism: instead of content demanding a window it did not need, content
/// refused to use the window it was given.
///
/// ⚠ **The tension, and why filling is safe where a wrapping `Label` was not.** A tool window
/// auto-sizes to its content (`egui-0.36.1`'s `Window::show_dyn` builds its `Resize` with
/// `.with_stroke(false)` and then `resizable(false)`, so `Resize::end` reports
/// `size[d] = last_content_size[d]`), and `Resize::begin` feeds that straight back as
/// `desired_size = desired_size.max(last_content_size)`. A child that FILLS `available_width`
/// makes `last_content_size.x == desired_size.x`, which is a FIXED POINT of that line — the max
/// of a value with itself. A child whose min width EXCEEDS what it was given is the growing case,
/// and it is what a wrapping prose `Label` does when its wrap width is the arena. So prose keeps
/// its measure ([`NOTE_W`]) and the pane does not need one.
///
/// **VERIFIED, not assumed** — `crates/vike-connections/tests/connections_layout.rs`'s
/// `the_windows_sizing_loop_converges_with_a_filling_detail_pane` drives the REAL `egui::Resize`
/// with the flags `Window` gives it and runs the loop to a fixed point.
///
/// The measure a PROSE paragraph in this panel is set in — the store-unreadable banner, the account
/// strip's label rule, its empty-account note, its standing no-credential-deletion rule and its
/// account-row lines. ⚠ It used to carry a removal INSTRUCTION here too — the widest thing in the
/// panel, because it interpolated an ABSOLUTE STORE PATH into a wrapping label. That text is a
/// tombstone (`account_rows_block`); the measure it forced is not, and every paragraph that
/// replaced it is set in it.
///
/// ⚠ **This is the one measure that survived the tombstone above, and the distinction is the whole
/// rule: a measure belongs on WORDS, not on a PANE.** A sentence set across a 2300pt pane is the
/// text wall this redesign removed; a pane narrowed to a paragraph's width is the column in the
/// corner it replaced it with. Applied through [`note`], always as `min(available_width, NOTE_W)`,
/// so it can only ever make a line SHORTER than the room there is — it can never push text outside
/// the window.
const NOTE_W: f32 = 520.0;

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
///
/// ⚠ `name_w` is a PARAMETER rather than the `const NAME_W: f32 = 86.0` it used to be, for
/// [`name_col_w`]'s reason: 86pt is `hyperliquid` at 12pt monospace with about a point to spare,
/// so the rail was one roster slug away from painting a name over its own first dot. The caller
/// measures it once and hands the SAME value to this row and to the `Venue` header above it —
/// which is the one-constant rule [`DOT_W`] states, kept while the value stops being a constant.
fn rail_row(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    name_w: f32,
    selected: bool,
    account: &AccountLabel,
    health: &StoreHealth,
    pick: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = RAIL_ROW_GAP;
        // ⚠ A LEFT-ALIGNED fixed cell, not `add_sized` — that helper lays its widget out
        // centred-and-justified, which would centre each venue name in its column and leave a
        // rail of ragged first letters nobody can scan down.
        ui.allocate_ui_with_layout(
            egui::vec2(name_w, 16.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(name_w);
                let name = egui::RichText::new(&row.venue).monospace().size(RAIL_TEXT_PT);
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

/// The rail's row text size in the TWO-COLUMN shape, and the gutter between a venue name and its
/// first dot. Named for [`name_col_w`]'s reason: the column is MEASURED at this size and drawn at
/// it, and a column measured at one size and drawn at another is a column whose dots do not line
/// up.
const RAIL_TEXT_PT: f32 = 12.0;
const NAME_PAD: f32 = 8.0;

/// The narrow rail's chip text size, and the gap between a chip's own four items. Named because
/// [`name_col_w`] lays the venue names out at this size to MEASURE the name column, and a column
/// measured at one size and drawn at another is a column whose dots do not line up.
const CHIP_TEXT_PT: f32 = 11.0;
const CHIP_GAP: f32 = 3.0;
/// One chip's height — the dot cells', so the chip is exactly as tall as what it holds.
const CHIP_H: f32 = 16.0;

/// The name column of either rail: the widest venue name IN THIS GRID, laid out with the real
/// painter at the size that rail draws them at.
///
/// ⚠ **Measured rather than pinned, and derived from the ROWS rather than from
/// [`crate::status::VENUES`]**, for the two reasons this file keeps re-learning. A constant is a
/// per-venue fact written down: a roster gaining a longer slug would paint that name over its own
/// dots, silently, with every test still green. And reading the roster rather than the rows would
/// widen the column for a venue this grid is not showing.
///
/// ⚠ It takes `pt` because BOTH rails now come through it. It was `chip_name_w`, private to the
/// narrow arm, while the two-column arm carried a written-down `const NAME_W: f32 = 86.0` — and
/// that constant was the very fact this function exists to stop anybody writing down.
fn name_col_w(ui: &egui::Ui, rows: &[VenueCredStatus], pt: f32) -> f32 {
    rows.iter()
        .map(|r| {
            ui.painter()
                .layout_no_wrap(
                    r.venue.clone(),
                    egui::FontId::monospace(pt),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
}

/// The two-column rail's width: its measured name column plus its three dot cells, clamped into
/// the approved design's `minmax(190px, 232px)` — see [`RAIL_MIN_W`]/[`RAIL_MAX_W`].
///
/// Returns the COLUMN width and the NAME cell width, because the caller owes the same name width
/// to the header and to every row (the one-constant rule [`DOT_W`] states) and the column width to
/// the gutter the divider is painted in.
fn rail_w(ui: &egui::Ui, rows: &[VenueCredStatus]) -> (f32, f32) {
    let name = name_col_w(ui, rows, RAIL_TEXT_PT) + NAME_PAD;
    let dots = 3.0 * (DOT_W + RAIL_ROW_GAP);
    ((name + dots).clamp(RAIL_MIN_W, RAIL_MAX_W), name)
}

/// The item spacing inside one two-column rail row — set explicitly by [`rail_row`] and read by
/// [`rail_w`], so the width the column is bounded to is the width its rows actually occupy.
const RAIL_ROW_GAP: f32 = 6.0;

/// A PROSE paragraph, set in [`NOTE_W`] rather than across whatever width the window happens to
/// offer. See that constant for why a sentence may not be allowed to decide the window's width.
fn note(ui: &mut egui::Ui, text: egui::RichText) {
    let w = ui.available_width().min(NOTE_W);
    ui.allocate_ui_with_layout(
        egui::vec2(w, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.add(egui::Label::new(text));
        },
    );
}

/// The rail in the NARROW shape: one wrapped chip per venue, the name and its three dots on one
/// line. Same selection rule as [`rail_row`] (selected = a label), same dots, same hovers — only
/// the layout differs, so the accessibility tree a test reads is the same in both shapes.
///
/// ⚠⚠ **EACH CHIP IS A FIXED-SIZE CHILD WITH AN EXPLICIT NON-WRAPPING LAYOUT, and that spelling is
/// the whole of this function.** It used to be `ui.scope(…)`, which INHERITS the parent's layout —
/// and the parent here is `horizontal_wrapped`, whose layout carries `main_wrap: true`. So every
/// chip was itself a wrapping row, opened at the cursor with only the width left on that line
/// (`Ui::new_child`'s `max_rect.unwrap_or_else(|| self.available_rect_before_wrap())`), and once
/// little was left it wrapped its own four items onto four lines. The parent's wrapped row then
/// advanced by that height, leaving the next chip even less width — and it COMPOUNDS.
///
/// What that cost, measured in the shipped 560pt window: the rail grew to ~700pt, the detail pane
/// and every one of its `✎` buttons landed 450-530pt below the window's clip rect, and
/// `egui-0.36.1/src/hit_test.rs` drops a widget whose `interact_rect` (`clip_rect ∩ rect`) is
/// negative. The buttons were all present in the accessibility tree with their labels, so every
/// test stayed green while **clicking the edit control did literally nothing** — the owner-reported
/// defect this function's spelling caused, and the reason
/// `crates/vike-app-core/tests/connections_window.rs` now clicks a `✎` inside the real window
/// frame rather than at a harness width that only ever reaches the two-column arm.
///
/// `allocate_ui_with_layout` is the cure and not merely a workaround: it hands
/// `Ui::new_child` an explicit `max_rect` AND an explicit layout, so the chip cannot wrap inside
/// itself, while `Layout::next_frame`'s `main_wrap` arm still wraps the PARENT between chips —
/// which is what a wrapped chip row was supposed to mean.
fn rail_chips(
    ui: &mut egui::Ui,
    rows: &[VenueCredStatus],
    selected: &str,
    account: &AccountLabel,
    health: &StoreHealth,
    pick: &mut Option<String>,
) {
    let name_w = name_col_w(ui, rows, CHIP_TEXT_PT);
    let chip_w = name_w + 3.0 * (CHIP_GAP + DOT_W);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for row in rows {
            ui.allocate_ui_with_layout(
                egui::vec2(chip_w, CHIP_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = CHIP_GAP;
                    let name = egui::RichText::new(&row.venue).monospace().size(CHIP_TEXT_PT);
                    // A LEFT-ALIGNED fixed cell, the same idiom (and the same argument) as
                    // [`rail_row`]'s: the dots sit in columns across the chips rather than
                    // drifting with each venue name's own advance.
                    ui.allocate_ui_with_layout(
                        egui::vec2(name_w, CHIP_H),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(name_w);
                            if row.venue == selected {
                                ui.add(
                                    egui::Label::new(name.strong().color(CONFIGURED_COLOR))
                                        .selectable(false)
                                        .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            } else if ui
                                .add(
                                    egui::Button::new(name)
                                        .frame(false)
                                        .wrap_mode(egui::TextWrapMode::Extend),
                                )
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
                },
            );
        }
    });
}

/// The detail pane's second chip, when the venue has a feed producer at all. ⚠ A BUILD fact — see
/// [`venue_detail`], where the sentence this badge may not contradict is argued at the call.
const FEED_PRODUCER_BADGE: &str = "feed producer in this build";

/// A small outlined badge — the detail pane's `Venue` / [`FEED_PRODUCER_BADGE`] chips.
///
/// ⚠ **Deliberately left WRAPPING**, unlike [`rail_chips`]'s chips, and the difference is the
/// failure mode rather than the mechanism. `Frame::show` builds its content `Ui` through
/// `Ui::new_child` with no layout override, so a badge near the end of [`venue_detail`]'s wrapped
/// row does inherit `main_wrap` and can wrap its own text — but the worst that costs is a taller
/// row with every character still on screen, and it does not COMPOUND (the badges are two, not
/// fourteen). Forcing `TextWrapMode::Extend` here without also measuring and allocating the
/// badge's true width would trade that graceful degradation for silent clipping off the right
/// edge, which is the trade this pane has already been bitten by once.
fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).monospace().size(10.0).color(color));
        });
}

/// One `LABEL  value` cell of the detail pane's meta row. ⚠ Wrapping for the same reason
/// [`badge`] is — three cells, not fourteen, and a wrapped cell loses nothing.
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
///
/// ⚠ **[`credential_fields`], not [`edit_fields`], and the difference is not cosmetic.** The
/// venue-wide attribution tail is not part of any venue's key family: including it would make
/// binance's LIVE form four fields where the generic trio is three (so every mechanised venue would
/// read `bespoke`), and polymarket's `POLY_LIVE_*` family would share only `POLY` with
/// `POLYMARKET_BUILDER_CODE` — no `_` boundary — so [`common_key_prefix`] would fall back to
/// printing one whole key name where a prefix belongs.
#[must_use]
pub fn key_family(venue: &str) -> String {
    let keys: Vec<String> = TIERS
        .iter()
        .flat_map(|tier| credential_fields(venue, tier).into_iter().map(|(_, key)| key))
        .collect();
    if keys.is_empty() {
        return "no configurable tier".to_string();
    }
    let v = venue.to_uppercase();
    let generic = TIERS.iter().all(|tier| {
        let fields = credential_fields(venue, tier);
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
    readable: &HashMap<String, String>,
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
    // ⚠ **THE RULE THAT MAKES THE PANE A PANE.** Its twin is the one under the rail's
    // `Venue  S D L` header, and the symmetry is the point: a `Separator` in a top-down layout
    // takes `available_size_before_wrap().x`, so this spans the DETAIL COLUMN — which, since that
    // column became the design's `1fr`, is the whole width left over from the rail.
    //
    // That is not decoration. Every other widget in this pane is left-packed and narrow, so
    // without something that occupies the column the pane's `min_rect` is its longest key name
    // and the pane reads as a card floating in the corner of a wide window, however much room it
    // was actually handed. MEASURED on the live capture that reopened this: a maximized 2560pt
    // window in which every rule stopped a third of the way across.
    ui.separator();
    ui.add_space(2.0);

    for tier in TIERS {
        tier_row(ui, row, tier, health, state);
        // The inline editor expands UNDER the tier row it belongs to, so the heading, the fields
        // and the row that opened them are one block rather than a form at the foot of the pane.
        if state.target.as_ref().is_some_and(|(v, t)| v == &row.venue && t == tier) {
            render_edit_form(ui, state, creds, readable);
        }
        // ⚠ …and so does the VERDICT, which is the second half of the same argument. It used to be
        // rendered by [`connections_ui`] after the rail/detail block, where the two-column arm's
        // full-height rail put it below the window floor: `save failed: …` reached no pixel, and a
        // Save that did not happen looked exactly like a button that did nothing. Drawn here it is
        // in the operator's eye line, immediately under the row whose ✎ they clicked — on the
        // SUCCESS path too, where the form has closed and this is the only thing that says so.
        if let Some(v) = &state.message
            && v.venue == row.venue
            && v.tier == tier
        {
            let color = if v.is_error { ERROR_COLOR } else { CONFIGURED_COLOR };
            ui.label(egui::RichText::new(v.text.as_str()).monospace().size(11.0).color(color));
        }
        ui.add_space(2.0);
    }
}

/// **The sentence the form leads with, and the one thing it has to make unmistakable.**
///
/// ⚠ It is `pub` so a headless test can assert the RENDERED text against the same bytes rather
/// than a paraphrase of them — `crates/vike-connections/tests/connections_a11y.rs`.
///
/// The old wording ("Fields are masked and start empty. Leave a field blank to keep its current
/// value unchanged; the existing secret is never shown here.") was true and was still read as a
/// bug: the owner's report of this panel was *"editing Dukascopy credentials doesn't show them"*,
/// which is half a missing-field complaint and half this. A form that discards what you cannot
/// see is indistinguishable from a broken one unless it says, at the point of use, that the
/// blankness is the DESIGN and what blank then does.
///
/// So this leads with the refusal and names the consequence in the operator's own terms. It does
/// not weaken the masking, and it must not: rule 1 of this module's security contract is that a
/// stored plaintext is never read back into the UI, gated end to end by
/// `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor`.
pub const MASKED_FIELD_HINT: &str = "Nothing is shown here on purpose: a value \
     already in the store is never read back into the app, so every field is masked and starts \
     EMPTY even for a key that is already set. Type a new value to REPLACE that key; leave a \
     field blank to KEEP whatever is there. Each field's grey text is the exact key it writes.";

/// A per-cell qualification rendered under [`MASKED_FIELD_HINT`], or `None` for the venues that
/// need none.
///
/// ⚠ **This exists for one shape of dishonesty: a field the form offers that the MOUNT may not
/// honour.** Every other field here is unconditional — write it and the bridge reads it. Dukascopy's
/// `_SERVER` is not: `vike_dukascopy::exec`'s `DukascopyExec::spawn_with_program` keeps the stored
/// value ONLY when it ends in `.jnlp`, and otherwise warns and hands the sidecar its own
/// `DEFAULT_DEMO_JNLP` — and the value that variable "commonly holds" (that function's own comment)
/// is the web-platform LOGIN url, which is not a JNLP. So the honest answers were to hide the field
/// or to state the condition, and hiding it is what left the key unreachable in the first place.
///
/// ⚠ The second sentence is a RESIDUAL, not a caveat about typing: nothing exercises a stored
/// server end to end. `crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s `live_config`
/// force-blanks `cfg.server` before every login, so both demo logins are proven against the
/// built-in default JNLP and a stored one is proven against nothing. Saying so here is the same
/// rule the rest of this panel follows — report the fact, never let a control imply an effect
/// nobody has measured.
#[must_use]
pub fn form_note(venue: &str, env_label: &str) -> Option<&'static str> {
    match (venue, env_label) {
        ("dukascopy", "DEMO") => Some(
            "JNLP URL: only a value ending in `.jnlp` is used — anything else (the JForex \
             web-platform login page included) is ignored and the built-in demo JNLP is used \
             instead. No live smoke exercises a stored JNLP URL.",
        ),
        _ => None,
    }
}

/// Render the masked edit form for `state.target`, if any. No-op when nothing is open.
///
/// `creds` carries the resolved store path and the durable ledger — see [`CredentialWrite`] for why
/// BOTH are parameters rather than walks this function performs for itself.
fn render_edit_form(
    ui: &mut egui::Ui,
    state: &mut EditState,
    creds: CredentialWrite<'_>,
    readable: &HashMap<String, String>,
) {
    let Some((venue, env_label)) = state.target.clone() else { return };
    let account = state.account.clone();
    // ⚠ `account_fields`, not `edit_fields`: the KEY NAMES this form writes carry the account, and
    // for the DEFAULT account they are the identical `String`s the venue-wide form always wrote.
    let fields = account_fields(&venue, &env_label, &account);

    // ⚠ **THE READ-BACK, and the whole of it.** Exactly once per opened form, and only for the
    // fields [`key_sensitivity`] classifies [`Sensitivity::Public`]. `readable` is
    // `crate::status::AccountGrids::readable_values` — a map the grid FILTERED through that same
    // classifier when it was built, so a secret's plaintext is not merely skipped here, it was
    // never handed to this function. That is what keeps security rule 1 a structural property
    // rather than a careful one, and it is why the filter lives at the grid rather than here.
    if !state.prefilled {
        for (i, (_, key)) in fields.iter().enumerate() {
            if key_sensitivity(key) == Sensitivity::Public
                && let Some(current) = readable.get(key)
                && let Some(buf) = state.buffers.get_mut(i)
            {
                buf.clone_from(current);
            }
        }
        state.prefilled = true;
    }

    ui.separator();
    // The title names the account only when there IS one to name — `AccountLabel::Default` has no
    // text BY DESIGN (its keys carry no label), so rendering one would invent a spelling that
    // appears in no file, and the default account's heading is byte-identical to before.
    let heading = match account.text() {
        None => format!("Edit {venue} / {}", env_label.to_lowercase()),
        Some(l) => format!("Edit {venue} / {} · account {l}", env_label.to_lowercase()),
    };
    ui.label(egui::RichText::new(heading).monospace().strong().size(13.0));
    ui.label(egui::RichText::new(MASKED_FIELD_HINT).size(10.0).weak());
    if let Some(note) = form_note(&venue, &env_label) {
        ui.label(egui::RichText::new(note).size(10.0).weak());
    }

    // Two venues pack a field list that is really TWO groups, and a thin divider between them
    // makes that visually obvious. Dukascopy's DEMO form carries two independent demo accounts
    // (DEMO1, DEMO2); cTrader's carries this tier's OAuth grant and then the tier-less Spotware
    // app registration, which is shared by all three of its cells and is the one thing about that
    // form an operator has to notice. Both groups otherwise render through the exact same generic
    // loop below (no special-cased widgets), and every other venue gets no divider at all.
    //
    // ⚠ The index is the LENGTH OF THE FIRST GROUP, so it moves when that group does: dukascopy's
    // DEMO1 group grew a third field (`_SERVER`) and the divider had to move with it, or the break
    // lands inside DEMO1 and the panel claims DEMO1's JNLP url belongs to DEMO2.
    let group_break = match (venue.as_str(), env_label.as_str()) {
        ("dukascopy", "DEMO") => Some(3),
        ("ctrader", _) => Some(2),
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
            // ⚠ MASKED IFF SECRET. `password(true)` is what files the widget as
            // `Role::PasswordInput`, which is the state assistive tech and any tree-reading
            // automation are entitled to speak aloud — right for a key, wrong for a server name
            // the operator is here to READ. [`key_sensitivity`] is the rule, and it fails closed.
            ui.add(
                egui::TextEdit::singleline(buf)
                    .password(key_sensitivity(key) == Sensitivity::Secret)
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
                    if trimmed.is_empty() {
                        return None;
                    }
                    // ⚠ A PREFILLED public field the operator did not touch is not an edit. Without
                    // this, every Save of any form would rewrite every readable key it showed and
                    // journal it as a change, which would make the ledger's own record of *what
                    // was rotated* useless exactly when somebody is reading it to find out.
                    if readable.get(key).map(|v| v.trim()) == Some(trimmed) {
                        return None;
                    }
                    Some((key.clone(), trimmed.to_string()))
                })
                .collect();

            if updates.is_empty() {
                state.message = Some(Verdict {
                    is_error: true,
                    text: "nothing entered — no changes saved".to_string(),
                    venue: venue.clone(),
                    tier: env_label.clone(),
                });
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
                        state.message = Some(Verdict {
                            is_error: false,
                            text: msg,
                            venue: venue.clone(),
                            tier: env_label.clone(),
                        });
                        ui.ctx().request_repaint();
                    }
                    Err(e) => {
                        // io::Error's Display is an OS message about the path, never file content.
                        state.message = Some(Verdict {
                            is_error: true,
                            text: format!("save failed: {e}"),
                            venue: venue.clone(),
                            tier: env_label.clone(),
                        });
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
    // The instant to stamp an account-row edit's ledger line with. A PARAMETER for
    // `CredentialWrite::now_ms`' reason: `vike_model::change_journal` reads no clock, so the
    // instant travels from the composition root all the way down.
    now_ms: i64,
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
        // ⚠⚠ **THE LEGEND RIDES THIS ROW, right-aligned opposite the chips** — that is the approved
        // design's placement and the cure for a full-width legend band of its own. It is padded
        // rather than flexed: there is no flex spacer in egui, so [`legend_w`] measures the run
        // with the real painter and this pads the row's REMAINING width down to it.
        //
        // ⚠ The `gap` read is what makes the measurement exact rather than approximate — the run
        // is spaced by this row's own `item_spacing.x`, so the width is computed from the number
        // the row is actually using rather than from a second copy of it.
        //
        // ⚠ And when the row does NOT have the room — the 400pt arm — nothing is padded and the
        // run wraps onto the next line, which is what `horizontal_wrapped` does with any item that
        // does not fit. The one thing that may not happen is padding a row too narrow for the run
        // and pushing its tail off the right edge, which is why this is a comparison and not an
        // `add_space` on every frame. `LEGEND_FIT_SLACK` is the rounding margin: the run is laid
        // out glyph by glyph by the same painter that measured it, and a fraction of a point of
        // disagreement would wrap the LAST entry alone onto its own line.
        //
        // ⚠⚠ **`available_rect_before_wrap()`, NOT `ui.available_width()` — that one LIES inside a
        // wrapping row.** `egui-0.36.1`'s `Layout::available_size` has a `main_wrap` arm that
        // returns `vec2(region.max_rect.width(), region.cursor.height())` for a horizontal wrap:
        // the WHOLE row's width, not what is left of it. Padding by that overshoots by everything
        // already on the line, and MEASURED with it the run was pushed past the wrap point and
        // landed at x = 8 on a line of its own — the band this call exists to remove, arrived at
        // through the padding meant to prevent it. `available_rect_before_wrap` is cursor-derived
        // and is the remaining width.
        let gap = ui.spacing().item_spacing.x;
        let run = legend_w(ui, gap);
        let room = ui.available_rect_before_wrap().width();
        if room >= run + LEGEND_FIT_SLACK {
            ui.add_space(room - run - LEGEND_FIT_SLACK);
        }
        mark_legend_items(ui);
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
        note(ui, egui::RichText::new(rule).monospace().size(10.0).weak());
    }
    if cancel {
        state.add_label = None;
        state.label_error = None;
    } else if create {
        let text = state.add_label.clone().unwrap_or_default();
        state.select_typed_account(&text);
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
            note(ui, egui::RichText::new(empty).monospace().size(10.0).weak());
        }
        // ⚠⚠ **TOMBSTONE — an INSTRUCTION lived here, and on a migrated box it was WRONG.** It read
        // *"To REMOVE account {l}, delete its `__{l}` lines from {store} by hand"*, naming a
        // `secrets.env` that `docs/decisions/0054` stopped reading: those lines are `credential`
        // ROWS in a database now, and the file it pointed at loads nothing. The rule it was
        // protecting is intact and is enforced in the STORE instead of by having no button —
        // `vike_secrets::edit_account` refuses to delete a row that still owns credentials, naming
        // them by key NAME. [`account_rows_block`] carries the whole argument.
        //
        // ⚠ The SETTINGS DIRECTORY is derived from the store path the root already resolved
        // (`CredentialHome::resolve` is what joined the file name onto it), never from a fresh
        // walk: the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-blind, and a panel that showed
        // one project's rows while editing another's is the exact defect `CredentialWrite` exists
        // to close for the credential half.
        account_rows_block(ui, state, &vike_secrets::settings_dir_of_store(store), now_ms);
    }
    ui.separator();
}

/// **The `account` ROWS the settings database holds for the selected label, and the two acts a UI
/// may perform on one** — DEACTIVATE (reversible, the one it leads with) and REMOVE.
///
/// # ⚠ The two "accounts" on this screen are different objects, and this block is the seam
///
/// Everything above it reads the credential key-NAME grammar: [`AccountGrids::from_vars`] derives
/// an account from `{BASE}__{LABEL}` and knows nothing about the settings database. The STORE's
/// account is a ROW, with an `id`, a venue, a tier and a book. On a migrated box the two answer
/// differently — the grid shows `default` plus any `__LABEL` accounts, and is blind to dukascopy's
/// two unlabelled rows entirely — and pretending otherwise is how a panel comes to disagree with
/// the process it is attached to. So this block says which object it is about: it renders ROWS,
/// keyed by `id`, read through the same `vike_secrets::resolve_accounts_in` a mount reads.
///
/// # Why DEACTIVATE and REMOVE and not CREATE
///
/// A row needs a `(venue, tier)`, and this strip's selection is venue-INDEPENDENT — one label spans
/// every venue's grid. A Create button here would have to GUESS which venue and which tier the
/// operator meant, and a guessed pair is a row filed against the wrong book. The two acts below
/// need neither: they address a row that EXISTS, by its own id. Creating one is
/// `vike-cli secrets account add --venue V --tier T --label L`, and the note below names it.
///
/// # ⚠ What this replaced, and why the old text had to go
///
/// It was an INSTRUCTION — *"To REMOVE account {l}, delete its `__{l}` lines from {store} by
/// hand"* — and on a migrated box that instruction is WRONG: `secrets.env` is no longer read, and
/// those lines are `credential` ROWS in a database. It named a file nothing loads.
///
/// The rule it was protecting is intact, and is now enforced one layer down rather than by having
/// no button at all: `vike_secrets::edit_account` REFUSES to delete a row that still owns
/// credentials, naming them by KEY NAME, and the schema's foreign key refuses it again behind that.
/// **Nothing here deletes a credential**, and nothing here rewrites a store.
fn account_rows_block(
    ui: &mut egui::Ui,
    state: &mut EditState,
    settings_dir: &std::path::Path,
    now_ms: i64,
) {
    let Some(label) = state.account.text().map(str::to_string) else { return };

    // ⚠ **THE STANDING RULE, rendered BEFORE the store is read and on every path below** — the
    // `Unanswerable` arm returns early, the empty arm returns early, and a reader who lands on
    // either still has to be told what this panel will and will not do. It is a property of the
    // PANEL rather than of whether this box happens to have rows, which is why it is not inside
    // any of the arms.
    note(
        ui,
        egui::RichText::new(
            "⚠ nothing here deletes a credential — the store is your only copy of live venue keys, \
             and the one write this app performs is an in-place upsert of the named keys it was \
             asked to save. An account ROW can be removed, and that is a different object: the \
             store REFUSES to delete one while it still owns credentials, naming them by key name.",
        )
        .monospace()
        .size(10.0)
        .weak(),
    );

    // The rows, from the store that ANSWERS — the same `backend_in` probe the daemon's own
    // credential read asks, so this panel and that mount cannot disagree about which store they are
    // describing. A box with no database answers `Unanswerable` and gets the note below rather than
    // an empty list, which would read as *this label has no rows* about a store that cannot be
    // asked at all.
    //
    // ⚠ **Resolved ONCE and held in [`EditState::account_rows`], not once per frame.** This is an
    // immediate-mode body; the unconditional call that used to sit here opened the SQLite store
    // 60–120 times a second on the GUI thread, and held a SHARED lock a concurrent writer had to
    // wait out. That field's doc carries the measurement and the exact set of acts that invalidate
    // it.
    //
    // ⚠ The cache holds the RESULT, not the `Accounts`. Folding the `Err` arm into `Unanswerable`
    // would be one line shorter and would collapse *a store that exists and will not open* into
    // *a store with nothing to say* — the exact failure `vike_secrets::resolve_accounts_in`'s own
    // doc exists to prevent, and the two arms below answer differently on purpose. The error is
    // stringified only because it is held across frames.
    if state.account_rows.is_none() {
        state.account_rows =
            Some(vike_secrets::resolve_accounts_in(settings_dir).map_err(|e| e.to_string()));
    }
    let rows = match state.account_rows.clone().expect("just resolved above") {
        Ok(vike_secrets::Accounts::Known(rows)) => rows,
        Ok(vike_secrets::Accounts::Unanswerable(why)) => {
            note(
                ui,
                egui::RichText::new(format!(
                    "{why} — so there are no account ROWS to show for {label}. On this box an \
                     account IS its credential key names, which the grid below already renders."
                ))
                .monospace()
                .size(10.0)
                .weak(),
            );
            return;
        }
        Err(e) => {
            // LOUD, never blank: a store that EXISTS and will not open is a different answer from
            // one with nothing to say, and collapsing the two is the failure
            // `vike_secrets::resolve_accounts_in`'s own doc exists to prevent.
            note(
                ui,
                egui::RichText::new(format!(
                    "the settings database could not be read, so this panel cannot say which \
                     account rows exist: {e}"
                ))
                .monospace()
                .size(10.0)
                .color(ERROR_COLOR),
            );
            return;
        }
    };
    let mine: Vec<vike_secrets::Account> =
        rows.into_iter().filter(|a| a.label.as_deref() == Some(label.as_str())).collect();
    if mine.is_empty() {
        note(
            ui,
            egui::RichText::new(format!(
                "no account ROW in the settings database carries the label {label} yet. One is \
                 created by `vike-cli secrets account add --venue <venue> --tier <tier> --label \
                 {label}` — not from here, because a row belongs to ONE venue and ONE tier and this \
                 strip's selection names neither."
            ))
            .monospace()
            .size(10.0)
            .weak(),
        );
        return;
    }

    // The act this frame, if any: `(id, Remove?)`. Collected rather than performed inside the row
    // loop so the store is written once, after the layout, with `&mut state` free.
    let mut act: Option<(i64, AccountRowAct)> = None;
    for row in &mine {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "row {}  {}/{}  {}",
                    row.id,
                    row.venue,
                    row.tier,
                    if row.active { "active" } else { "INACTIVE" }
                ))
                .monospace()
                .size(11.0),
            );
            // DEACTIVATE leads, and REMOVE sits beside it rather than instead of it: every consumer
            // of the arming reader already treats `active = 0` exactly as it would treat a deleted
            // row, and the row survives as evidence. It is the REVERSIBLE act, so it performs on
            // one click; the irreversible one below does not.
            let word = if row.active { "Deactivate" } else { "Activate" };
            if ui.button(egui::RichText::new(word).size(11.0)).clicked() {
                act = Some((row.id, AccountRowAct::SetActive(!row.active)));
                state.remove_confirm = None;
            }
            // ⚠ **THE TYPED CONFIRM, and the button does not remove.** A DELETE is the one act on
            // this panel the store cannot put back, and the CLI (`--confirm N`) and the node wire
            // (`AccountRequest::confirm`) both require the operator to type the row id for it. A
            // GUI that deleted on one click would be the surface where the ceremony is cheapest to
            // skip, and it is the surface where a misclick is likeliest.
            //
            // The shape is `crates/vike-app-core/src/tool_views/backend_settings.rs`'s
            // `SettingsEditState::can_save`, which is this tree's precedent for a destructive act
            // behind a keyboard: the box is NEVER pre-filled, because pre-filling reduces the
            // ceremony to a click, *"which is precisely what the contract exists to prevent"*.
            match &mut state.remove_confirm {
                Some((armed, typed)) if *armed == row.id => {
                    ui.label(
                        egui::RichText::new(format!("type {} to remove:", row.id))
                            .monospace()
                            .size(10.0)
                            .color(ERROR_COLOR),
                    );
                    ui.add(
                        egui::TextEdit::singleline(typed)
                            .desired_width(56.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    let matches = typed.trim() == row.id.to_string();
                    if ui
                        .add_enabled(
                            matches,
                            egui::Button::new(egui::RichText::new("Confirm remove").size(11.0)),
                        )
                        .clicked()
                    {
                        act = Some((row.id, AccountRowAct::Remove));
                    }
                    if ui.button(egui::RichText::new("Cancel").size(11.0)).clicked() {
                        state.remove_confirm = None;
                    }
                }
                _ => {
                    if ui.button(egui::RichText::new("Remove").size(11.0)).clicked() {
                        // ARMS the confirm; it does not remove. The buffer starts EMPTY.
                        state.remove_confirm = Some((row.id, String::new()));
                    }
                }
            }
        });
    }
    note(
        ui,
        egui::RichText::new(
            "⚠ Deactivate is reversible and is the act to reach for: the row and its credential \
             keys stay, and every reader of the arming table already treats an inactive row exactly \
             as it treats a deleted one. Remove DELETES the row, and is REFUSED while it still owns \
             credentials. Remove ARMS a typed confirm rather than performing: the row id has to be \
             typed, exactly as `vike-cli secrets account remove --confirm N` requires it. ⚠ A \
             RUNNING backend notices neither until it restarts: its arming snapshot is read once, \
             at boot.",
        )
        .monospace()
        .size(10.0)
        .weak(),
    );

    if let Some((id, what)) = act {
        let edit = match what {
            AccountRowAct::Remove => vike_secrets::AccountEdit::Remove { id },
            AccountRowAct::SetActive(active) => vike_secrets::AccountEdit::SetActive { id, active },
        };
        // ⚠ Through `crate::env_write`'s journalled wrapper, never `vike_secrets::edit_account_in`
        // directly — so the write and its ledger record cannot drift apart at the call site, the
        // rule `save_credentials_journalled` already holds for the credential half.
        state.row_message = Some(
            match crate::env_write::edit_account_journalled(
                settings_dir,
                edit,
                vike_model::change_journal::Actor::Gui,
                now_ms,
            ) {
                Ok(done) if done.changed => format!("account row {id}: {}", done.verb),
                Ok(_) => format!("account row {id}: unchanged — nothing was written"),
                // ⚠ The store's OWN refusal, verbatim and SAFE to render: `vike_secrets`' account
                // arms name ids, venues, tiers and credential key NAMES, from statements with no
                // `value` column, and the one that could have echoed an operator-supplied token
                // deliberately does not.
                Err(e) => e.to_string(),
            },
        );
        // ⚠ The act that changes the answer is the act that drops the cache — including on the
        // REFUSED and unchanged branches. A refusal means the store said no, and the rows it said
        // no about are the rows to re-read: this panel must never render a list from before a write
        // it just attempted. Re-resolving costs one open on the NEXT frame, not on every frame.
        state.account_rows = None;
    }
    if let Some(msg) = &state.row_message {
        let color =
            if msg.contains("NOTHING WAS WRITTEN") { ERROR_COLOR } else { CONFIGURED_COLOR };
        note(ui, egui::RichText::new(msg.as_str()).monospace().size(10.0).color(color));
    }
}

/// Which act a row's buttons requested this frame. Two variants rather than a `bool`, so a call
/// site cannot read *remove* as *deactivate* — the two differ by whether the row survives.
#[derive(Clone, Copy)]
enum AccountRowAct {
    /// `account.active` — reversible.
    SetActive(bool),
    /// DELETE. Refused by the store while the row still owns credentials.
    Remove,
}

/// The rounding margin [`account_strip`] leaves when it right-aligns the legend run. See the
/// comment at that call for what a fraction of a point costs without it.
const LEGEND_FIT_SLACK: f32 = 4.0;

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

    // ⚠⚠ **THE PANEL TAKES THE WHOLE WIDTH IT IS GIVEN, AND IS ALLOCATED AT ZERO HEIGHT.** Those
    // are two different claims and the container exists for both.
    //
    // WIDTH — `ui.available_width()` outright, with NO ceiling. This read `.min(PANEL_MAX_W)` and
    // that was the wrong half of the design: the rail is the bounded column, the detail pane is
    // `1fr`. Capping here capped BOTH, which is what put an ~866pt column in the top-left corner
    // of a maximized 2560pt window. The tombstone above [`NOTE_W`] carries the measurement and the
    // fixed-point argument for why filling cannot run the window away.
    //
    // HEIGHT — `0.0`, which is a DESIRED size and not a bound: egui clips nothing by `max_rect`,
    // and `scope_dyn` advances the parent by the child's own `min_rect`. So the panel is laid out
    // at the height of what it draws, and — belt and braces — every `available_height()` inside it
    // reads ~0 rather than the window's, so a future greedy read cannot silently re-open the
    // vertical feedback loop this panel has already been bitten by once.
    //
    // …and it is a CONTAINER rather than a `set_max_*` on the caller's own `Ui`, for two reasons
    // that outlive the width change:
    //
    // * [`state_id`] is read from the CALLER's `ui`, above — so `shown_account`, which the binary
    //   asks on that same `Ui` one step earlier, still keys the same slot. The container moves the
    //   ids of the WIDGETS inside it and nothing else.
    // * `connections_body` draws the foot strip on the caller's `Ui` AFTER this call returns.
    //   Mutating that `Ui`'s max rect would follow the strip with it, and the strip spans the
    //   WINDOW — it is process-level state and not part of this panel.
    let measure = ui.available_width();
    let pick = ui
        .allocate_ui_with_layout(
            egui::vec2(measure, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| panel_body(ui, grids, live, health, creds, &mut state),
        )
        .inner;

    if let Some(venue) = pick {
        state.select_venue(&venue);
    }

    ui.data_mut(|d| d.insert_temp(state_id, state));
}

/// Everything [`connections_ui`] draws, inside the container it draws it in. Returns the venue the
/// operator picked this frame, if any — applied by the caller, after the panel has been laid out,
/// for the reason [`EditState::select_venue`] carries.
fn panel_body(
    ui: &mut egui::Ui,
    grids: &AccountGrids,
    live: &HashMap<String, ConnectionState>,
    health: &StoreHealth,
    creds: CredentialWrite<'_>,
    state: &mut EditState,
) -> Option<String> {
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
        // ⚠ [`note`], not `ui.label`: this one interpolates a PATH and an OS reason, so it is the
        // panel's longest sentence in the one state an operator most needs to read it in.
        note(
            ui,
            egui::RichText::new(format!(
                "⚠ the credential store could not be opened — nothing below was measured: {why}"
            ))
            .monospace()
            .size(11.0)
            .color(ERROR_COLOR),
        );
        ui.add_space(4.0);
    }

    // ⚠ The mark legend rides the account strip's own row now (`account_strip` → the
    // `mark_legend_items` call at the end of it), right-aligned opposite the chips. It used to be
    // a `mark_legend(ui)` call HERE — a full-width run plus a full-width sentence, two stacked
    // bands across the top of the panel. `mark_legend_items` carries what that cost.
    account_strip(ui, grids, state, creds.store, creds.now_ms);

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
        let (rail_col, name_w) = rail_w(ui, statuses);
        // ⚠⚠ **NOTHING IN THIS ROW MAY CLAIM `available_height()`, and two things used to.**
        //
        // A tool window AUTO-SIZES TO ITS CONTENT — `egui-0.36.1`'s `Window::show_dyn` builds its
        // `Resize` with `.with_stroke(false)` and then `resizable(false)`, so `Resize::end` reports
        // `size[d] = last_content_size[d]` on BOTH axes. A body that claims every pixel it is
        // offered therefore does not merely look greedy: it TELLS THE WINDOW to be that tall, the
        // window grows, the body claims the new height, and the only thing that stops the loop is
        // `show_window`'s `constrain_to` clamping at the arena edge.
        //
        // MEASURED on the owner's capture: a ~1250pt-tall window whose rail ended at y≈600 and
        // whose detail pane ended at y≈400, with the foot strip — correctly pinned to the window's
        // floor by `connections_body`, which is what a status bar is — stranded ~600pt below the
        // last thing it described. The emptiness was never the STRIP's: it was this rail asking
        // for a window it had nothing to put in.
        //
        // So the rail is allocated at its NATURAL height (`0.0` is a desired size, not a bound —
        // egui's `max_rect` clips nothing, and `scope_dyn` advances the parent by the child's own
        // `min_rect`), and the divider between the columns is PAINTED rather than drawn with
        // `ui.separator()`, which takes `available_size_before_wrap()` and would claim the same
        // height right back. See [`RAIL_GUTTER`].
        let row_resp = ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(rail_col, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = RAIL_ROW_GAP;
                        ui.allocate_ui_with_layout(
                            egui::vec2(name_w, 14.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(name_w);
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
                        rail_row(
                            ui,
                            s,
                            name_w,
                            s.venue == selected,
                            &state.account,
                            health,
                            &mut pick,
                        );
                    }
                    // The rail's own FOOTER — the design's "About these dots", inside the rail
                    // column where it wraps at ~190pt instead of across the whole panel.
                    ui.add_space(8.0);
                    ui.separator();
                    rail_footnote(ui);
                },
            );
            ui.add_space(RAIL_GUTTER);
            ui.vertical(|ui| {
                if let Some(s) = &row {
                    venue_detail(
                        ui,
                        s,
                        FeedFact::of(&s.venue, live),
                        health,
                        state,
                        creds,
                        grids.readable_values(),
                    );
                }
            });
        });
        // The divider, painted down the middle of the gutter across exactly what the row occupied.
        let rect = row_resp.response.rect;
        if rect.height() > 0.0 {
            let x = rect.left() + rail_col + RAIL_GUTTER * 0.5;
            ui.painter().vline(x, rect.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
        }
    } else {
        rail_chips(ui, statuses, &selected, &state.account, health, &mut pick);
        ui.separator();
        if let Some(s) = &row {
            venue_detail(
                ui,
                s,
                FeedFact::of(&s.venue, live),
                health,
                state,
                creds,
                grids.readable_values(),
            );
        }
    }

    if statuses.is_empty() {
        ui.label(
            egui::RichText::new("no venue rows to show").monospace().size(11.0).color(ABSENT_COLOR),
        );
    }

    // ⚠ The narrow arm has no rail COLUMN to foot, so the same sentence is a FOOTNOTE here, set in
    // [`NOTE_W`]. It is at the foot rather than under the chips deliberately: a paragraph between
    // the marks and the detail pane is the band this redesign removed, and the foot is reachable
    // now for the same reason the window no longer grows — nothing above it claims the height.
    if !two_column {
        ui.add_space(8.0);
        ui.separator();
        let w = ui.available_width().min(NOTE_W);
        ui.allocate_ui_with_layout(
            egui::vec2(w, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            rail_footnote,
        );
    }

    pick
}

/// The legend's own text size. Named because [`legend_w`] lays the entries out at it to MEASURE
/// the run, and [`mark_legend_items`] draws them at it — a run measured at one size and drawn at
/// another is a run that does not land where the measurement said it would.
const LEGEND_PT: f32 = 10.0;

/// **The mark legend, as data** — one row per mark: the glyph, the word, the colour, the hover.
///
/// A function rather than an inline array because the run is MEASURED before it is drawn (see
/// [`legend_w`]), and two copies of this table are two chances for the measurement to describe a
/// legend the panel does not render.
fn legend_entries() -> [(&'static str, &'static str, egui::Color32, &'static str); 5] {
    [
        (
            GLYPH_CONFIGURED,
            "configured",
            CONFIGURED_COLOR,
            "every key that tier's form writes is set in this store, for the account selected to \
             the left",
        ),
        (
            GLYPH_NOT_SET,
            "not set",
            ABSENT_COLOR,
            "the tier exists for this venue and no key for it is in the store",
        ),
        (
            GLYPH_NOT_CONFIGURABLE,
            "no such tier",
            ABSENT_COLOR.gamma_multiply(NOT_CONFIGURABLE_DIM),
            "this venue has no such tier — nothing to configure, which is why the mark is not a dot",
        ),
        (
            GLYPH_UNKNOWN,
            "not measured",
            CONNECTING_COLOR,
            "the credential store could not be opened — nothing about that cell was measured",
        ),
        (
            "\u{270E}",
            "edit",
            egui::Color32::GRAY,
            "opens a masked form for that tier; the fields start empty and an existing secret is \
             never shown",
        ),
    ]
}

/// One legend entry's rendered text. ⚠ `<glyph> = <word>`, and the `=` is load-bearing rather than
/// decorative — see [`mark_legend_items`], where the whole argument is.
fn legend_text(glyph: &str, word: &str) -> String {
    format!("{glyph} = {word}")
}

/// The width the whole legend run occupies, laid out with the real painter at [`LEGEND_PT`] and
/// spaced with the gap the row it is going into actually uses.
///
/// This is what lets the legend be RIGHT-ALIGNED opposite the account chips on one row without a
/// flex container: [`account_strip`] asks how wide the run is, and pads to it when the row has the
/// room. When it does not, nothing is padded and the run simply wraps onto the next line like any
/// other item in a `horizontal_wrapped` — which is the 400pt arm.
fn legend_w(ui: &egui::Ui, gap: f32) -> f32 {
    let entries = legend_entries();
    let text: f32 = entries
        .iter()
        .map(|(glyph, word, _, _)| {
            ui.painter()
                .layout_no_wrap(
                    legend_text(glyph, word),
                    egui::FontId::monospace(LEGEND_PT),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
        })
        .sum();
    text + gap * (entries.len() as f32 - 1.0)
}

/// **The compact one-line legend, drawn INLINE on the account-strip row.**
///
/// ⚠⚠ **This used to be a full-width TEXT WALL and that is what it was reported as.** It was two
/// stacked rows of its own under the strip: this run, and then a ~250-character SENTENCE as a
/// wrapping `Label`. In a window free to size to its content that sentence laid out on one line
/// and took the window with it — MEASURED on the owner's capture, a ~2000pt window around a
/// ~700pt panel, with the legend band running edge to edge across the top of it. The run is now
/// one line on a row that already exists, and the sentence is [`rail_footnote`], set in a column.
/// This file's module doc carries the sizing mechanism.
///
/// ⚠ **The run right-aligns against the ROW's right edge, which is now the window's**, since the
/// panel stopped capping its own width. That is the design's `space-between`: the account chips at
/// one end of the strip and the key at the other. It is not anchored to a measure — a legend
/// floating in the middle of a wide bar, ending where nothing else ends, is worse than one at the
/// edge.
///
/// ⚠ **`<glyph> = <word>`, and the `=` is load-bearing rather than decorative** — the approved
/// design spells these entries `● configured`, and that spelling may not be copied. The DETAIL
/// pane's own status cell is `<glyph> <word>` for the SAME two words (`tier_row`, over
/// `TierState::label`). Spelled identically, a legend entry and a cell CLAIMING to have measured
/// something would be the same accessibility node text, and
/// `crates/vike-connections/tests/connections_a11y.rs`'s
/// `an_unreadable_store_says_so_and_renders_no_measurement` — whose whole job is to catch a cell
/// that claims a measurement nothing took — could no longer tell them apart. Its `status_cells`
/// helper says the same thing from the other side. The separator is what keeps that gate exact
/// rather than approximate, and two characters at 10pt is what it costs.
///
/// ⚠ Adds plain `Label`s straight to the caller's row and opens NO container of its own. The row
/// is `horizontal_wrapped`, whose layout carries `main_wrap: true`, and a child `Ui` that inherits
/// it is the 48× vertical blow-up [`rail_chips`] documents.
fn mark_legend_items(ui: &mut egui::Ui) {
    for (glyph, word, color, hover) in legend_entries() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(legend_text(glyph, word))
                    .monospace()
                    .size(LEGEND_PT)
                    .color(color),
            )
            .selectable(false),
        )
        .on_hover_text(hover);
    }
}

/// The rail footer's heading and its sentence — the long half of the old legend, moved out of the
/// panel-wide band and into the rail COLUMN, which is where the approved design puts it.
const FOOTNOTE_TITLE: &str = "About these marks";
const FOOTNOTE: &str = "The three per venue are Sim · Demo · Live CREDENTIAL PRESENCE in this store — never whether a \
     venue is reachable or armed. That is the detail pane's own Status (live feed) row, from a \
     different producer.";

/// **The rail's own footer**, under the venue rows: the sentence that says what the marks are.
///
/// ⚠ **It is a node on the accessibility tree and may not become a hover.**
/// `crates/vike-connections/tests/connections_a11y.rs`'s
/// `the_feed_status_is_labelled_and_an_absent_producer_is_not_unknown` reads `CREDENTIAL PRESENCE`
/// off the tree, because the half of this panel that keeps two facts apart — credential presence
/// and live feed state — is the half a hover would hide.
///
/// ⚠ **It wraps at whatever column it is given**, which is the whole move: in the two-column arm
/// that is the rail's ~190pt and the sentence is seven short lines filling the rail's own foot; in
/// the narrow arm the caller sets it in [`NOTE_W`] at the FOOT of the panel. Neither is a
/// full-width band, and neither can decide how wide the window is.
fn rail_footnote(ui: &mut egui::Ui) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(FOOTNOTE_TITLE)
                .monospace()
                .size(LEGEND_PT)
                .strong()
                .color(ABSENT_COLOR),
        )
        .selectable(false),
    );
    ui.add(egui::Label::new(egui::RichText::new(FOOTNOTE).monospace().size(LEGEND_PT).weak()));
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

    /// Dukascopy's DEMO edit form must offer both DEMO1 and DEMO2 field groups, DEMO1 first, and
    /// each group must be the WHOLE of what `vike_dukascopy::config::dukascopy_env_var_names`
    /// composes for that account — login, password AND server.
    ///
    /// ⚠ **SIX fields, not four, and the two that arrived late are the reported defect.** The form
    /// offered only DEMO1 until the DEMO2 group landed, and then offered neither account's
    /// `_SERVER` — both of which were in the owner's real store (`vike-cli secrets list`) and
    /// therefore settable from nowhere in the app. The order is the loader's own per account, so a
    /// reader of this list and a reader of that function see the same three names in the same
    /// sequence.
    #[test]
    fn dukascopy_demo_edit_fields_cover_both_accounts() {
        let fields = edit_fields("dukascopy", "DEMO");
        let keys: Vec<&str> = fields.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "DUKASCOPY_DEMO1_LOGIN",
                "DUKASCOPY_DEMO1_PASSWORD",
                "DUKASCOPY_DEMO1_SERVER",
                "DUKASCOPY_DEMO2_LOGIN",
                "DUKASCOPY_DEMO2_PASSWORD",
                "DUKASCOPY_DEMO2_SERVER",
            ]
        );
    }

    /// **The `_SERVER` field says what it does, and the form says what it does NOT do.**
    ///
    /// Two halves, and the second is the one that matters: the mount honours a stored server only
    /// when it ends in `.jnlp` (`vike_dukascopy::exec`'s `DukascopyExec::spawn_with_program`), so
    /// a field labelled like an unconditional one would be a control that silently does nothing
    /// for the value that variable most often holds. [`form_note`] is where that is stated, and
    /// it is stated for THIS cell only — every other form renders no note at all.
    #[test]
    fn the_jnlp_field_is_labelled_and_its_condition_is_stated() {
        let fields = edit_fields("dukascopy", "DEMO");
        let jnlp: Vec<&str> = fields
            .iter()
            .filter(|(_, k)| k.ends_with("_SERVER"))
            .map(|(label, _)| *label)
            .collect();
        assert_eq!(jnlp, vec!["JNLP URL (DEMO1, optional)", "JNLP URL (DEMO2, optional)"]);

        let note = form_note("dukascopy", "DEMO").expect("the DEMO cell carries the condition");
        assert!(note.contains(".jnlp"), "the note must name the condition itself: {note}");
        assert!(
            note.contains("No live smoke"),
            "…and the residual, or the field implies a measured effect: {note}"
        );
        assert!(form_note("dukascopy", "LIVE").is_none(), "a tier with no form needs no note");
        assert!(form_note("binance", "LIVE").is_none(), "no other venue carries one");
    }

    /// ⚠ **Polymarket's own field list is asserted in
    /// `crates/vike-connections/tests/editor_key_shapes.rs`, not here, and it is FORCED** — the
    /// same rule the `key_family` comment below states. Its key names are COMPOSED by the arm
    /// (`format!("POLY_{env_label}_…")`), so no literal exists under `src/` for
    /// `crates/vike-ops/tests/settings_registry.rs` to harvest, and spelling one in this test
    /// region would demand a `vike_ops::settings::SETTINGS` row asserting a read this crate does
    /// not perform. What can be said without a literal is said here: only the first field is
    /// required, and every other one is marked.
    #[test]
    fn only_polymarkets_first_field_is_required() {
        let fields = edit_fields("polymarket", "LIVE");
        assert_eq!(
            fields.len(),
            8,
            "the L1 key, the funder address, the five late arrivals and the venue-wide builder code"
        );
        assert!(
            !fields[0].0.contains("optional"),
            "the L1 private key IS the venue's live gate: {:?}",
            fields[0].0
        );
        // ⚠ `optional`, not `(optional)`: the venue-wide tail's own mark is
        // `(venue-wide, optional)`, and a parenthesised match would have silently excluded it.
        assert!(
            fields.iter().skip(1).all(|(label, _)| label.contains("optional")),
            "every other name `load_poly_tier` reads is optional there: {:?}",
            fields.iter().map(|(l, _)| *l).collect::<Vec<_>>()
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
        // ⚠ The CREDENTIAL half, which is what "unaffected" was ever about — the venue-wide
        // attribution tail rides `edit_fields`' LIVE cell and is asserted by
        // `the_live_cell_carries_the_venue_wide_attribution_tail` below.
        let binance = credential_fields("binance", "LIVE");
        let keys: Vec<&str> = binance.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_SECRET", "BINANCE_LIVE_API_PASSPHRASE"]
        );

        let fxcm = edit_fields("fxcm", "DEMO");
        let keys: Vec<&str> = fxcm.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(keys, vec!["FXCM_DEMO_USER", "FXCM_DEMO_PASSWORD"]);
        // …and fxcm's LIVE cell grows nothing either: this venue has no attribution mechanic.
        assert_eq!(edit_fields("fxcm", "LIVE").len(), 2);
    }

    /// **The VENUE-WIDE attribution tail lands on LIVE, on the mechanised venues only.**
    ///
    /// ⚠ The key names are COMPOSED here rather than spelled: a `{VENUE}_BROKER_CODE` literal in a
    /// `src/` test region would make `crates/vike-ops/tests/settings_registry.rs` demand a
    /// `vike_ops::settings::SETTINGS` row asserting a read this crate does not perform — the rule
    /// the `key_family` comment below states. The two FEE knobs are literals because the table
    /// itself spells them and they carry `vike-connections` rows for that reason.
    #[test]
    fn the_live_cell_carries_the_venue_wide_attribution_tail() {
        use vike_model::attribution::attribution_for;
        for &venue in crate::status::VENUES {
            let tail: Vec<String> = edit_fields(venue, "LIVE")
                .into_iter()
                .map(|(_, k)| k)
                .filter(|k| is_venue_wide_key(k))
                .collect();
            if attribution_for(venue).is_none() || edit_fields(venue, "LIVE").is_empty() {
                assert!(tail.is_empty(), "{venue} has no mechanic and must grow no tail: {tail:?}");
                continue;
            }
            let want = credential_keys::attribution_var_for(venue).expect("a mechanised venue");
            assert_eq!(tail.first(), Some(&want), "{venue}");
            // …and it is on LIVE ONLY: one name, three cells, so a DEMO field would be an operator
            // changing what a REAL order is tagged with from a cell labelled demo.
            for tier in ["SIM", "DEMO"] {
                assert!(
                    edit_fields(venue, tier).iter().all(|(_, k)| !is_venue_wide_key(k)),
                    "{venue}/{tier} must carry no venue-wide field"
                );
            }
        }
        // The two fee knobs ride beside their venue's code, and nobody else's.
        let aster: Vec<String> = edit_fields("aster", "LIVE").into_iter().map(|(_, k)| k).collect();
        assert!(aster.contains(&"ASTER_BUILDER_FEE_RATE".to_string()), "{aster:?}");
        let hl: Vec<String> =
            edit_fields("hyperliquid", "LIVE").into_iter().map(|(_, k)| k).collect();
        assert!(hl.contains(&"HYPERLIQUID_BUILDER_FEE_TENTHS_BP".to_string()), "{hl:?}");
        assert!(
            edit_fields("binance", "LIVE").iter().all(|(_, k)| !k.contains("_BUILDER_FEE")),
            "a CEX venue takes no builder fee"
        );
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
        // The CREDENTIAL half; the venue-wide tail is `the_live_cell_carries_…`'s subject.
        let fields = credential_fields("aster", "LIVE");
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
            // The CREDENTIAL half; the venue-wide tail is `the_live_cell_carries_…`'s subject.
            let fields = credential_fields("hyperliquid", tier);
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

    /// `EditState::open` sizes its buffer vec to `edit_fields`'s length — with DEMO2 and both
    /// `_SERVER` fields added that must now be 6 empty buffers for dukascopy/DEMO, not 2 and not
    /// 4. Every buffer starts empty (rule: an existing secret's plaintext is never read back into
    /// the UI). ⚠ This is not a restatement of the field-list test: `render_edit_form` INDEXES
    /// `buffers` by field position and `expect`s the entry, so a buffer vec that lagged the table
    /// would panic the panel rather than render a short form.
    #[test]
    fn edit_state_open_sizes_buffers_for_dukascopy_demo() {
        let mut state = EditState::default();
        state.open("dukascopy", "DEMO");
        assert_eq!(state.buffers.len(), 6);
        assert!(state.buffers.iter().all(String::is_empty));
        assert_eq!(state.target, Some(("dukascopy".to_string(), "DEMO".to_string())));
    }
}
