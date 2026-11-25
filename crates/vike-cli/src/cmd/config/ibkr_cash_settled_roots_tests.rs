//! `config ibkr-cash-settled-roots` over a real store in a temp directory: nothing is written before the seed, the
//! seed creates the release's roots once, and a typed root is read upper-case or refused.

use super::*;

fn store() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("store");
    dir
}

fn roots(dir: &Path) -> Option<Vec<String>> {
    read_ibkr_cash_settled_roots(dir).expect("read")
}

#[test]
fn add_and_rm_refuse_before_the_seed_and_ls_says_why() {
    let dir = store();
    for refused in [add("ES", Some(dir.path())), rm("ES", Some(dir.path()))] {
        let e = refused.expect_err("no table");
        assert!(
            e.message.contains("seed") && e.message.contains("nothing was written"),
            "{}",
            e.message
        );
    }
    assert_eq!(roots(dir.path()), None, "no table was created");
    let shown = ls(Some(dir.path())).expect("ls").join("\n");
    assert!(shown.contains("first IBKR future admission"), "{shown}");
}

#[test]
fn the_seed_writes_the_release_roots_once_and_the_operators_edits_stay() {
    let dir = store();
    let first = seed(Some(dir.path())).expect("seed").join("\n");
    assert!(first.contains("ES, MES, NQ, MNQ, RTY, M2K, YM, MYM"), "{first}");
    assert_eq!(
        add(" m2k ", Some(dir.path())).expect("add").join(""),
        "M2K is a cash-settled root already; nothing was written"
    );
    assert!(rm("ym", Some(dir.path())).expect("rm")[0].contains("removed YM"));
    assert!(add("vx", Some(dir.path())).expect("add")[0].contains("added VX"));
    let again = seed(Some(dir.path())).expect("seed again").join("\n");
    assert!(again.contains("left as it is"), "{again}");
    let shown = ls(Some(dir.path())).expect("ls");
    assert_eq!(shown[0], "8 cash-settled root(s); an IBKR future on any other root is refused");
    assert_eq!(shown[1..].join(" "), "  ES   MES   NQ   MNQ   RTY   M2K   MYM   VX");
}

#[test]
fn a_root_that_is_not_letters_and_digits_is_a_usage_error_and_nothing_is_written() {
    let dir = store();
    seed(Some(dir.path())).expect("seed");
    for bad in ["", "  ", "E.S", "ES-Z6", "É"] {
        let e = add(bad, Some(dir.path())).expect_err("refused");
        assert_eq!(e.exit, crate::exit::Exit::Usage, "{bad:?}");
    }
    assert_eq!(roots(dir.path()).map(|r| r.len()), Some(8));
}

#[test]
fn no_store_is_refused_by_every_writer() {
    let dir = tempfile::tempdir().expect("temp");
    assert!(seed(Some(dir.path())).is_err());
    assert!(add("ES", Some(dir.path())).is_err());
    assert!(!vike_secrets::db_path_in(dir.path()).exists(), "nothing created the database");
    assert!(ls(None).is_err(), "no settings directory");
}

/// `list` and `remove` are retired with no alias: each refuses by its id, naming `ls` or `rm`, and the
/// new word parses.
#[test]
fn list_and_remove_refuse_naming_ls_and_rm() {
    use crate::cmd::config::{Sub, parse};
    use crate::refusals::RetiredConfigSpelling;
    let argv = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    for (was, now, refusal) in [
        ("list", "ls", RetiredConfigSpelling::IbkrCashSettledRootsList),
        ("remove", "rm", RetiredConfigSpelling::IbkrCashSettledRootsRemove),
    ] {
        let said = parse(argv(&["ibkr-cash-settled-roots", was, "ES"])).expect_err(was);
        assert!(said.contains(&format!("error [{}]", refusal.id().word())), "{}", &*said);
        let named = format!("is now `config ibkr-cash-settled-roots {now}`");
        assert!(said.contains(&named), "{}", &*said);
    }
    let listed = parse(argv(&["ibkr-cash-settled-roots", "ls"]));
    assert!(matches!(listed, Ok(Sub::IbkrCashSettledRoots { verb: RootsVerb::Ls })), "{listed:?}");
    let removed = parse(argv(&["ibkr-cash-settled-roots", "rm", "ES"]));
    assert!(
        matches!(removed, Ok(Sub::IbkrCashSettledRoots { verb: RootsVerb::Rm(_) })),
        "{removed:?}"
    );
}
