//! `rename`'s grammar and its route: the venue and both names, exactly, on the engine route alone.

use super::*;

/// `history rename --venue d --symbol OLD --to NEW` plus `extra`, through the plane's parser.
fn rename_line(extra: &[&str]) -> Result<Args, crate::cmd::args::Stop> {
    let argv: Vec<&str> = ["history", "rename", "--venue", "d", "--symbol", "OLD", "--to", "NEW"]
        .iter()
        .chain(extra)
        .copied()
        .collect();
    parse_of(&argv)
}

/// The names land where they belong — `--to` is the NEW name, never a window bound — and the
/// optional narrowing is carried as typed.
#[test]
fn rename_parses_the_venue_both_names_and_the_narrowing() {
    let a = rename_line(&["--kind", "quote", "--lane", "recorder", "--dry-run"]).unwrap();
    let r = a.rename.as_ref().expect("a RenameArgs");
    assert_eq!((r.venue.as_str(), r.from.as_str(), r.to.as_str()), ("d", "OLD", "NEW"));
    assert_eq!((r.kind.as_deref(), r.source.as_deref()), (Some("quote"), Some("recorder")));
    assert!(r.dry_run && !r.yes);
    assert_eq!(a.window, None, "--to is the new name here, not a window");
    assert_eq!(a.filter, Filter::default(), "--kind/--venue were moved into the selector");
}

/// Each missing name and each foreign flag is refused by name, with the reason.
#[test]
fn rename_refuses_a_missing_name_a_group_a_window_and_the_remote_route() {
    for (argv, needle) in [
        (vec!["history", "rename", "--symbol", "OLD", "--to", "NEW"], "rename needs --venue"),
        (vec!["history", "rename", "--venue", "d", "--to", "NEW"], "rename needs --symbol"),
        (vec!["history", "rename", "--venue", "d", "--symbol", "OLD"], "rename needs --to"),
    ] {
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains(needle), "{argv:?} must say {needle:?}: {err}");
    }
    for (extra, needle) in [
        (&["--group", "g"][..], "PER-SYMBOL"),
        (&["--from", "0"][..], "bounds a FETCH window"),
        (&["--name", "X"][..], "EXACTLY"),
        (&["--produced-by", "p:"][..], "only `rm` deletes"),
        (&["--addr", "h:1"][..], "store's own machine"),
        (&["d:OLD:1m"][..], "no positional argument"),
        (&["--interval", "1*"][..], "glob character"),
    ] {
        let err = rename_line(extra).unwrap_err();
        assert!(err.contains(needle), "{extra:?} must say {needle:?}: {err}");
    }
    let err =
        parse_of(&["history", "rename", "--venue", "d", "--symbol", "A", "--to", "A"]).unwrap_err();
    assert!(err.contains("nothing to rename"), "{err}");
    let err = parse_of(&["history", "rename", "--venue", "d", "--symbol", "A", "--to", "B/C"])
        .unwrap_err();
    assert!(err.contains("--to") && err.contains("cannot be a store key"), "{err}");
}

/// The child's argv: `store rename` with both names, the narrowing, the two confirmation flags and
/// `--json`/`--store` forwarded, `--engine` consumed here.
#[test]
fn the_rename_engine_argv_forwards_the_request_and_consumes_the_engine_path() {
    let a = rename_line(&[
        "--interval",
        "5s",
        "--yes",
        "--json",
        "--store",
        "/srv/hist",
        "--engine",
        "/opt/tool",
    ])
    .unwrap();
    assert_eq!(
        rename_engine_argv(&a, a.rename.as_ref().unwrap()),
        [
            "store",
            "rename",
            "--venue",
            "d",
            "--symbol",
            "OLD",
            "--to",
            "NEW",
            "--interval",
            "5s",
            "--yes",
            "--json",
            "--store",
            "/srv/hist"
        ]
    );
}
