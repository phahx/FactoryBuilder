//! Factory Builder engine: game-data parsing, LP planning, graph layout and
//! flow simulation.
//!
//! The `solver` feature (on by default) adds the good_lp planner and the Docs
//! parser for the native backend. The wasm frontend turns it off and uses
//! only `layout` and `sim`.

#[cfg(feature = "solver")]
pub mod docs;
pub mod layout;
pub mod sim;
#[cfg(feature = "solver")]
pub mod solver;
