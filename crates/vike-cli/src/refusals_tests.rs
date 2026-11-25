//! The three `--html` sentences that printed runs of spaces mid-sentence (a string continuation lost
//! its `\`), pinned exactly; the shape of that defect refused in every template; and every retired
//! `data`, `secrets`, `account` and `access` spelling shown with its id at its real refusing site.

use super::{FlagRefusal, FlagRefusalId};

#[test]
fn the_html_refusals_are_each_one_clean_sentence() {
    assert_eq!(
        FlagRefusal::HtmlOnlyOnShow { sub: "ls" }.to_string(),
        "--html does not apply to `backtest ls` — it renders ONE stored run's whole tearsheet as an \
         HTML page, which is `show`'s answer beside its --metrics: `backtest show <run> --html --out \
         sheet.html`"
    );
    assert_eq!(
        FlagRefusal::HtmlBesideASection { flag: "--trades" }.to_string(),
        "--html and --trades ask for two different documents: --html renders this run's whole \
         tearsheet as one HTML page, and --trades selects or exports part of it. Which one wins \
         would be a guess, so neither does — run `backtest show <run> --html --out sheet.html`, \
         then `backtest show <run> --trades`."
    );
    assert_eq!(
        FlagRefusal::HtmlBesideJson.to_string(),
        "--html and --json are two SERIALIZATIONS of the same run, and this verb writes one \
         document: --html is the rendered tearsheet a human opens, --json is the machine shape \
         (`{run_id, dir, manifest, report}`). Run it twice, or take the JSON and render it \
         elsewhere — every number in the HTML comes from the same report.json --json prints."
    );
}

/// A run of spaces inside a line is a continuation that lost its `\`. Only an indent after a
/// deliberate newline (`NothingToRun`'s example lines) may hold one.
#[test]
fn no_template_holds_a_run_of_spaces_mid_line() {
    for id in FlagRefusalId::ALL {
        for line in id.template().split('\n') {
            assert!(
                !line.trim_start().contains("  "),
                "{} has a run of spaces mid-line: {line:?}",
                id.word()
            );
        }
    }
}

/// Each retired `data` spelling, typed where it is refused, reads `error [<id>]: ...`: a site
/// that hands it on as the bare `.to_string()` prints no id. Every id has a line here, and every
/// `Class` site (one per verb that took `--class`) is driven.
#[test]
fn every_retired_data_spelling_is_refused_showing_its_id() {
    use super::RetiredDataSpellingId as Id;
    const LINES: [(Id, &str); 6] = [
        (Id::HistGroup, "hist ls"),
        (Id::SourceAsLane, "history rm --kind quote --venue d --symbol S --source recorder"),
        (Id::ImportLabel, "history import csv-bars F --venue d --symbol S --interval 1h --label x"),
        (Id::Class, "history ls --class"),
        (Id::Class, "catalog ls --venue binance --class CryptoPerp"),
        (Id::Class, "instruments add --venue ig X --tick 1 --quote USD --class fx"),
    ];
    for id in Id::ALL {
        assert!(LINES.iter().any(|(i, _)| *i == id), "{} has no line here", id.word());
    }
    for (id, line) in LINES {
        let argv: Vec<&str> = ["data"].into_iter().chain(line.split(' ')).collect();
        let said = crate::cmd::accepts(&argv).expect_err(line);
        assert!(said.starts_with(&format!("error [{}]: ", id.word())), "`data {line}`: {said}");
    }
}

/// **Every retired `secrets`, `account` and `access` spelling is refused SHOWING ITS ID**: one real command line per
/// variant through the real refusing path (`crate::cmd::accepts`), answered `vike-cli <plane>: error [<id>]: …`. A
/// variant with no line here fails.
#[test]
fn every_retired_spelling_is_refused_showing_its_id() {
    use super::RetiredSpellingId as Id;
    const LINES: [(Id, &str); 9] = [
        (Id::SecretsList, "secrets list"),
        (Id::SecretsDelete, "secrets delete KEY --confirm KEY"),
        (Id::SecretsAccounts, "secrets accounts"),
        (Id::SecretsAccount, "secrets account set-symbol --id 4 BTCUSDT"),
        (Id::SecretsSetBook, "secrets set-book --id 7 --venue-account-id 1"),
        (Id::SecretsConfirm, "secrets confirm --dry-run"),
        (Id::SecretsSetAccountFlag, "secrets set --account 7 API_KEY"),
        (Id::AccountRemove, "account remove --id 7 --confirm 7"),
        (Id::AccessList, "access list"),
    ];
    for id in Id::ALL {
        assert!(LINES.iter().any(|(i, _)| *i == id), "{} has no line here", id.word());
    }
    for (id, line) in LINES {
        let argv: Vec<&str> = line.split(' ').collect();
        let said = crate::cmd::accepts(&argv).expect_err(line);
        let shown = format!("vike-cli {}: error [{}]: ", argv[0], id.word());
        assert!(said.starts_with(&shown), "`{line}`: {said}");
    }
}
