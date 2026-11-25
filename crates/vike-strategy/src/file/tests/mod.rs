//! The tests of [`crate::file`], one file per concern. Each child carries its own `#[cfg(test)]`, so
//! a source-walking gate never reads one as production.

#[cfg(test)]
mod header;
#[cfg(test)]
mod model;
#[cfg(test)]
mod scan;
