//! The paper book's HALT arming and its slippage, asserted on the CONSTRUCTED book.

use super::{holds_credentials, report_not_running, report_table_decision};
use crate::AdapterPolicy;
use crate::policy::DEFAULT_PAPER_SLIPPAGE;

/// Every paper book `make_engine` builds — a `paper`-tier account's simulator, the one place a
/// paper engine is still built (decision 0156) — arms the operator HALT sentinel.
///
/// A paper book is a MOUNT, so `touch <project>/settings/state/HALT` must reach it. Before
/// `paper_client`, eleven paper arms armed nothing: an all-paper daemon silently observed no HALT
/// file.
///
/// ⚠ Asserts on the CONSTRUCTED book, not the source text: a text gate cannot tell an armed
/// construction from an unarmed one. Drop `.with_halt_path(..)` from `paper_client` -> red.
#[test]
fn the_paper_fallback_every_venue_arm_uses_is_halt_armed() {
    let client = super::paper_client(
        "binance",
        "BTCUSDT",
        DEFAULT_PAPER_SLIPPAGE,
        vike_model::FeeSchedule::Free,
    );
    assert!(
        client.halt_path().is_some(),
        "a paper-tier account's book must arm the HALT sentinel — an operator rehearses the \
         kill switch on exactly this mount"
    );
}

/// **A box without the `config.paper_slippage` row builds the book it always built**: no policy at
/// all, and a policy that carries no row, both give the compiled default — `0.0002`, the literal the
/// mount hard-coded — and the book is still HALT-armed.
#[test]
fn a_paper_book_without_the_row_keeps_the_compiled_slippage() {
    assert_eq!(DEFAULT_PAPER_SLIPPAGE, 0.0002, "the figure the mount shipped with");
    let no_policy =
        super::paper_book_for("binance", "BTCUSDT", None, vike_model::FeeSchedule::Free);
    assert_eq!(no_policy.slippage, DEFAULT_PAPER_SLIPPAGE);

    let no_row = AdapterPolicy::default();
    assert_eq!(no_row.paper_slippage, None, "the default policy carries no row");
    let book =
        super::paper_book_for("binance", "BTCUSDT", Some(&no_row), vike_model::FeeSchedule::Free);
    assert_eq!(book.slippage, DEFAULT_PAPER_SLIPPAGE);
    assert!(book.halt_path().is_some(), "the default book is HALT-armed as before");
}

/// **The row reaches the book**: whatever the policy carries is the slippage the paper client
/// fills with, `0` (no slippage at all) and a large figure included, and the book is still
/// HALT-armed. Fees are another parameter: the schedule handed in is the schedule the book keeps.
#[test]
fn the_policy_row_is_the_slippage_the_paper_book_fills_with() {
    for row in [0.0, 0.0001, 0.001, 0.05] {
        let policy = AdapterPolicy { paper_slippage: Some(row), ..AdapterPolicy::default() };
        let book = super::paper_book_for(
            "binance",
            "BTCUSDT",
            Some(&policy),
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(book.slippage, row, "the row {row} is the book's slippage");
        assert!(book.halt_path().is_some(), "{row}: the HALT arming is not the row's to drop");
    }
}

/// **A paper start says, at `error`, that its book will refuse a symbol quoted in another fiat
/// currency** (the book has no rate source, `vike_paper::foreign_quote_refusal`), for the engine symbol
/// and for each declared leg; a dollar-quoted leg is not named.
#[tracing_test::traced_test]
#[test]
fn a_paper_start_on_a_foreign_quoted_symbol_says_so() {
    use vike_model::accounts::account_keys::AccountLabel;
    let legs = ["EURGBP".to_string(), "EURUSD".to_string()];
    let _ = super::paper_engine("oanda", "USDJPY", &AccountLabel::Default, &legs, None, None);
    assert!(logs_contain("USDJPY is quoted in JPY"));
    assert!(logs_contain("EURGBP is quoted in GBP"));
    assert!(!logs_contain("EURUSD is quoted"), "a dollar-quoted leg is not refused");
}

/// The other half: a start on a dollar-quoted or unnamed-quote symbol logs nothing of the kind.
#[tracing_test::traced_test]
#[test]
fn a_paper_start_on_a_dollar_quoted_symbol_says_nothing() {
    use vike_model::accounts::account_keys::AccountLabel;
    for (venue, symbol) in [("oanda", "EURUSD"), ("binance", "BTCUSDT"), ("alpaca", "AAPL")] {
        let _ = super::paper_engine(venue, symbol, &AccountLabel::Default, &[], None, None);
    }
    assert!(!logs_contain("paper-quote-currency"));
}

/// A directory of one `oanda` demo account row `id` 1 (`active` as asked), the key NAMES it owns and
/// an EMPTY credential book: the shape of the tradehub's directory, which holds no venue VALUE
/// (decision 0146's second reopener).
fn names_only_directory(active: bool, names: &[&str]) -> AdapterPolicy {
    let row = vike_secrets::Account {
        id: 1,
        venue: "oanda".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    };
    let keys = (!names.is_empty()).then(|| {
        std::collections::BTreeMap::from([(
            1,
            vike_secrets::AccountKeys {
                prefixes: vec![],
                names: names.iter().map(|n| n.to_string()).collect(),
            },
        )])
    });
    AdapterPolicy {
        accounts: vike_secrets::account_directory::AccountDirectory::from_rows(
            vike_secrets::Accounts::Known(vec![row]),
            keys,
        ),
        ..AdapterPolicy::default()
    }
}

/// **`holds_credentials` answers from the key NAMES when the directory holds no book**: the tradehub
/// reads no venue value, so its book is empty by design, and without the names the `ARMING:` line of
/// an account that does hold keys would drop from `warn` to `debug`. The book half still answers for
/// a process that holds values. Drop the `keys` arm from `holds_credentials` -> the first assertion
/// is red; drop the book arm -> the third is.
#[test]
fn an_account_holds_credentials_by_its_key_names_when_the_book_is_empty() {
    use vike_model::accounts::account_keys::AccountLabel;
    let label = AccountLabel::Default;
    let named = names_only_directory(false, &["oanda_demo_api_key"]);
    assert!(holds_credentials(Some(&named), "oanda", &label), "a key name is a held credential");
    assert!(!holds_credentials(Some(&named), "ig", &label), "another venue's rows are not it");
    assert!(
        !holds_credentials(Some(&names_only_directory(false, &[])), "oanda", &label),
        "an account that owns no key name holds nothing"
    );
    // The book half: a planted value with no key names (a process that holds values).
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    book.insert_account(1, "API_KEY", "v");
    let mut valued = names_only_directory(false, &[]);
    valued.accounts = valued.accounts.with_credentials(book);
    assert!(holds_credentials(Some(&valued), "oanda", &label));
    assert!(!holds_credentials(None, "oanda", &label), "no policy, no directory, no credentials");
}

/// The LEVEL of the `ARMING:` line follows it: `warn` ("It has credentials") for an inactive account
/// whose keys are named, `debug` ("no credentials either") for one that owns none — with the book
/// empty in both.
#[tracing_test::traced_test]
#[test]
fn the_arming_line_of_a_named_key_account_is_a_warn_with_an_empty_book() {
    use vike_model::accounts::account_keys::AccountLabel;
    let block = vike_config::ArmingBlock::AccountInactive;
    report_table_decision(
        "oanda",
        &AccountLabel::Default,
        block,
        Some(&names_only_directory(false, &["oanda_demo_api_key"])),
    );
    assert!(logs_contain("WARN"), "the line is a warn");
    assert!(logs_contain("It has credentials"));
    assert!(!logs_contain("no credentials either"));
}

/// The other half of the pair, in its own test so its capture holds only its own line.
#[tracing_test::traced_test]
#[test]
fn the_arming_line_of_a_keyless_account_stays_a_debug_with_an_empty_book() {
    use vike_model::accounts::account_keys::AccountLabel;
    report_table_decision(
        "oanda",
        &AccountLabel::Default,
        vike_config::ArmingBlock::AccountInactive,
        Some(&names_only_directory(false, &[])),
    );
    assert!(logs_contain("no credentials either"));
    assert!(!logs_contain("It has credentials"));
    assert!(!logs_contain("WARN"));
}

/// **Every cause only the network or the box could produce is an `error`**, the price path's included: the
/// `ARMING:` line of an account that does not run because the DATAHUB is away used to be a `warn` while the
/// `PRICE FEEDS UNAVAILABLE` line beside it was an `error`, and its two siblings (`gatewayhub unavailable`, `adapter
/// failed`) were `error`s. An ordinary refusal (no credentials) stays a `warn`.
#[tracing_test::traced_test]
#[test]
fn the_arming_line_of_every_network_or_box_cause_is_an_error() {
    use vike_config::ArmingBlock;
    use vike_model::accounts::account_keys::AccountLabel;
    let louder = [
        ArmingBlock::PreflightFailed,
        ArmingBlock::AdapterFailed,
        ArmingBlock::GatewayUnavailable,
        ArmingBlock::FeedsUnavailable,
    ];
    for block in louder.into_iter().chain([ArmingBlock::NoCredentials]) {
        report_not_running(
            &crate::NotRunning {
                venue: "binance".to_string(),
                account: AccountLabel::Default,
                tier: Some(vike_model::AccountTier::Demo),
                block,
            },
            None,
        );
    }
    logs_assert(|lines: &[&str]| {
        let level_of = |block: ArmingBlock| -> Result<&str, String> {
            let needle = format!("does NOT RUN — {}:", block.as_str());
            lines
                .iter()
                .find(|l| l.contains(&needle))
                .map(|l| if l.contains(" ERROR ") { "ERROR" } else { "other" })
                .ok_or_else(|| format!("no line for {}: {lines:?}", block.as_str()))
        };
        for block in louder {
            if level_of(block)? != "ERROR" {
                return Err(format!("{} is not logged at error", block.as_str()));
            }
        }
        if level_of(ArmingBlock::NoCredentials)? == "ERROR" {
            return Err("an ordinary refusal must stay a warn".to_string());
        }
        Ok(())
    });
}
