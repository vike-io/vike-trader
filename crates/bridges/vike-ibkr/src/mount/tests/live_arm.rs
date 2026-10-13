//! The LIVE tier's arming rules: every rule holding arms it, and each refusal says which rule.

use super::*;
use vike_secrets::NoAccountTable;

// ── the LIVE tier ───────────────────────────────────────────────────────────────────────────────
//
// It arms only for a `live` account whose `ACCOUNT` is stored, whose `account` table holds an
// ACTIVE ibkr/live row for the asking account, whose live gateway rows load and whose backend is
// `socket` (or `cpapi`, held). Anything else is paper with a named cause; a `demo` account never
// reaches it.

/// **THE ARMING CASE**: a `live` account, its `ACCOUNT` stored and its row active, the gateway is
/// socket → `Armed` at the LIVE tier, and NOT held below live. Its mount binds `AccountTier::Live`,
/// announces the real-money warning once in the convention, connects on the LIVE tier's own gateway
/// rows, and — reconciliation ON — builds the SOCKET reconcile client on that same live config
/// (decision 0140: reconcile rides the exec's product, so the live tier gets one), never the cpapi
/// one.
#[test]
fn a_live_account_arms_the_live_tier_under_a_permitting_ceiling_with_an_active_row() {
    let mut fx = live_store(&AccountLabel::Default, vec![account_row(VENUE, "live", None, true)]);
    fx.settings = live_gateway(&[("port", "4101"), ("backend", "socket")]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Armed { tier: AccountTier::Live, held_below_live: None },
        "socket live is the supported path and is not held below live"
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let props =
        SymbolProperties { tick_size: 0.01, step_size: 1.0, min_qty: 1.0, ..Default::default() };
    let (out, events) = captured(|| {
        let mut req = fx.request(AccountTier::Live, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        mount_with(
            req,
            |cfg, symbol| {
                assert_eq!(cfg.tier, AccountTier::Live, "reconciles the LIVE tier's account");
                assert_eq!((cfg.port, cfg.backend), (4101, IbkrBackend::Socket));
                assert_eq!(symbol, "AAPL.SMART.USD");
                Some(Box::new(NoReports))
            },
            |_, _| panic!("a socket live mount never builds a cpapi reconcile client"),
            |cfg, _| {
                assert_eq!(cfg.tier, AccountTier::Live, "connects on the LIVE tier's config");
                assert_eq!((cfg.port, cfg.backend), (4101, IbkrBackend::Socket));
                assert_eq!(cfg.account, SECRET, "the live account's own `ACCOUNT`");
                Ok(Box::new(Inert))
            },
            |cfg, symbol| {
                assert_eq!(cfg.tier, AccountTier::Live);
                assert_eq!(symbol, "AAPL.SMART.USD");
                Some(props)
            },
        )
    });
    let ExecOutcome::Live(exec) = out.exec else { panic!("a connected live mount must be Live") };
    assert_eq!(exec.bound_tier, AccountTier::Live);
    assert_eq!(exec.grid, Some(props));
    assert!(out.recon.is_some(), "the live tier keeps the reconcile client it built");
    assert!(out.identity.is_none());
    let announced: Vec<_> = events.iter().filter(|e| e.field("tier") == Some("live")).collect();
    assert_eq!(announced.len(), 1, "exactly one live announcement: {events:?}");
    let e = announced[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert!(e.message.starts_with("⚠ REAL-MONEY: "), "the alert prefix: {}", e.message);
    assert_eq!((e.field("venue"), e.field("account")), (Some("ibkr"), Some("DEFAULT")), "{e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "the live account number must not reach a line: {events:?}"
    );
    assert!(found_tier_events(&events).is_empty(), "nothing is refused or unused: {events:?}");
}

/// A LABELLED account arms from its OWN fields and its OWN `account` row, and a row of the default
/// account does not stand in for it.
#[test]
fn a_labelled_live_account_needs_its_own_active_row() {
    let who = named("LIVEALT");
    let fx = live_store(&who, vec![account_row(VENUE, "live", Some("LIVEALT"), true)]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Armed { tier: AccountTier::Live, held_below_live: None }
    );
    let wrong = live_store(&who, vec![account_row(VENUE, "live", None, true)]);
    assert_eq!(
        IbkrVenueMount.resolve(&wrong.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "the DEFAULT account's active row is not the labelled account's"
    );
}

/// **The account's tier picks the arm, and a tier below `live` is the DEMO arm at the MOUNT too**:
/// every live rule holding — an active ibkr/live row for the label, live gateway rows that load — a
/// `demo` account's `ACCOUNT` connects on the DEMO tier's config (the demo default port, never the
/// live row's), binds `AccountTier::Demo`, and says nothing about a live tier.
#[test]
fn a_demo_account_mounts_the_demo_tier_whatever_the_live_rows_say() {
    let mut fx =
        live_store(&named("NOCEIL"), vec![account_row(VENUE, "live", Some("NOCEIL"), true)]);
    fx.settings = live_gateway(&[("port", "4101"), ("backend", "socket")]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx),
            |_, _| None,
            |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
            |cfg, _| {
                assert_eq!(cfg.tier, AccountTier::Demo, "connects on the DEMO tier's config");
                assert_eq!(cfg.port, 7497, "the live gateway row is another tier's");
                Ok(Box::new(Inert))
            },
            |_, _| None,
        )
    });
    let ExecOutcome::Live(exec) = out.exec else { panic!("a connected demo mount") };
    assert_eq!(exec.bound_tier, AccountTier::Demo);
    assert!(found_tier_events(&events).is_empty(), "no live tier to speak of: {events:?}");
    assert!(
        events.iter().all(|e| e.field("tier") != Some("live")),
        "no line may claim the live tier mounted: {events:?}"
    );
}

/// A DEACTIVATED live row (`account.active` is the off switch), a table with NO such row, and a store
/// that cannot say at all (never read; no `account` table) each keep real money off — and each says
/// which, because "present and unused" is an error, not an absence.
#[test]
fn a_live_account_that_is_not_provably_active_stays_paper_and_says_why() {
    let who = named("INACTV");
    let mut unread = live_store(&who, vec![]);
    unread.accounts = AccountDirectory::unread();
    let mut no_table = live_store(&who, vec![]);
    no_table.accounts = AccountDirectory::from_rows(
        Accounts::Unanswerable(NoAccountTable::NoStore { db: "vike.db".into() }),
        None,
    );
    let cases: [(MountFixture, &str); 5] = [
        (live_store(&who, vec![account_row(VENUE, "live", Some("INACTV"), false)]), "deactivated"),
        (live_store(&who, vec![]), "no LIVE ibkr row"),
        (
            // Rows of OTHER venues, tiers and labels are not this account's.
            live_store(
                &who,
                vec![
                    account_row("binance", "live", Some("INACTV"), true),
                    account_row(VENUE, "demo", Some("INACTV"), true),
                    account_row(VENUE, "live", Some("OTHER"), true),
                ],
            ),
            "no LIVE ibkr row",
        ),
        (unread, "read no settings store"),
        (no_table, "no `account` table"),
    ];
    for (fx, needle) in &cases {
        assert_eq!(
            IbkrVenueMount.resolve(&fx.inputs(AccountTier::Live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "{needle}"
        );
        says(&refused_mount_says(fx, AccountTier::Live), needle);
    }
}
