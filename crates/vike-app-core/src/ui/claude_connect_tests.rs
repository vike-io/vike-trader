//! Connect Claude's merge, write and lookup, over planted files in temp directories.

use super::*;

const EXE: &str = r"C:\Program Files\vike\vike-desktop.exe";

/// The entry's text as the splice writes it (`serde_json` sorts the keys).
const ENTRY: &str = r#"{"args":["--mcp"],"command":"C:\\Program Files\\vike\\vike-desktop.exe","env":{"VIKE_SETTINGS_DIR":"C:\\vike\\settings"}}"#;

fn entry() -> Value {
    entry_for(EXE, Some(r"C:\vike\settings"))
}

fn merged(text: &str) -> String {
    merge_entry(Some(text), &entry()).expect("merged").expect("changed")
}

#[test]
fn the_entry_starts_this_exe_with_the_mcp_flag_and_the_settings_folder() {
    assert_eq!(serde_json::to_string(&entry()).expect("json"), ENTRY);
    assert_eq!(entry_for(EXE, None), serde_json::json!({ "command": EXE, "args": ["--mcp"] }));
}

#[test]
fn a_missing_or_empty_file_becomes_the_one_entry() {
    let want = serde_json::json!({ "mcpServers": { "vike": entry() } });
    for text in [None, Some(""), Some("  \r\n")] {
        let out = merge_entry(text, &entry()).expect("merged").expect("changed");
        assert_eq!(serde_json::from_str::<Value>(&out).expect("json"), want, "{text:?}");
        assert!(out.ends_with("}\n"), "{out:?}");
    }
}

/// Every other key and server keeps its bytes: the order, the spacing, a `1.50`, a string holding
/// braces, quotes and backslashes.
#[test]
fn other_keys_and_servers_keep_every_byte() {
    let text = r#"{
  "globalShortcut": "Ctrl+Space",
  "mcpServers": {
    "filesystem": {"command": "npx", "args": ["-y", "C:\\Users\\u\\{odd}\"dir]"]}
  },
  "zeta": 1.50
}
"#;
    let want = format!(
        r#"{{
  "globalShortcut": "Ctrl+Space",
  "mcpServers": {{
    "filesystem": {{"command": "npx", "args": ["-y", "C:\\Users\\u\\{{odd}}\"dir]"]}},
    "vike": {ENTRY}
  }},
  "zeta": 1.50
}}
"#
    );
    assert_eq!(merged(text), want);
}

#[test]
fn an_existing_vike_entry_is_the_only_thing_replaced() {
    let text = r#"{"mcpServers": {"vike": {"command": "D:\\old\\vike-desktop.exe", "args": ["--mcp"]}, "other": {"command": "x"}}, "a": true}"#;
    let want =
        format!(r#"{{"mcpServers": {{"vike": {ENTRY}, "other": {{"command": "x"}}}}, "a": true}}"#);
    assert_eq!(merged(text), want);
}

#[test]
fn a_file_without_a_server_table_gets_one_at_its_end() {
    assert_eq!(
        merged(r#"{"a": 1}"#),
        format!("{{\"a\": 1,\n  \"mcpServers\": {{\"vike\": {ENTRY}}}}}")
    );
    assert_eq!(merged("{}"), format!("{{\n  \"mcpServers\": {{\"vike\": {ENTRY}}}\n}}"));
    assert_eq!(
        merged(r#"{"mcpServers": {}}"#),
        format!("{{\"mcpServers\": {{\n    \"vike\": {ENTRY}\n  }}}}")
    );
}

#[test]
fn a_byte_order_mark_is_kept() {
    assert_eq!(
        merged("\u{feff}{}"),
        format!("\u{feff}{{\n  \"mcpServers\": {{\"vike\": {ENTRY}}}\n}}")
    );
}

#[test]
fn a_file_that_already_holds_the_entry_is_left_alone() {
    let text = format!(r#"{{"theme": "dark", "mcpServers": {{"vike": {ENTRY}}}}}"#);
    assert_eq!(merge_entry(Some(&text), &entry()), Ok(None));
}

#[test]
fn a_file_that_is_not_a_json_object_is_refused() {
    assert!(matches!(merge_entry(Some("{\"a\": 1,}"), &entry()), Err(MergeRefusal::NotJson(_))));
    assert!(matches!(
        merge_entry(Some("// a comment\n{}"), &entry()),
        Err(MergeRefusal::NotJson(_))
    ));
    assert_eq!(merge_entry(Some("[]"), &entry()), Err(MergeRefusal::NotAnObject));
    assert_eq!(
        merge_entry(Some(r#"{"mcpServers": null}"#), &entry()),
        Err(MergeRefusal::ServersNotAnObject)
    );
}

/// Of two `mcpServers` keys a JSON reader keeps the last, and so does the merge.
#[test]
fn a_repeated_key_is_merged_where_a_reader_looks() {
    let text = r#"{"mcpServers": {"a": 1}, "mcpServers": {"b": 2}}"#;
    let want = format!(
        "{{\"mcpServers\": {{\"a\": 1}}, \"mcpServers\": {{\"b\": 2,\n    \"vike\": {ENTRY}}}}}"
    );
    assert_eq!(merged(text), want);
}

#[test]
fn connect_backs_the_old_file_up_beside_it_then_replaces_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE);
    let old = r#"{"mcpServers": {"other": {"command": "x"}}}"#;
    std::fs::write(&path, old).expect("plant");
    assert!(connect(&path, &entry(), "20261125-101500").expect("written"));
    let backup = dir.path().join("claude_desktop_config.json.vike-backup-20261125-101500");
    assert_eq!(std::fs::read_to_string(&backup).expect("the backup"), old);
    assert_eq!(std::fs::read_to_string(&path).expect("the file"), merged(old));
    // Again: nothing to write, so no second backup either.
    assert!(matches!(connect(&path, &entry(), "20261125-101600"), Ok(false)));
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("list")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [CONFIG_FILE, "claude_desktop_config.json.vike-backup-20261125-101500"]);
}

#[test]
fn connect_refuses_to_overwrite_a_backup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE);
    std::fs::write(&path, "{}").expect("plant");
    std::fs::write(backup_path(&path, "s"), "kept").expect("plant a backup");
    let why = connect(&path, &entry(), "s").expect_err("refused");
    assert!(
        matches!(&why, ClaudeConnectError::Write(p, _) if *p == backup_path(&path, "s")),
        "{why}"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("the file"), "{}");
    assert_eq!(std::fs::read_to_string(backup_path(&path, "s")).expect("the backup"), "kept");
}

#[test]
fn connect_leaves_an_invalid_file_untouched_and_makes_no_backup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE);
    std::fs::write(&path, "{ not json").expect("plant");
    let why = connect(&path, &entry(), "s").expect_err("refused");
    assert!(matches!(why, ClaudeConnectError::Refused(MergeRefusal::NotJson(_))), "{why}");
    assert_eq!(std::fs::read_to_string(&path).expect("the file"), "{ not json");
    assert_eq!(std::fs::read_dir(dir.path()).expect("list").count(), 1);
    let found = ClaudeDesktopFound::Config(path.clone());
    assert!(matches!(status_of(&found, &Ok(entry())), ClaudeStatus::Refused(p, _) if p == path));
}

#[test]
fn connect_creates_a_missing_file_and_its_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Roaming").join("Claude").join(CONFIG_FILE);
    assert!(connect(&path, &entry(), "s").expect("written"));
    let found = ClaudeDesktopFound::Config(path.clone());
    assert_eq!(status_of(&found, &Ok(entry())), ClaudeStatus::Connected);
    assert_eq!(std::fs::read_dir(path.parent().expect("dir")).expect("list").count(), 1);
}

/// Another program under `vike` is named; this exe with other settings is just not connected.
#[test]
fn another_copy_is_named_by_its_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_FILE);
    let found = ClaudeDesktopFound::Config(path.clone());
    std::fs::write(&path, r#"{"mcpServers": {"vike": {"command": "D:\\old.exe"}}}"#)
        .expect("plant");
    assert_eq!(status_of(&found, &Ok(entry())), ClaudeStatus::Elsewhere(r"D:\old.exe".into()));
    let other_settings =
        serde_json::to_string(&entry_for(EXE, Some("D:\\elsewhere"))).expect("json");
    std::fs::write(&path, format!(r#"{{"mcpServers": {{"vike": {other_settings}}}}}"#))
        .expect("plant");
    assert_eq!(status_of(&found, &Ok(entry())), ClaudeStatus::NotConnected);
    let unnamed = Err(ClaudeConnectError::NotUtf8(PathBuf::from("x")));
    assert_eq!(status_of(&found, &unnamed), ClaudeStatus::CannotName("x is not UTF-8".into()));
}

/// A planted profile: `%LOCALAPPDATA%`, `%APPDATA%` and `$HOME` under one temp directory.
struct Profile {
    dir: tempfile::TempDir,
}

impl Profile {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().expect("tempdir") }
    }
    fn local(&self) -> PathBuf {
        self.dir.path().join("Local")
    }
    fn roaming(&self) -> PathBuf {
        self.dir.path().join("Roaming")
    }
    fn vars(&self) -> HashMap<String, String> {
        HashMap::from([
            ("LOCALAPPDATA".to_string(), self.local().display().to_string()),
            ("APPDATA".to_string(), self.roaming().display().to_string()),
            ("HOME".to_string(), self.dir.path().display().to_string()),
        ])
    }
    fn mkdir(&self, dir: &Path) {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    fn package(&self) -> PathBuf {
        self.local().join("Packages").join("Claude_pzs8sxrjxfjjc")
    }
    fn package_copy(&self) -> PathBuf {
        self.package().join("LocalCache").join("Roaming").join("Claude").join(CONFIG_FILE)
    }
    fn real_config(&self) -> ClaudeDesktopFound {
        ClaudeDesktopFound::Config(self.roaming().join("Claude").join(CONFIG_FILE))
    }
    fn find(&self) -> ClaudeDesktopFound {
        find_claude_desktop(ClaudePlatform::Windows, &self.vars())
    }
}

#[test]
fn windows_without_claude_desktop_finds_nothing() {
    let p = Profile::new();
    p.mkdir(&p.local().join("Packages").join("Microsoft.Paint_8wekyb3d8bbwe"));
    assert_eq!(p.find(), ClaudeDesktopFound::NotFound);
    let unswept = HashMap::new();
    assert_eq!(
        find_claude_desktop(ClaudePlatform::Windows, &unswept),
        ClaudeDesktopFound::NotFound
    );
}

#[test]
fn the_classic_install_is_found_by_its_program_or_its_data_folder() {
    let p = Profile::new();
    p.mkdir(&p.local().join("AnthropicClaude"));
    assert_eq!(p.find(), p.real_config());
    let q = Profile::new();
    q.mkdir(&q.roaming().join("Claude"));
    assert_eq!(q.find(), q.real_config());
}

/// The Store package reads the real `%APPDATA%` file until it holds a copy of its own (Claude 6.x
/// keeps none), so a package with no copy is written in the real folder.
#[test]
fn a_store_install_writes_its_own_copy_only_when_that_file_exists() {
    let p = Profile::new();
    p.mkdir(&p.package());
    assert_eq!(p.find(), p.real_config());
    p.mkdir(p.package_copy().parent().expect("dir"));
    assert_eq!(p.find(), p.real_config());
    std::fs::write(p.package_copy(), "{}").expect("plant");
    assert_eq!(p.find(), ClaudeDesktopFound::Config(p.package_copy()));
}

#[test]
fn macos_is_found_in_application_support_and_other_systems_are_unsupported() {
    let p = Profile::new();
    let dir = p.dir.path().join("Library").join("Application Support").join("Claude");
    p.mkdir(&dir);
    assert_eq!(
        find_claude_desktop(ClaudePlatform::MacOs, &p.vars()),
        ClaudeDesktopFound::Config(dir.join(CONFIG_FILE))
    );
    assert_eq!(
        find_claude_desktop(ClaudePlatform::Other, &p.vars()),
        ClaudeDesktopFound::Unsupported
    );
    assert_eq!(claude_desktop_pids(ClaudePlatform::Other, &p.vars()), None);
}

/// `tasklist /APPS /FO CSV /NH` as measured on the owner's PC (2026-11-25): the package's processes,
/// another package's, and nothing else.
#[test]
fn the_running_store_claude_is_read_from_the_package_column() {
    let csv = "\r\n\"claude.exe (Claude)\",\"50976\",\"268304 K\",\"Claude_6.28899.6.0_x64__pzs8sxrjxfjjc\"\r\n\
        \"RuntimeBroker.exe (runtimebroker07f4)\",\"16180\",\"15584 K\",\"Claude_6.28899.6.0_x64__pzs8sxrjxfjjc\"\r\n\
        \"claude.exe (Claude)\",\"49308\",\"37684 K\",\"Claude_6.28899.6.0_x64__pzs8sxrjxfjjc\"\r\n\
        \"Calculator.exe (App)\",\"777\",\"1 K\",\"Microsoft.WindowsCalculator_8wekyb3d8bbwe\"\r\n";
    assert_eq!(packaged_claude_pids(csv), [50976, 49308]);
    assert_eq!(packaged_claude_pids(""), Vec::<u32>::new());
}

#[test]
fn the_line_after_the_write_names_a_restart_only_while_claude_runs() {
    assert_eq!(next_step(Some(&[50976][..])), RESTART_CLAUDE);
    assert_eq!(next_step(Some(&[][..])), NEXT_START);
    assert_eq!(next_step(None), MAYBE_RESTART);
}

/// The click, end to end over a planted Store profile: the real file is written, and the next look
/// says connected.
#[test]
fn the_click_writes_the_file_and_the_view_then_says_connected() {
    let p = Profile::new();
    p.mkdir(&p.package());
    let settings = p.dir.path().join("settings");
    let mut view = ClaudeConnectView::on(ClaudePlatform::Windows);
    assert_eq!(view.status(&p.vars(), Some(&settings)), ClaudeStatus::NotConnected);
    view.connect_now(&p.vars(), Some(&settings));
    assert!(matches!(view.done, Some(Ok(_))), "{:?}", view.done);
    assert_eq!(view.status(&p.vars(), Some(&settings)), ClaudeStatus::Connected);
    let entry = this_entry(Some(&settings)).expect("this entry");
    assert_eq!(status_of(&p.real_config(), &Ok(entry)), ClaudeStatus::Connected);
}
