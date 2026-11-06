//! Every news source the datahub polls has a bundled icon, or is listed here as having none.
//!
//! The News window (`crates/vike-desktop`) draws each headline's source as a small icon from
//! `crates/vike-desktop/assets/news-logos/<slug>.png`, compiled in through the `NEWS_LOGO_PNGS` table
//! in `crates/vike-desktop/src/app_setup.rs` (decision 0143 point 9; the README beside the files
//! says where they came from). `vike-desktop` is outside the derived CI roster, so nothing there
//! runs in CI; the table that decides which feeds exist is [`FEEDS`], so the check lives HERE,
//! where a PR that adds a feed is tested:
//!
//! 1. [`every_feed_has_an_icon_file_or_is_listed_as_having_none`] — a new row in `FEEDS` fails until
//!    its PNG is committed and its `NEWS_LOGO_PNGS` row is written (or the source is named in
//!    [`NO_ICON`], which is then drawn as a coloured initial);
//! 2. [`no_icon_file_is_orphaned`] — a file no feed names, or a non-PNG, fails.
//!
//! The files and the table are read at RUN time (never `include_str!`: a tree without
//! `crates/vike-desktop` must skip loudly, not fail to compile).

use std::path::PathBuf;

use vike_datahub::tool_feeds::news::FEEDS;

/// Sources with deliberately NO icon (drawn as a coloured initial). Empty: all sixteen have one.
/// A source named here must NOT have a file, so the list cannot rot into a second copy of the table.
const NO_ICON: &[&str] = &[];

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The file stem of a source name: lowercase, every run of non-alphanumerics one `-`
/// (`Investing.com FX` -> `investing-com-fx`).
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn desktop_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vike-desktop");
    if dir.join("assets/news-logos").is_dir() {
        Some(dir)
    } else {
        eprintln!(
            "SKIPPED: {} has no assets/news-logos (a tree without vike-desktop)",
            dir.display()
        );
        None
    }
}

#[test]
fn slug_spells_the_file_stem() {
    assert_eq!(slug("CoinDesk"), "coindesk");
    assert_eq!(slug("Bitcoin Magazine"), "bitcoin-magazine");
    assert_eq!(slug("Investing.com FX"), "investing-com-fx");
    assert_eq!(slug("Seeking Alpha"), "seeking-alpha");
}

#[test]
fn every_feed_has_an_icon_file_or_is_listed_as_having_none() {
    let Some(desktop) = desktop_dir() else { return };
    let table = std::fs::read_to_string(desktop.join("src/app_setup.rs"))
        .expect("read crates/vike-desktop/src/app_setup.rs");
    for (name, _, _) in FEEDS {
        let file = desktop.join(format!("assets/news-logos/{}.png", slug(name)));
        if NO_ICON.contains(&name) {
            assert!(
                !file.exists(),
                "{name} is in NO_ICON but {} exists: drop it from NO_ICON",
                file.display()
            );
            continue;
        }
        let bytes = std::fs::read(&file).unwrap_or_else(|e| {
            panic!(
                "news source {name:?} has no icon: commit {} (see assets/news-logos/README.md) or list it in NO_ICON ({e})",
                file.display()
            )
        });
        assert!(bytes.starts_with(&PNG_MAGIC), "{} is not a PNG", file.display());
        let row =
            format!("(\"{name}\", include_bytes!(\"../assets/news-logos/{}.png\")),", slug(name));
        assert!(
            table.contains(&row),
            "NEWS_LOGO_PNGS in crates/vike-desktop/src/app_setup.rs lacks the row {row}"
        );
    }
}

#[test]
fn no_icon_file_is_orphaned() {
    let Some(desktop) = desktop_dir() else { return };
    let wanted: Vec<String> = FEEDS
        .iter()
        .filter(|(n, _, _)| !NO_ICON.contains(n))
        .map(|(n, _, _)| format!("{}.png", slug(n)))
        .collect();
    for entry in std::fs::read_dir(desktop.join("assets/news-logos")).expect("read news-logos") {
        let name = entry.expect("dir entry").file_name().to_string_lossy().into_owned();
        if name == "README.md" {
            continue;
        }
        assert!(
            wanted.contains(&name),
            "assets/news-logos/{name} belongs to no feed in FEEDS: delete it"
        );
    }
}
