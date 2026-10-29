//! Canonical Hanzo H: rigid Y-axis extrusion, ray-cast into terminal Braille.
//!
//! The front-facing outline comes from the package's actual mark signed-distance
//! field, not a hand-drawn voxel approximation. Square subdivisions are only
//! shading cues; they must never alter the canonical outer silhouette.
//! Each dot has one nearest surface, so overlapping faces cannot flash.

use std::f64::consts::TAU;

use super::geometry::{EXTENT, FIELDS, GRID, STEP};
use super::lighting::Lighting;

pub(super) const MAX_COLUMNS: u16 = 60;
pub(super) const MAX_ROWS: u16 = 24;
const DOT_BITS: [u8; 8] = [1, 8, 2, 16, 4, 32, 64, 128];
const HALF_DEPTH: f64 = 0.34;
const CUBE_PITCH: f64 = 2.0 / 12.0;
const CAMERA_DISTANCE: f64 = 4.5;
const HIT_EPSILON: f64 = 0.006;

// Bilinear interpolation of the canonical mark field. Positive is inside.
fn mark_distance(x: f64, y: f64) -> f64 {
    let fx = (x + EXTENT) / STEP;
    let fy = (y + EXTENT) / STEP;
    if !(0.0..(GRID - 1) as f64).contains(&fx)
        || !(0.0..(GRID - 1) as f64).contains(&fy)
    {
        return -1.0;
    }
    let ix = fx.floor() as usize;
    let iy = fy.floor() as usize;
    let a = fx - ix as f64;
    let b = fy - iy as f64;
    let field = &FIELDS[0];
    let sample = |dx: usize, dy: usize| f64::from(field[(iy + dy) * GRID + ix + dx]);
    let top = sample(0, 0) * (1.0 - a) + sample(1, 0) * a;
    let bottom = sample(0, 1) * (1.0 - a) + sample(1, 1) * a;
    top * (1.0 - b) + bottom * b
}

fn outward_normal(p: [f64; 3]) -> [f64; 3] {
    let cap = p[2].abs() - HALF_DEPTH;
    let rim = -mark_distance(p[0], p[1]);
    if cap >= rim - 0.006 {
        return [0.0, 0.0, if p[2] >= 0.0 { 1.0 } else { -1.0 }];
    }
    let epsilon = STEP;
    let nx = mark_distance(p[0] - epsilon, p[1])
        - mark_distance(p[0] + epsilon, p[1]);
    let ny = mark_distance(p[0], p[1] - epsilon)
        - mark_distance(p[0], p[1] + epsilon);
    let magnitude = nx.hypot(ny).max(1e-9);
    [nx / magnitude, ny / magnitude, 0.0]
}

fn box_interval(origin: [f64; 3], direction: [f64; 3]) -> Option<(f64, f64)> {
    let min = [-EXTENT, -EXTENT, -HALF_DEPTH];
    let max = [EXTENT, EXTENT, HALF_DEPTH];
    let (mut enter, mut exit) = (0.0_f64, 20.0_f64);
    for axis in 0..3 {
        let dir = direction[axis];
        if dir.abs() < 1e-9 {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
            continue;
        }
        let t0 = (min[axis] - origin[axis]) / dir;
        let t1 = (max[axis] - origin[axis]) / dir;
        enter = enter.max(t0.min(t1));
        exit = exit.min(t0.max(t1));
        if enter > exit {
            return None;
        }
    }
    Some((enter, exit))
}

fn cube_seam(p: [f64; 3], normal: [f64; 3]) -> f64 {
    // Inlaid, subtle voxel seams; faces remain coplanar and no second
    // reflective surface is introduced. At the logo boundary the exact
    // mark always wins over this interior decorative grid.
    let grid_distance = |v: f64| {
        let coordinate = ((v + 1.0) / CUBE_PITCH).rem_euclid(1.0);
        coordinate.min(1.0 - coordinate)
    };
    let seam = if normal[2].abs() > 0.5 {
        grid_distance(p[0]).min(grid_distance(p[1]))
    } else if normal[0].abs() > normal[1].abs() {
        grid_distance(p[1]).min(grid_distance(p[2]))
    } else {
        grid_distance(p[0]).min(grid_distance(p[2]))
    };
    if seam < 0.035 { 0.78 } else { 1.0 }
}

fn hit_color(screen_x: f64, screen_y: f64, yaw: f64, light: &Lighting) -> Option<u32> {
    let (sin_y, cos_y) = yaw.sin_cos();
    // Orthographic rays: constant camera, a single rigid Y rotation.
    let origin = [
        cos_y * screen_x - sin_y * CAMERA_DISTANCE,
        screen_y,
        sin_y * screen_x + cos_y * CAMERA_DISTANCE,
    ];
    let direction = [sin_y, 0.0, -cos_y];
    let (mut t, stop) = box_interval(origin, direction)?;
    for _ in 0..180 {
        if t > stop + HIT_EPSILON {
            break;
        }
        let p = std::array::from_fn(|axis| origin[axis] + t * direction[axis]);
        let signed = (-mark_distance(p[0], p[1])).max(p[2].abs() - HALF_DEPTH);
        if signed <= HIT_EPSILON {
            let local_n = outward_normal(p);
            let world_n = [
                local_n[0] * cos_y + local_n[2] * sin_y,
                local_n[1],
                -local_n[0] * sin_y + local_n[2] * cos_y,
            ];
            let rgb = light.shade(world_n, 0.0, 0.0);
            let [_, r, g, b] = rgb.to_be_bytes();
            let seam = cube_seam(p, local_n);
            return Some(u32::from_be_bytes([
                0,
                (f64::from(r) * seam) as u8,
                (f64::from(g) * seam) as u8,
                (f64::from(b) * seam) as u8,
            ]));
        }
        t += signed.max(0.008) * 0.82;
    }
    None
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Cell {
    pub(super) dots: u8,
    pub(super) rgb: u32,
}

pub(super) struct Renderer {
    cells: Vec<Cell>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self {
            cells: vec![Cell::default(); usize::from(MAX_COLUMNS) * usize::from(MAX_ROWS)],
        }
    }
}

impl Renderer {
    pub(super) fn frame(
        &mut self,
        columns: u16,
        rows: u16,
        phase: f64,
        light: &Lighting,
    ) -> &[Cell] {
        assert!(columns <= MAX_COLUMNS && rows <= MAX_ROWS);
        let cols = usize::from(columns);
        let rows = usize::from(rows);
        let dot_cols = cols * 2;
        let dot_rows = rows * 4;
        let yaw = phase.rem_euclid(1.0) * TAU;
        // Braille dots are approximately square in a typical 1:2 terminal
        // character cell. Match X/Y world units, not raw row/column counts.
        let world_per_dot = 2.65 / dot_rows as f64;
        self.cells[..cols * rows].fill(Cell::default());
        for row in 0..rows {
            for col in 0..cols {
                let (mut dots, mut count, mut rgb) = (0, 0_u32, [0_u32; 3]);
                for (point, bit) in DOT_BITS.into_iter().enumerate() {
                    let dx = col * 2 + point % 2;
                    let dy = row * 4 + point / 2;
                    let x = (dx as f64 + 0.5 - dot_cols as f64 * 0.5) * world_per_dot;
                    // SVG Y coordinates run downward from the top of the mark.
                    let y = (dy as f64 + 0.5 - dot_rows as f64 * 0.5) * world_per_dot;
                    if let Some(color) = hit_color(x, y, yaw, light) {
                        dots |= bit;
                        let channels = color.to_be_bytes();
                        for channel in 0..3 {
                            rgb[channel] += u32::from(channels[channel + 1]);
                        }
                        count += 1;
                    }
                }
                let color = if count == 0 {
                    0
                } else {
                    let [r, g, b] = rgb.map(|c| ((c + count / 2) / count) as u8);
                    u32::from_be_bytes([0, r, g, b])
                };
                self.cells[row * cols + col] = Cell { dots, rgb: color };
            }
        }
        &self.cells[..cols * rows]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_turn_is_seamless_at_80_columns() {
        let mut renderer = Renderer::default();
        let light = Lighting::terminal((230, 230, 230), (10, 10, 10));
        let first = renderer.frame(60, 21, 0.0, &light).to_vec();
        let last = renderer.frame(60, 21, 1.0, &light).to_vec();
        assert_eq!(first, last);
        assert!(first.iter().any(|c| c.dots != 0));
        assert!(renderer.frame(60, 21, 0.25, &light).iter().any(|c| c.dots != 0));
    }

    #[test]
    fn small_stage_does_not_clip_or_panic() {
        let mut renderer = Renderer::default();
        let light = Lighting::terminal((230, 230, 230), (10, 10, 10));
        assert_eq!(renderer.frame(40, 14, 0.0, &light).len(), 560);
    }
}
