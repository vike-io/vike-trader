//! ⚠ **These cover `vike_datahub_client::route`, which is no longer this crate's code, and
//! they stayed here because this is the only place they RUN.** The roster lane builds that
//! crate with DEFAULT features, which compile none of the module; the lanes that do enable
//! `hist-route` reach it as a DEPENDENCY, and cargo compiles no `#[cfg(test)]` module of a
//! dependency at all. There is no `-p vike-datahub-client --features hist-route` lane, so
//! moving these down with the code would have turned all six off while reading green. The
//! `backtest-datafusion-store` lane (`scripts/ci_feature_suite.sh`) executes them here.
// ⚠ Named at their home rather than through `super::`: `datahub_addr_for_bin` has no
// PRODUCTION caller in this file, so a module-level `use` of it would be an unused import in
// every non-test build and `-D warnings` would refuse the crate.
use vike_datahub_client::flag_vocab::store_flag_removed;
use vike_datahub_client::route::{datahub_addr_for_bin, history_route};

/// A bare bin's address is the `config.datahub_addr` ROW of the settings database its
/// `VIKE_SETTINGS_DIR` names — and a retired `VIKE_DATAHUB_ADDR` in the same map moves nothing
/// (decision 0111). The BLANK case is deliberately NOT special-cased there: it reaches
/// [`history_route`], which owns that filter for every caller, so a bin cannot come to disagree
/// with the daemon about what an empty address means.
#[test]
fn a_bin_reads_its_address_from_the_settings_row() {
    let settings = tempfile::tempdir().expect("a settings dir");
    let dir = settings.path().to_str().expect("utf-8 temp path").to_string();
    let retired = (concat!("VIKE", "_DATAHUB_ADDR").to_string(), "<host>:1".to_string());
    let vars = std::collections::HashMap::from([("VIKE_SETTINGS_DIR".to_string(), dir), retired]);
    assert_eq!(datahub_addr_for_bin(&vars), None, "no database is absent, not a default");

    vike_secrets::plant_settings_rows(
        settings.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_addr".to_string(),
                value: "\"<host>:7878\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("plant the config.datahub_addr row");
    assert_eq!(datahub_addr_for_bin(&vars).as_deref(), Some("<host>:7878"));
    assert_eq!(
        history_route(datahub_addr_for_bin(&vars).as_deref()).hub(),
        "<host>:7878",
        "and it feeds the SAME route decision the compute daemon uses"
    );
}

/// What every reader PRINTS and RECORDS: the datahub's ADDRESS. ⚠ It took a `local_root` and
/// named a path on the local arm until 2026-09-25; with that arm closed there is no path a
/// reader can honestly name, so the label cannot carry one by construction rather than by care.
#[test]
fn the_label_names_the_datahub_and_nothing_else() {
    let wire = history_route(Some("<host>:7878")).label();
    assert_eq!(wire, "the datahub at <host>:7878");
}

/// ⚠ The DEFAULT is the wire, and that is the assertion the whole of
/// `docs/decisions/0084-only-the-datahub-touches-the-store.md` turns on for this daemon: with
/// no flag it must not open files, whatever the environment says.
#[test]
fn no_flag_is_the_wire() {
    assert_eq!(history_route(Some("<host>:7878")).hub(), "<host>:7878");
}

/// ...and an UNCONFIGURED address is still the wire, at the default peer — never a silent
/// fallback to opening the files.
#[test]
fn an_unconfigured_address_is_still_the_wire() {
    assert_eq!(history_route(None).hub(), vike_config::DEFAULT_DATAHUB_ADDR);
    // A BLANK line is absent, not an address: dialling "" would be a connect nobody asked for.
    assert_eq!(history_route(Some("   ")).hub(), vike_config::DEFAULT_DATAHUB_ADDR);
}

/// ⚠ This test was `the_flag_is_the_only_way_local` until 2026-09-25, asserting that `--store`
/// on the line won over any configured peer. The owner closed that door, so what is worth
/// pinning now is the REFUSAL: that it names what changed and the one command that gives the
/// operator their local run back. A refusal that only said "no" would send them hunting.
#[test]
fn the_store_flag_is_refused_with_its_replacement_named() {
    let why = store_flag_removed("backtest");
    assert!(why.starts_with("backtest:"), "it names the verb that refused: {why}");
    assert!(why.contains("`--store DIR`"), "it names what was refused: {why}");
    assert!(
        why.contains("vike-backend datahub --store DIR"),
        "it names the ONE command that gives the local run back: {why}"
    );
    // By NUMBER, never by its `docs/` path: `docs/` is withheld from the public mirror, and
    // `vike-cli`'s published surface carries this sentence verbatim.
    assert!(why.contains("decision 0084"), "it cites the record that decided it: {why}");
    assert!(!why.contains("docs/"), "…without citing a tree the mirror withholds: {why}");
}
