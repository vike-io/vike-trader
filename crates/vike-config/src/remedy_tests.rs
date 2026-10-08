use super::*;

#[test]
fn the_remedy_names_config_set_and_never_a_file_to_write_into() {
    let r = SettingsFile::Policy.write_remedy("deadman_timeout_ms");
    assert_eq!(
        r.write_line("60000"),
        "vike-cli config set policy.deadman_timeout_ms 60000",
        "the ONE write that lands in the one store"
    );
    assert_eq!(r.write_clause(), "run this");
    let inline = SettingsFile::Flags.write_remedy("reconcile_off").inline_write("true");
    assert_eq!(inline, "`vike-cli config set flags.reconcile_off true`");
    assert!(
        !inline.contains(".toml"),
        "the remedy may not name a FILE as somewhere to write: {inline}"
    );
    assert_eq!(r.holder(), "the settings database");
    assert_eq!(
        r.unset_clause("0"),
        "Run `vike-cli config set policy.deadman_timeout_ms 0`",
        "there is no `config unset`, so the way back is the same verb"
    );
    let absent = r.absent_clause();
    assert!(absent.contains("the settings database does not carry"), "{absent}");
}

/// Every file's section is the first segment `vike-cli config set` demands, so the rendered
/// command is one this box can actually run.
#[test]
fn every_settings_section_renders_a_dotted_key_config_set_would_accept() {
    for file in SettingsFile::ALL {
        let r = file.write_remedy("k");
        assert_eq!(r.dotted(), format!("{}.k", file.section()));
        assert_eq!(SettingsFile::parse(file.section()), Some(file));
        assert!(r.write_line("v").starts_with("vike-cli config set "));
    }
}

/// **No rendered command ever carries `--confirm`, for any key** — the deleted ceremony
/// (0086 point 7).
#[test]
fn no_rendered_command_ever_carries_a_confirm_flag() {
    for (file, leaf) in
        [(SettingsFile::Policy, "deadman_timeout_ms"), (SettingsFile::Flags, "venue_catalog_off")]
    {
        let r = file.write_remedy(leaf);
        for rendered in [r.write_line("60000"), r.inline_write("60000"), r.unset_clause("0")] {
            assert!(!rendered.contains("--confirm"), "{rendered}");
        }
    }
}
