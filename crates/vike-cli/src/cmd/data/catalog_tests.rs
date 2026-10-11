use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(&argv(args), None)
}

/// ⚠ **The two groups spell one rule and there is no shared const to import**, so this is what
/// holds them equal: `crate::cmd::data`'s `parse` refuses the identical contradiction for the
/// `hist` group, and a reader who meets both must not learn that one is a different KIND of
/// no. It fails when either side is reworded, which is the moment to reword the other.
///
/// ⚠ **The comparison is over WORDS, not bytes, and that is a measurement rather than a
/// loosening.** The sibling's literal carries an eighteen-space run where a `\` line
/// continuation was meant — measured on this branch — so a byte comparison would demand that
/// this file reproduce that spacing in order to pass, which is copying a typographic defect
/// into a second place under the guise of agreement. Splitting on whitespace compares the
/// sentence both operators actually read, and still reddens on any rewording of either side.
#[test]
fn the_two_groups_refuse_the_json_format_contradiction_in_the_same_words() {
    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }
    let mine = parse_of(&["venues", "--json", "--format", "table"])
        .expect_err("the contradiction is refused");
    let hist = super::super::parse(
        ["hist", "ls", "--json", "--format", "table"].into_iter().map(String::from),
        None,
    )
    .expect_err("the sibling group refuses it too");
    assert_eq!(words(&mine), words(&hist), "one group reworded the shared refusal");
    // Anti-vacuity: `words` on two empty or two generic strings would also compare equal, so
    // the sentence has to be the real one — and it has to be more than a couple of tokens.
    assert!(mine.contains("--json") && mine.contains("--format table"), "{mine}");
    assert!(words(&mine).len() > 8, "a near-empty message would compare equal too: {mine}");
}

/// **…and the `jsonl` spelling, which is the one that actually parted.**
///
/// ⚠ `crate::cmd::data`'s `parse` grew a `--json --format jsonl` arm with `get`, and applied
/// it ABOVE the verb dispatch — so `data hist ls --json --format jsonl` answered with a
/// sentence about GET's document, ending "Pass one", while `data catalog ls --json --format
/// jsonl` answered with `ROW_VERB`, because THIS parser reads `--format` eagerly and its
/// contradiction check never sees a `jsonl` at all. One question, two answers, on the same
/// plane. The arm is `Sub::Get`'s alone now and this is what holds the two groups equal — the
/// case the `table` twin above could never have covered, since `table` is valid on both.
#[test]
fn the_two_groups_refuse_the_jsonl_format_contradiction_in_the_same_words() {
    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }
    let mine = parse_of(&["venues", "--json", "--format", "jsonl"])
        .expect_err("a catalog verb emits no rows");
    let hist = super::super::parse(
        ["hist", "ls", "--json", "--format", "jsonl"].into_iter().map(String::from),
        None,
    )
    .expect_err("the sibling group refuses it too");
    assert_eq!(words(&mine), words(&hist), "one group reworded the shared refusal");
    // Anti-vacuity, the same two rungs the twin above uses: the sentence has to be the real
    // one, and it has to name where `jsonl` DOES work rather than merely be long.
    assert!(mine.contains(super::super::ROW_VERB), "{mine}");
    assert!(words(&mine).len() > 8, "a near-empty message would compare equal too: {mine}");
    // THE CONTROL: the verb that SERVES `jsonl` answers differently, so the equality above is
    // about these two groups agreeing rather than about one sentence for every line.
    let get = super::super::parse(
        ["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"]
            .into_iter()
            .map(String::from),
        None,
    )
    .expect_err("a sequence is not one document");
    assert_ne!(words(&get), words(&mine), "`get` has a contradiction of its own: {get}");
}

#[path = "catalog_tests/grammar.rs"]
#[cfg(test)]
mod grammar;

#[path = "catalog_tests/ls.rs"]
#[cfg(test)]
mod ls;

#[path = "catalog_tests/venues.rs"]
#[cfg(test)]
mod venues;
