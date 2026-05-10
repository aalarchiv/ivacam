//! 2.5D cutting simulator primitives. The heightmap stores Z(x,y) over
//! the stock footprint and tool Z-profiles describe what radius of the
//! cutter surface reaches how far down at a given radial offset.

pub mod heightmap;
pub mod sweep;
pub mod timing;
