use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()), None)
}

#[cfg(test)]
mod grammar;

#[cfg(test)]
mod import_grammar;

#[cfg(test)]
mod write_grammar;

#[cfg(test)]
mod produced_by;

#[cfg(test)]
mod repair;

#[cfg(test)]
mod read_grammar;

#[cfg(test)]
mod list_render;

#[cfg(test)]
mod coverage_gate;

#[cfg(test)]
mod get_grammar;

#[cfg(test)]
mod fetch;
