//! Deterministic simulator around the Fleet kernel transition function.

pub mod fixture;
pub mod world;

pub use fixture::{GIB, MachineIds, TinyGraph, tiny_graph};
pub use world::{TraceEvent, World};
