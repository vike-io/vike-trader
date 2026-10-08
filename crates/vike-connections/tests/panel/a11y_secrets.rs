//! The sentinel suite: no stored SECRET reaches the tree; a readable NAME shows only in its form.

use std::collections::HashMap;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_connections::{AccountGrids, CredentialWrite, StoreHealth, connections_ui};
use vike_model::feed_status::ConnectionState;

use crate::support::{edit_buttons, nodes, test_proc, text_fields, tree_text};

// ------------------------------------------------------------------------------------------
// ⚠ THE PROPERTY THAT MATTERS MOST — and until now the one this suite could not have caught
// ------------------------------------------------------------------------------------------

/// A value no venue key, no tier token, no account label, no venue name and no scratch path
/// contains — so a hit is a LEAK and never a coincidence. Same discipline (and the same reason) as
/// `tests/credential_write_journal.rs`'s typed constants.
const SENTINEL: &str = "zqxjvw7413mfbphgnd8256wu";

/// The shortest run of [`SENTINEL`] this test refuses to find on screen. A pane that rendered every
/// character but the last would pass a bare `contains` check, and a truncated API secret is still
/// an API secret.
const MIN_LEAK_WINDOW: usize = 8;

/// **The SECOND sentinel, and the reason this suite needed one.**
///
/// ⚠ The panel used to mask every field and read none of them back, so "no stored value is on
/// screen" was the whole rule and [`SENTINEL`] alone could state it. It is no longer the whole
/// rule: `vike_connections::keys::key_sensitivity` splits the store into what AUTHENTICATES (a
/// key, a secret, a token — still never read back, still masked) and what NAMES something (a
/// server, an account number, an address, an attribution tag), and the second kind is prefilled
/// into an opened form on purpose, because a masked field that starts empty is how a server name
/// an operator set six months ago becomes unknowable from inside the app that wrote it.
///
/// So the security property is now a CLASSIFICATION and not an absence, and a test that planted
/// one sentinel could only ever assert the absence — it would pass just as well against a panel
/// that had stopped reading anything back at all, which is the failure the split was made to fix.
/// Two sentinels state both halves: this one must APPEAR (in an opened form, and nowhere else),
/// and [`SENTINEL`] must appear NOWHERE.
const PUBLIC_SENTINEL: &str = "kfbrmt5091zwycpldx4738qv";

/// The keys [`seeded_store`] plants [`PUBLIC_SENTINEL`] in — the readable half. Every other seeded
/// key gets [`SENTINEL`].
///
/// ⚠ Chosen from the store's own shapes rather than invented: a JForex account number and an
/// EIP-712 account address. (The JForex SERVER was the third until decision 0095's Task 7 made it a
/// venue setting: a credential row under that name now refuses startup, so a store holding one is
/// not a store this panel is shown.) `key_sensitivity` is what actually decides, and
/// `crates/vike-app-core/tests/credential_editor_completeness_gate.rs` pins its verdict per key —
/// this list is asserted AGAINST that function below, so the two cannot drift.
const PUBLIC_KEYS: [&str; 2] = ["DUKASCOPY_DEMO1_LOGIN", "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS"];

/// The store this suite's OTHER tests never write is a relative dead path; this one needs a REAL
/// file, because the property is "a value on disk does not reach the screen" and a store that does
/// not exist cannot prove it.
fn seeded_store() -> (vike_model::scratch::ScratchDir, HashMap<String, String>) {
    // The system temp directory is legitimate in a test — `crates/vike-ops/tests/hygiene/system_temp_gate.rs`
    // scopes itself to production code. `ScratchDir` is unique per process and self-deleting.
    let dir = vike_model::scratch::ScratchDir::create_in(
        &std::env::temp_dir(),
        "vike-connections-no-secret",
    )
    .expect("scratch root");

    // Every SHAPE this panel can render a key name for: the generic `{VENUE}_{TIER}_API_*` trio,
    // a bespoke FX pair, an EIP-712 private key, and one LABELLED account (whose label the panel
    // DOES render — which is exactly why the label and the value are different strings here).
    let keys = [
        "BINANCE_LIVE_API_KEY",
        "BINANCE_LIVE_API_SECRET",
        "BINANCE_LIVE_API_PASSPHRASE",
        "BINANCE_DEMO_API_KEY",
        "BINANCE_DEMO_API_SECRET",
        "DUKASCOPY_DEMO1_LOGIN",
        "DUKASCOPY_DEMO1_PASSWORD",
        "HYPERLIQUID_LIVE_PRIVATE_KEY",
        "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS",
        "BINANCE_LIVE_API_KEY__HEDGE",
        "BINANCE_LIVE_API_SECRET__HEDGE",
    ];
    // ⚠ WHICH sentinel a key gets is decided by `key_sensitivity` itself, not by [`PUBLIC_KEYS`]:
    // that list is then asserted equal to the classifier's own verdict, so a test claiming
    // "an account number is readable" cannot be true only because this file said so.
    let value_for = |k: &str| {
        if vike_connections::keys::key_sensitivity(k) == vike_connections::keys::Sensitivity::Public
        {
            PUBLIC_SENTINEL
        } else {
            SENTINEL
        }
    };
    assert_eq!(
        keys.iter().copied().filter(|k| value_for(k) == PUBLIC_SENTINEL).collect::<Vec<_>>(),
        PUBLIC_KEYS.to_vec(),
        "the readable half of the seeded store must be exactly what `key_sensitivity` says it is"
    );
    let vars: HashMap<String, String> =
        keys.iter().map(|k| ((*k).to_string(), value_for(k).to_string())).collect();

    // …and the same bytes on disk, at the path the panel is handed. Nothing in `connections_ui`
    // reads this file today; writing it is what makes the test able to FAIL if something ever
    // starts to — a SECRET read back into the edit buffers being the regression the module's own
    // security rule 1 forbids by name.
    let mut text = String::new();
    for k in keys {
        text.push_str(&format!("{k}={}\n", value_for(k)));
    }
    std::fs::write(dir.path().join("secrets.env"), text).expect("seed the store");
    // …and carried into the settings DATABASE, the only credential store since the credential FILE
    // store was removed (2026-10-07): the values now sit on disk in BOTH forms, so a panel that
    // started reading either one back would put a sentinel on screen.
    vike_secrets::migrate(
        dir.path().to_str(),
        vike_model::credential_keys::is_platform_key,
        &vike_bridge_core::credentials::classify_credential_name,
        vike_secrets::WhenNothingToCarry::CreateEmptyStore,
    )
    .expect("carry the seeded store into the database");
    (dir, vars)
}

fn assert_no_sentinel(h: &Harness<'_, ()>, stage: &str) {
    let text = tree_text(h);
    for start in 0..=SENTINEL.len() - MIN_LEAK_WINDOW {
        let window = &SENTINEL[start..start + MIN_LEAK_WINDOW];
        assert!(
            !text.contains(window),
            "{stage}: a {MIN_LEAK_WINDOW}-character run of a stored credential VALUE ({window}) \
             reached the accessibility tree:\n{text}"
        );
    }
}

/// ⚠ **NO STORED VALUE REACHES THE RAIL, THE DETAIL PANE OR AN OPENED EDITOR.**
///
/// This is the pane's first security rule — *"An existing secret's plaintext is NEVER read back
/// into the UI — edit fields always start empty"* — and until now nothing asserted it end to end:
/// every harness in this file seeded an all-`false` grid, so there was no value on the box for a
/// regression to render. A pane that pre-filled the edit buffers from the store, or put a value in
/// a heading, a chip or a cell, passed every test here.
///
/// Driven the way the binary drives it: a real `secrets.env` on disk, the grid derived from the
/// same map by `AccountGrids::from_vars`, and the panel walked through the four states that could
/// each leak differently — the rail, a selected venue's detail pane, an OPENED form, and a
/// LABELLED account's grid.
///
/// ⚠ **Save is never clicked** — the suite's rule, and here the store is a real file, so it matters
/// more than usual. The scratch directory is self-deleting either way.
#[test]
fn no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor() {
    let (dir, vars) = seeded_store();
    let store = dir.path().join("secrets.env");

    // The guard against a test that cannot fail for its stated reason: the value really is on disk
    // and really is what the grid was derived from.
    let on_disk = std::fs::read_to_string(&store).expect("read the seeded store");
    assert!(on_disk.contains(SENTINEL), "the store must actually hold the sentinel");
    let grids = AccountGrids::from_vars(&vars);
    assert!(
        grids.labels().any(|l| l.text() == Some("HEDGE")),
        "the labelled account must be enumerated, or half this test is inert"
    );
    assert!(
        grids.default_grid().iter().any(|r| r.venue == "binance" && r.live && r.demo),
        "binance's LIVE and DEMO must read as configured, or the grid saw nothing"
    );

    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { store: &store, journal: None, proc: test_proc(), now_ms: 0 };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
        }
    });
    h.run();
    assert_no_sentinel(&h, "the rail on first paint");

    // 1 — SELECT the venue whose keys are seeded, so the detail pane renders them. ⚠ The selected
    // row is a LABEL, not a button (the pane's `aria-current` idiom), so there is NO button to
    // click when binance is already the roster's first row — which it is today. Both shapes end on
    // binance, and the assertion below is what proves which pane is actually on screen rather than
    // the click count.
    {
        let picks = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("binance")
        });
        assert!(picks.len() <= 1, "a venue appears in the rail once: {}", picks.len());
        if let Some(p) = picks.first() {
            p.click();
        }
    }
    h.run();
    let text = tree_text(&h);
    assert!(
        text.contains("BINANCE_LIVE_API_KEY"),
        "the detail pane must be binance's, and it renders the key NAME: {text}"
    );
    assert_no_sentinel(&h, "binance's detail pane");

    // 2 — OPEN the LIVE form. Every SECRET buffer must start EMPTY; a read-back is the regression.
    // ⚠ binance's LIVE form is FOUR fields, not three: the generic trio plus the venue-wide
    // `BINANCE_BROKER_CODE`, which is public and readable. The store seeds no broker code, so it
    // starts empty here for a different reason — that it is not masked is the assertion.
    {
        let pencils = edit_buttons(&h);
        assert_eq!(pencils.len(), 3, "binance offers three editable tiers");
        pencils.last().expect("the LIVE row's edit button").click();
    }
    h.run();
    let fields = text_fields(&h);
    assert_eq!(fields.len(), 4, "the LIVE form is open: the API trio plus the broker code");
    for f in &fields {
        let a = f.accesskit_node();
        let key = a.placeholder().unwrap_or_default().to_string();
        let secret = vike_connections::keys::key_sensitivity(&key)
            == vike_connections::keys::Sensitivity::Secret;
        assert_eq!(
            a.role(),
            if secret { Role::PasswordInput } else { Role::TextInput },
            "{key}: a secret is masked and a name is not"
        );
        assert_eq!(
            a.value().unwrap_or_default(),
            "",
            "{key}: this store seeds no value for it, so the buffer must be empty — and for a \
             SECRET that holds whatever the store says, because blank means keep"
        );
    }
    drop(fields);
    assert_no_sentinel(&h, "binance's opened LIVE editor");

    // 3 — the LABELLED account's grid, reached through its chip.
    {
        let picks = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("HEDGE")
        });
        assert_eq!(picks.len(), 1, "the HEDGE chip is a button until it is selected");
        picks[0].click();
    }
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("HEDGE"), "the account LABEL is rendered — names are not values: {text}");
    assert_no_sentinel(&h, "the HEDGE account's grid");

    // …and Save was never reached: the seeded bytes are untouched.
    assert_eq!(
        std::fs::read_to_string(&store).expect("re-read the store"),
        on_disk,
        "this suite never writes the store"
    );
}

/// **THE OTHER HALF OF THE SAME RULE: a NAME is read back, and only inside an opened form.**
///
/// ⚠ [`no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor`] asserts an
/// ABSENCE, and an absence is satisfied by a panel that reads nothing back at all — which is the
/// state the owner reported as *"editing Dukascopy credentials doesn't show them"*. This test is
/// the claim that makes that one a classification rather than a blanket: the dukascopy DEMO form
/// PREFILLS the account number it already holds, renders it UNMASKED so it can actually be read,
/// and the password beside it is still empty and still a `Role::PasswordInput`. (The JNLP url was
/// prefilled beside it until decision 0095's Task 7 took the server off this form — it is the
/// `venue.dukascopy.demo.server` setting now, and the seeded store holds no `_SERVER` row.)
///
/// ⚠ **The scope is the FORM.** The rail and the detail pane render key NAMES and never values, so
/// the readable sentinel must be absent until the ✏ is clicked — asserted in that order, because
/// "it appears somewhere" would be satisfied by a panel painting the store across the rail.
#[test]
fn a_readable_field_is_prefilled_unmasked_inside_the_form_and_nowhere_else() {
    let (dir, vars) = seeded_store();
    let store = dir.path().join("secrets.env");
    let grids = AccountGrids::from_vars(&vars);
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { store: &store, journal: None, proc: test_proc(), now_ms: 0 };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 900.0)).build_ui(move |ui| {
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
        }
    });
    h.run();
    assert!(
        !tree_text(&h).contains(PUBLIC_SENTINEL),
        "a readable VALUE is not painted on the rail — the rail renders key names"
    );

    // Select dukascopy, then open its one editable tier.
    {
        let picks = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("dukascopy")
        });
        assert_eq!(picks.len(), 1, "dukascopy is in the rail exactly once");
        picks[0].click();
    }
    h.run();
    assert!(
        !tree_text(&h).contains(PUBLIC_SENTINEL),
        "…nor in the detail pane, which also renders only names"
    );
    assert_no_sentinel(&h, "dukascopy's detail pane");

    {
        let pencils = edit_buttons(&h);
        assert_eq!(pencils.len(), 1, "dukascopy offers one editable tier");
        pencils[0].click();
    }
    h.run();

    let fields = text_fields(&h);
    assert_eq!(fields.len(), 4, "both demo accounts, two fields each");
    // ⚠ The key each field writes is read from `edit_fields`, NOT from the widget's placeholder:
    // egui files `hint_text` as the accessibility `placeholder` and a PREFILLED field has a value
    // instead, so reading the key off the tree would answer `""` for exactly the fields this test
    // exists to check — and `key_sensitivity` fails closed, so every one of them would then be
    // demanded to be masked. Zipping against the table is what keeps the check pointed at the
    // field it means.
    let keys: Vec<String> = vike_connections::keys::edit_fields("dukascopy", "DEMO")
        .into_iter()
        .map(|(_, k)| k)
        .collect();
    assert_eq!(keys.len(), fields.len(), "the form renders one widget per table row");
    let mut prefilled = 0usize;
    for (f, key) in fields.iter().zip(&keys) {
        let a = f.accesskit_node();
        let value = a.value().unwrap_or_default().to_string();
        match vike_connections::keys::key_sensitivity(key) {
            vike_connections::keys::Sensitivity::Secret => {
                assert_eq!(a.role(), Role::PasswordInput, "{key} must stay masked");
                assert_eq!(value, "", "{key} is a SECRET and must never be read back");
            }
            vike_connections::keys::Sensitivity::Public => {
                assert_eq!(
                    a.role(),
                    Role::TextInput,
                    "{key} names something — masking it is what made it unreadable"
                );
                if PUBLIC_KEYS.contains(&key.as_str()) {
                    assert_eq!(value, PUBLIC_SENTINEL, "{key} must show what the store holds");
                    prefilled += 1;
                }
            }
        }
    }
    drop(fields);
    assert_eq!(prefilled, 1, "DEMO1's login is the one seeded readable field this form offers");

    // The SECRET sentinel is still nowhere, with the form open over the same store.
    assert_no_sentinel(&h, "dukascopy's opened DEMO editor");

    // ...and nothing was written.
    assert!(
        std::fs::read_to_string(&store).expect("re-read the store").contains(PUBLIC_SENTINEL),
        "the store is untouched"
    );
}
