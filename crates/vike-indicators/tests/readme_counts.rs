//! The README opens with numbers (how many indicators, how many candlestick patterns, how many
//! per category). They are claims about [`registry`] and [`pair_registry`], and a claim nobody
//! checks goes stale the first time a row is added. This test reads `README.md` at run time and
//! holds every one of those numbers against the code, so adding an indicator without editing the
//! README fails here and the message says which number to change.
//!
//! The README's machine-readable shape is deliberately small and strict:
//!
//! * one line `**N indicators** · P candlestick patterns · C categories`;
//! * one table headed `| Category | Indicators |`, a row per category as `Category::label` spells
//!   it, plus one `Pair (benchmark)` row for the pair seam, whose counts add up to `N`.
//!
//! `Category::User` is a variant no built-in row ever carries, so it has no table row; this test
//! asserts that, rather than assuming it.

use vike_indicators::{Category, pair_registry, registry};

/// The table row that counts [`pair_registry`] (pair indicators are not part of [`Category`]).
const PAIR_ROW: &str = "Pair (benchmark)";

/// The table's first cell of its header row; the table is the run of `|` lines that starts here.
const TABLE_HEADER: &str = "| Category | Indicators |";

struct Readme {
    total: usize,
    patterns: usize,
    categories: usize,
    rows: Vec<(String, usize)>,
}

/// The number in front of `suffix` in one `·`-separated part of the header line, e.g.
/// `leading_number(" 63 candlestick patterns ", "candlestick patterns") == Some(63)`.
fn leading_number(part: &str, suffix: &str) -> Option<usize> {
    part.trim().strip_suffix(suffix)?.trim().parse().ok()
}

fn parse_readme() -> Readme {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/README.md");
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {path}: {e}; the README is part of the package"));

    let header = text.lines().find(|l| l.starts_with("**") && l.contains(" indicators**")).expect(
        "README.md has no counts line; the line under the title must read \
             `**N indicators** · P candlestick patterns · C categories`",
    );
    let (total_part, rest) = header.split_once(" indicators**").expect("checked by the find above");
    let total = total_part.trim_start_matches("**").trim().parse().unwrap_or_else(|_| {
        panic!("README.md counts line has no number before `indicators`: {header}")
    });
    let parts: Vec<&str> = rest.split('·').collect();
    let patterns =
        parts.iter().find_map(|p| leading_number(p, "candlestick patterns")).unwrap_or_else(|| {
            panic!("README.md counts line has no `P candlestick patterns` part: {header}")
        });
    let categories = parts
        .iter()
        .find_map(|p| leading_number(p, "categories"))
        .unwrap_or_else(|| panic!("README.md counts line has no `C categories` part: {header}"));

    let rows: Vec<(String, usize)> = text
        .lines()
        .skip_while(|l| l.trim() != TABLE_HEADER)
        .skip(1)
        .take_while(|l| l.starts_with('|'))
        .filter_map(|l| {
            let cells: Vec<&str> = l.split('|').map(str::trim).collect();
            let count = cells.get(2)?.parse().ok()?; // the `|---|---|` rule row has no number
            Some((cells.get(1)?.to_string(), count))
        })
        .collect();
    assert!(
        !rows.is_empty(),
        "README.md has no category table; it must start with the line `{TABLE_HEADER}`"
    );

    Readme { total, patterns, categories, rows }
}

fn readme_row(readme: &Readme, label: &str) -> Option<usize> {
    readme.rows.iter().find(|(l, _)| l == label).map(|(_, n)| *n)
}

fn rows_in(category: Category) -> usize {
    registry().iter().filter(|m| m.category == category).count()
}

#[test]
fn readme_total_matches_the_registries() {
    let readme = parse_readme();
    let single = registry().len();
    let pairs = pair_registry().len();
    assert_eq!(
        readme.total,
        single + pairs,
        "README.md says `{} indicators` but the code has {single} single-series + {pairs} pair = {}; \
         change the first number of the counts line (and the table below it)",
        readme.total,
        single + pairs
    );
}

#[test]
fn readme_pattern_count_matches_the_registry() {
    let readme = parse_readme();
    let actual = rows_in(Category::Pattern);
    assert_eq!(
        readme.patterns, actual,
        "README.md says `{} candlestick patterns` but the registry has {actual} rows in \
         Category::Pattern; change the pattern count on the counts line",
        readme.patterns
    );
}

#[test]
fn readme_table_has_one_correct_row_per_category() {
    let readme = parse_readme();
    for &category in Category::ALL {
        let label = category.label();
        let actual = rows_in(category);
        match (readme_row(&readme, label), actual) {
            (Some(n), a) if n == a && a > 0 => {}
            (None, 0) => {} // a variant no built-in row carries (`User`) has no row
            (Some(n), 0) => panic!(
                "README.md table lists `{label}` with {n}, but no registry row has {category:?}; \
                 delete that row from the table"
            ),
            (Some(n), a) => panic!(
                "README.md table says `{label}` has {n} indicators but the registry has {a}; \
                 change that row's number"
            ),
            (None, a) => panic!(
                "README.md table has no row for `{label}` ({a} indicators in {category:?}); add \
                 `| {label} | {a} |` to the table and raise the categories count if it is new"
            ),
        }
    }

    let pairs = pair_registry().len();
    assert_eq!(
        readme_row(&readme, PAIR_ROW),
        Some(pairs),
        "README.md table must have the row `| {PAIR_ROW} | {pairs} |` (pair_registry() has {pairs})"
    );

    for (label, _) in &readme.rows {
        let known = label == PAIR_ROW || Category::ALL.iter().any(|c| c.label() == label);
        assert!(
            known,
            "README.md table has a row `{label}` that is no Category::label and not `{PAIR_ROW}`"
        );
    }
}

#[test]
fn readme_category_count_and_table_sum_agree_with_the_code() {
    let readme = parse_readme();
    let actual = Category::ALL.iter().filter(|&&c| rows_in(c) > 0).count();
    assert_eq!(
        readme.categories, actual,
        "README.md says `{} categories` but {actual} Category variants have registry rows; \
         change the categories count on the counts line",
        readme.categories
    );
    let sum: usize = readme.rows.iter().map(|(_, n)| n).sum();
    assert_eq!(
        sum, readme.total,
        "README.md table rows add up to {sum} but the counts line says {}; the table must \
         partition the total",
        readme.total
    );
}
