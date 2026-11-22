//! Core model.
pub mod graph;
mod store;
#[path = "gen/ids.rs"]
pub mod ids;
mod missing;

pub use graph::*;
pub use store::Store;

use crate::graph::node::Node;
use crate::generated::Table;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store as S;
}
