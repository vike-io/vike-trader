//! The `agent` module's unit tests, one file per role; the shared fixtures live in `support`.
//!
//! Each child carries its own `#[cfg(test)]` although this whole module is test-only already: the
//! repo's path-keyed scans (`vike_model::libm_walk::cfg_test_module_files`) follow ONE level of
//! `#[cfg(test)] mod x;`, so an unmarked grandchild reads as production source.

#[cfg(test)]
mod authoring;
#[cfg(test)]
mod deflation;
#[cfg(test)]
mod grounding;
#[cfg(test)]
mod support;
