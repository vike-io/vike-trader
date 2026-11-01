//! The ledger's unit tests, by subject: the disk round-trips and bounds, and the prompt summary.
//!
//! Each child carries its own `#[cfg(test)]` although this whole module is test-only already: the
//! repo's path-keyed scans (`vike_model::libm_walk::cfg_test_module_files`) follow ONE level of
//! `#[cfg(test)] mod x;`, so an unmarked grandchild reads as production source.

#[cfg(test)]
mod store;
#[cfg(test)]
mod summary;
#[cfg(test)]
mod support;
