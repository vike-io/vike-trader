use super::*;

fn set_args(argv: &[&str]) -> Args {
    let mut full = vec!["--profile".to_string(), "p.toml".to_string()];
    full.extend(argv.iter().map(|s| (*s).to_string()));
    parse_run_args(full.into_iter()).expect("parses")
}

fn explicit(a: &Args) -> Vec<&Override> {
    a.overrides.iter().filter(|o| !matches!(o.origin, Origin::Implied(_))).collect()
}

#[path = "tests/builder_sugar_param.rs"]
#[cfg(test)]
mod builder_sugar_param;
#[path = "tests/effective_and_ladder.rs"]
#[cfg(test)]
mod effective_and_ladder;
#[path = "tests/preset_and_set.rs"]
#[cfg(test)]
mod preset_and_set;
#[path = "tests/reading_verbs.rs"]
#[cfg(test)]
mod reading_verbs;
#[path = "tests/route_and_spine.rs"]
#[cfg(test)]
mod route_and_spine;
