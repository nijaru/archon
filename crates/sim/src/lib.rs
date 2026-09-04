//! Deterministic simulator around the Archon kernel transition function.

pub mod fixture;
pub mod measure;
pub mod world;

pub use fixture::{GIB, MachineIds, TinyGraph, tiny_graph};
pub use measure::{ScaleParams, ScaleReport, measure_scale};
pub use world::{TraceEvent, World};
