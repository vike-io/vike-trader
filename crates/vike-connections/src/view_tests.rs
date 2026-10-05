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
    for &venue in vike_model::VENUES {
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
/// composes for that account — the login pair.
///
/// ⚠ **FOUR fields.** The form carried six — each account's `_SERVER` too — until decision 0095's
/// Task 7 made the JForex server the demo tier's `venue.dukascopy.demo.server` SETTING: a
/// credential row under the old name is read by nothing and refuses the trading daemon's start,
/// so the form must not write one.
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
    assert!(
        !keys.iter().any(|k| k.ends_with("_SERVER")),
        "the JForex server is a venue setting, not a credential this form writes"
    );
}

/// **The DEMO cell says where the JForex server is set, and the condition on its value.**
///
/// The server is not a field here any more (decision 0095, Task 7), so [`form_note`] names the
/// `vike-cli config set` line that writes the setting, and carries over the one condition the
/// mount puts on the value: only a `.jnlp` URL is used (`vike_dukascopy::exec`'s
/// `DukascopyExec::spawn_with_program`). It is stated for THIS cell only.
#[test]
fn the_demo_cell_names_the_server_setting_and_its_condition() {
    let note = form_note("dukascopy", "DEMO").expect("the DEMO cell names the server setting");
    assert!(
        note.contains("vike-cli config set venue.dukascopy.demo.server"),
        "the note must name the verb and the setting: {note}"
    );
    assert!(note.contains(".jnlp"), "the note must name the condition itself: {note}");
    assert!(
        note.contains("No live smoke exercises a stored JNLP URL"),
        "the note keeps saying what is unproven about the value: {note}"
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
    use vike_model::venues::attribution::attribution_for;
    for &venue in vike_model::VENUES {
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
    let hl: Vec<String> = edit_fields("hyperliquid", "LIVE").into_iter().map(|(_, k)| k).collect();
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

/// `EditState::open` sizes its buffer vec to `edit_fields`'s length — 4 empty buffers for
/// dukascopy/DEMO (two accounts' login pairs; the two `_SERVER` fields left with decision 0095's
/// Task 7). Every buffer starts empty (rule: an existing secret's plaintext is never read back into
/// the UI). ⚠ This is not a restatement of the field-list test: `render_edit_form` INDEXES
/// `buffers` by field position and `expect`s the entry, so a buffer vec that lagged the table
/// would panic the panel rather than render a short form.
#[test]
fn edit_state_open_sizes_buffers_for_dukascopy_demo() {
    let mut state = EditState::default();
    state.open("dukascopy", "DEMO");
    assert_eq!(state.buffers.len(), 4);
    assert!(state.buffers.iter().all(String::is_empty));
    assert_eq!(state.target, Some(("dukascopy".to_string(), "DEMO".to_string())));
}
