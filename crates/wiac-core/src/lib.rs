//! wiaConstructor core: DXF/SVG import, CAM math, gcode generation.
//!
//! The public surface mirrors `schema/openapi.yaml` so a single set of types
//! drives the JSON contract across HTTP / Tauri / WASM transports.

#![forbid(unsafe_code)]

pub mod cam;
pub mod error;
pub mod gcode;
pub mod geometry;
pub mod input;
pub mod math;
pub mod pipeline;
pub mod project;
pub mod schema;
pub mod testing;

pub use error::{Error, Result};
pub use geometry::{BBox, Layer, Point2, Segment, SegmentKind};
pub use input::{ImportOptions, ImportOutput};
