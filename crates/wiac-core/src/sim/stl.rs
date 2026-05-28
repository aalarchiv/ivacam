//! Binary STL export of a carved [`Heightmap`] (9c34).
//!
//! Produces a watertight triangle mesh of the simulated stock so users can
//! inspect / 3-D print the post-cut geometry, and so future visual
//! regression tests (notably v5az's chamfer cone-below-floor bug) can diff
//! against a reference STL instead of relying on screenshots.
//!
//! ## Mesh shape
//!
//! Top surface — every 2×2 block of sample points becomes two triangles,
//! so the carved heightfield reads as a smooth (interpolated) topographic
//! mesh rather than Minecraft-style voxel boxes. Pairs with how the
//! heightmap's bilinear `sample()` already treats the data as a regular
//! grid of point samples, not unit cells.
//!
//! Perimeter walls — the four edge runs of samples each drop straight
//! down to `stock_bottom_z`. Plus one flat bottom quad. Together this
//! gives a watertight mesh suitable for STL viewers / mesh-compare tools.
//!
//! ## Triangle budget
//!
//! For a `cols × rows` heightmap: `2 * (cols-1) * (rows-1)` top triangles
//! + `4 * (cols + rows - 2)` perimeter triangles + 2 bottom triangles.
//! At 50 bytes/tri the binary STL fits in ~1 MB per 200×200 grid.

use crate::sim::heightmap::Heightmap;

/// Serialize the heightmap as a binary STL with a flat bottom plane at
/// `stock_bottom_z`. Returns the raw bytes — callers wire it through the
/// active transport's "save bytes to file" facility.
///
/// `stock_bottom_z` is the absolute Z of the stock's underside (typically
/// `top_z - stock_thickness`). Edge walls of the mesh drop from the
/// height at each perimeter sample down to this plane.
#[must_use]
pub fn heightmap_to_stl_binary(hm: &Heightmap, stock_bottom_z: f32) -> Vec<u8> {
    let cols = hm.cols as usize;
    let rows = hm.rows as usize;
    let cell = hm.cell as f32;
    let ox = hm.origin.x as f32;
    let oy = hm.origin.y as f32;

    // Sample point in world coordinates (cell-center-as-grid-vertex).
    let pos = |c: usize, r: usize, z: f32| -> [f32; 3] {
        [ox + c as f32 * cell, oy + r as f32 * cell, z]
    };
    let h = |c: usize, r: usize| -> f32 { hm.data[r * cols + c] };

    let mut tris: Vec<[[f32; 3]; 3]> = Vec::new();

    // ── Top surface: 2 triangles per 2×2 sample block, CCW from +Z ─────
    for r in 0..rows.saturating_sub(1) {
        for c in 0..cols.saturating_sub(1) {
            let p00 = pos(c, r, h(c, r));
            let p10 = pos(c + 1, r, h(c + 1, r));
            let p01 = pos(c, r + 1, h(c, r + 1));
            let p11 = pos(c + 1, r + 1, h(c + 1, r + 1));
            tris.push([p00, p10, p11]);
            tris.push([p00, p11, p01]);
        }
    }

    // ── Perimeter walls: each edge run drops to stock_bottom_z ────────
    //
    // Winding convention: each tri's vertices listed CCW when viewed from
    // OUTSIDE the volume, so the right-hand-rule normal points outward.

    // South edge (r = 0), outward normal -Y.
    for c in 0..cols.saturating_sub(1) {
        let top0 = pos(c, 0, h(c, 0));
        let top1 = pos(c + 1, 0, h(c + 1, 0));
        let bot0 = pos(c, 0, stock_bottom_z);
        let bot1 = pos(c + 1, 0, stock_bottom_z);
        tris.push([top0, bot0, bot1]);
        tris.push([top0, bot1, top1]);
    }
    // North edge (r = rows-1), outward normal +Y.
    if rows >= 2 {
        let last_r = rows - 1;
        for c in 0..cols.saturating_sub(1) {
            let top0 = pos(c, last_r, h(c, last_r));
            let top1 = pos(c + 1, last_r, h(c + 1, last_r));
            let bot0 = pos(c, last_r, stock_bottom_z);
            let bot1 = pos(c + 1, last_r, stock_bottom_z);
            tris.push([top1, bot1, bot0]);
            tris.push([top1, bot0, top0]);
        }
    }
    // West edge (c = 0), outward normal -X.
    for r in 0..rows.saturating_sub(1) {
        let top0 = pos(0, r, h(0, r));
        let top1 = pos(0, r + 1, h(0, r + 1));
        let bot0 = pos(0, r, stock_bottom_z);
        let bot1 = pos(0, r + 1, stock_bottom_z);
        tris.push([top1, bot1, bot0]);
        tris.push([top1, bot0, top0]);
    }
    // East edge (c = cols-1), outward normal +X.
    if cols >= 2 {
        let last_c = cols - 1;
        for r in 0..rows.saturating_sub(1) {
            let top0 = pos(last_c, r, h(last_c, r));
            let top1 = pos(last_c, r + 1, h(last_c, r + 1));
            let bot0 = pos(last_c, r, stock_bottom_z);
            let bot1 = pos(last_c, r + 1, stock_bottom_z);
            tris.push([top0, bot0, bot1]);
            tris.push([top0, bot1, top1]);
        }
    }
    // Bottom face: one big quad at stock_bottom, outward normal -Z.
    if cols >= 2 && rows >= 2 {
        let bot00 = pos(0, 0, stock_bottom_z);
        let bot10 = pos(cols - 1, 0, stock_bottom_z);
        let bot01 = pos(0, rows - 1, stock_bottom_z);
        let bot11 = pos(cols - 1, rows - 1, stock_bottom_z);
        tris.push([bot00, bot01, bot11]);
        tris.push([bot00, bot11, bot10]);
    }

    // ── Binary STL: 80-byte header + u32 count + 50 bytes / triangle ──
    let mut out: Vec<u8> = Vec::with_capacity(84 + tris.len() * 50);
    let mut header = [0u8; 80];
    let banner = b"wiaConstructor simulated stock STL (9c34)";
    header[..banner.len()].copy_from_slice(banner);
    out.extend_from_slice(&header);
    out.extend_from_slice(&(tris.len() as u32).to_le_bytes());
    for tri in &tris {
        let n = triangle_normal(tri);
        for f in n {
            out.extend_from_slice(&f.to_le_bytes());
        }
        for v in tri {
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        // Attribute byte count — always zero.
        out.extend_from_slice(&[0u8; 2]);
    }
    out
}

/// Right-hand-rule normal of a triangle. Degenerate (zero-area) triangles
/// fall back to +Z so an STL viewer doesn't paint them black.
fn triangle_normal(tri: &[[f32; 3]; 3]) -> [f32; 3] {
    let a = tri[0];
    let b = tri[1];
    let c = tri[2];
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len2 = n[0] * n[0] + n[1] * n[1] + n[2] * n[2];
    if len2 > 1e-18 {
        let inv = 1.0 / len2.sqrt();
        [n[0] * inv, n[1] * inv, n[2] * inv]
    } else {
        [0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point2;

    /// Triangle count matches the closed-form formula: `2·(cols-1)·(rows-1)`
    /// top tris + `4·(cols + rows - 2)` perimeter tris + 2 bottom tris.
    #[test]
    fn triangle_count_matches_formula() {
        let hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 5, 4, 0.0);
        let bytes = heightmap_to_stl_binary(&hm, -10.0);
        // u32 triangle count lives at offset 80 (after the 80-byte header).
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
        let expected = 2 * (5 - 1) * (4 - 1) + 4 * (5 + 4 - 2) + 2;
        assert_eq!(count, expected);
        // Header + u32 + 50 bytes per triangle.
        assert_eq!(bytes.len(), 84 + count * 50);
    }

    /// A flat (uncarved) heightmap produces a top surface whose triangles
    /// all face +Z, and a bottom whose triangles face -Z. Walls face
    /// outward (+/- X or Y). Sanity-checks the winding convention.
    #[test]
    fn flat_heightmap_has_axis_aligned_normals() {
        let hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 3, 3, 2.5);
        let bytes = heightmap_to_stl_binary(&hm, -1.0);
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
        // For each triangle, the normal is at byte offset 84 + i*50.
        let mut up = 0usize;
        let mut down = 0usize;
        let mut sideways = 0usize;
        for i in 0..count {
            let off = 84 + i * 50;
            let nx = f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
            let ny = f32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap());
            let nz = f32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap());
            if (nz - 1.0).abs() < 1e-3 && nx.abs() < 1e-3 && ny.abs() < 1e-3 {
                up += 1;
            } else if (nz + 1.0).abs() < 1e-3 && nx.abs() < 1e-3 && ny.abs() < 1e-3 {
                down += 1;
            } else if nz.abs() < 1e-3 {
                sideways += 1;
            }
        }
        assert_eq!(up, 2 * (3 - 1) * (3 - 1), "top triangles all +Z");
        assert_eq!(down, 2, "exactly two bottom triangles, -Z");
        assert_eq!(sideways, 4 * (3 + 3 - 2), "perimeter walls, normal in XY");
    }

    /// Header begins with our banner and the STL is byte-stable across
    /// repeated calls (no nondeterministic ordering).
    #[test]
    fn header_banner_and_byte_stability() {
        let hm = Heightmap::new(Point2::new(2.0, -3.0), 0.5, 4, 4, 1.0);
        let a = heightmap_to_stl_binary(&hm, 0.0);
        let b = heightmap_to_stl_binary(&hm, 0.0);
        assert_eq!(a, b, "stl bytes must be deterministic");
        assert!(
            a.starts_with(b"wiaConstructor simulated stock STL (9c34)"),
            "header banner missing",
        );
    }

    /// A carved cell produces a non-flat top surface — verify the affected
    /// region's Z values appear in the emitted vertex data.
    #[test]
    fn carved_cells_show_up_in_vertex_data() {
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 3, 3, 0.0);
        // Carve the centre cell down to -2.5 mm.
        hm.data[1 * 3 + 1] = -2.5;
        let bytes = heightmap_to_stl_binary(&hm, -10.0);
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
        let mut saw_carved_z = false;
        for i in 0..count {
            let off = 84 + i * 50 + 12; // skip 3 f32 normal
            for v in 0..3 {
                let vz = f32::from_le_bytes(
                    bytes[off + v * 12 + 8..off + v * 12 + 12]
                        .try_into()
                        .unwrap(),
                );
                if (vz - (-2.5)).abs() < 1e-3 {
                    saw_carved_z = true;
                }
            }
        }
        assert!(saw_carved_z, "carved Z (-2.5) should appear in some triangle's vertex list");
    }
}
