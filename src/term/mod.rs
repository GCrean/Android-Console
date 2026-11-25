//! Terminal state + VT/ANSI parser, independent of any UI toolkit.

mod cell;
mod parser;
mod screen;

pub use cell::{indexed_rgb, Attrs, Cell, Color};
pub use screen::{Pos, Term};

#[cfg(test)]
mod tests;
