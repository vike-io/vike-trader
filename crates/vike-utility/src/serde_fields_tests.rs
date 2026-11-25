use serde::Deserialize;

use super::*;

#[derive(Deserialize)]
#[expect(dead_code, reason = "a struct here is a list of names: nothing reads a field")]
struct Declared {
    first: u8,
    #[serde(rename = "second")]
    renamed: u8,
    #[serde(skip)]
    skipped: u8,
    third: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "a struct here is a list of names: nothing reads a field")]
struct Aliased {
    #[serde(alias = "alpha")]
    zeta: u8,
    #[serde(alias = "omega")]
    beta: u8,
    gamma: u8,
}

/// The names the derive reads, in declaration order: a rename under its new name, a skipped field
/// left out.
#[test]
fn the_declared_fields_are_the_derives_own_list() {
    assert_eq!(declared_fields::<Declared>(), ["first", "second", "third"]);
}

/// An alias sits in its field's group, wherever serde's sort puts it: `alpha, zeta, beta, omega` is
/// two fields of two names each, which the flat list alone cannot tell from `alpha` + `zeta, beta` + ...
#[test]
fn each_field_groups_its_own_name_with_its_aliases() {
    assert_eq!(declared_fields::<Aliased>(), ["alpha", "zeta", "beta", "omega", "gamma"]);
    assert_eq!(
        field_groups::<Aliased>(),
        [vec!["alpha", "zeta"], vec!["beta", "omega"], vec!["gamma"]]
    );
    assert_eq!(field_groups::<Declared>(), [vec!["first"], vec!["second"], vec!["third"]]);
}

/// A type that is no struct with named fields has no field list to give.
#[test]
fn a_type_that_is_no_struct_has_no_fields() {
    assert!(declared_fields::<u8>().is_empty());
    assert!(declared_fields::<Vec<String>>().is_empty());
    assert!(field_groups::<Option<u8>>().is_empty());
}
