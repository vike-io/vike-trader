//! The declared credential SCOPE, exercised through this binary's own `api_key` resolver: both
//! halves, and why they exist, are `vike_backfill::cli::assert_scope_covers_its_read`'s.

use super::*;

#[test]
fn the_declared_scope_covers_the_name_this_binary_reads() {
    vike_backfill::cli::assert_scope_covers_its_read(&SCOPE, api_key);
}
