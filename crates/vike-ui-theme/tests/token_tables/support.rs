//! The three-way-merge check the `[[value]]` and `[[map]]` suites share: two edits to neighbouring
//! sections of a generated table change lines with an unchanged line between them, so a merge takes
//! both. Each suite hands in its own `texts` (the TOML a person edits plus the outputs that carry
//! the rows, for a planted table whose sections `a`, `b` and `c` have rows exactly when named).

/// The texts of one planted table, as `(output name, text)`.
pub type Texts = Vec<(&'static str, String)>;

/// The lines of `before` that `after` replaced, as `(first, one past the last)` in `before`'s numbering,
/// and what stands there in `after`.
fn change(before: &str, after: &str) -> (usize, usize, Vec<String>) {
    let (b, a): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let prefix = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let suffix =
        b[prefix..].iter().rev().zip(a[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let replacement = a[prefix..a.len() - suffix].iter().map(|l| l.to_string()).collect();
    (prefix, b.len() - suffix, replacement)
}

/// Applies two changes to the same `before` the way a three-way merge does, or says they touch.
fn merge(
    before: &str,
    first: (usize, usize, Vec<String>),
    second: (usize, usize, Vec<String>),
) -> Option<String> {
    let lines: Vec<&str> = before.lines().collect();
    // A conflict is a change that touches the other: no unchanged line between them.
    if first.1 >= second.0 {
        return None;
    }
    let mut out: Vec<String> = lines[..first.0].iter().map(|l| l.to_string()).collect();
    out.extend(first.2);
    out.extend(lines[first.1..second.0].iter().map(|l| l.to_string()));
    out.extend(second.2);
    out.extend(lines[second.1..].iter().map(|l| l.to_string()));
    Some(out.join("\n") + "\n")
}

/// Merges, in each of the texts, the edit that makes `one` out of `before` with the edit that makes
/// `two` out of it, and requires that they do not touch and that the result is `both`.
fn assert_merges(
    before: &[(&str, String)],
    one: &[(&str, String)],
    two: &[(&str, String)],
    both: &[(&str, String)],
    what: &str,
) {
    for (((b, o), t), w) in before.iter().zip(one).zip(two).zip(both) {
        let name = b.0;
        let merged = merge(&b.1, change(&b.1, &o.1), change(&b.1, &t.1))
            .unwrap_or_else(|| panic!("{name}: {what}: the two edits change adjacent lines"));
        assert_eq!(merged, w.1, "{name}: {what}: the merge is not what a table with both would be");
    }
}

/// The first rows of two sections merge without touching, for every pair of the three sections
/// (first, middle, last).
pub fn assert_first_rows_of_two_sections_merge(texts: impl Fn(&[&str]) -> Texts) {
    let empty = texts(&[]);
    for (first, second) in [("a", "b"), ("b", "c"), ("a", "c")] {
        let what = format!("first rows of `{first}` and `{second}`");
        let (one, two, both) = (texts(&[first]), texts(&[second]), texts(&[first, second]));
        assert_merges(&empty, &one, &two, &both, &what);
    }
}

/// The same once the section in between already has rows: the neighbours' first rows still merge.
pub fn assert_a_filled_middle_keeps_the_neighbours_apart(texts: impl Fn(&[&str]) -> Texts) {
    assert_merges(
        &texts(&["b"]),
        &texts(&["a", "b"]),
        &texts(&["b", "c"]),
        &texts(&["a", "b", "c"]),
        "first rows of `a` and `c` around a filled `b`",
    );
}
