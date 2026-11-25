use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// `--mcp` as the first argument hands the server every word after it, in order, and nothing else.
#[test]
fn the_first_argument_makes_the_desktop_the_server() {
    let flags: Option<Vec<String>> =
        mcp_flags(argv(&["vike-desktop", "--mcp", "--profile", "read-only"])).map(|f| f.collect());
    assert_eq!(flags, Some(argv(&["--profile", "read-only"])));
    let bare: Option<Vec<String>> =
        mcp_flags(argv(&["vike-desktop", "--mcp"])).map(|f| f.collect());
    assert_eq!(bare, Some(Vec::new()));
}

/// Anywhere else, spelled otherwise or with no argument at all, the word is not the mode: the window
/// opens, as it does on whatever a launcher appended.
#[test]
fn only_the_first_argument_is_the_mode() {
    let cases: [&[&str]; 4] = [
        &["vike-desktop"],
        &["vike-desktop", "--observe", "--mcp"],
        &["vike-desktop", "--mcp=1"],
        &[],
    ];
    for words in cases {
        assert!(mcp_flags(argv(words)).is_none(), "{words:?}");
    }
}
