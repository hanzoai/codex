//! Shared terminal cubelet mesh, clipped analytically to the canonical mark.
//! Each convex prism has front/back caps and every clipped side face. Rays select
//! the nearest face across all pieces; seams are empty space, never painted lines.
use std::f64::consts::TAU;
use std::sync::LazyLock;

pub const MAX_COLUMNS: u16 = 60;
pub const MAX_ROWS: u16 = 24;
pub const LOOP_SECONDS: f64 = 7.2;
const PITCH: f64 = 2.0 / 6.0;
const GAP: f64 = 0.05;
const DOT_BITS: [u8; 8] = [1, 8, 2, 16, 4, 32, 64, 128];
type Point = [f64; 2];

#[derive(Debug)]
struct Plane {
    normal: [f64; 3],
    offset: f64,
}
#[derive(Debug)]
struct Cubelet {
    planes: Vec<Plane>,
    low_y: f64,
    high_y: f64,
}
static MESH: LazyLock<Vec<Cubelet>> = LazyLock::new(mesh);

#[expect(
    clippy::expect_used,
    reason = "coordinates are from the embedded canonical MARK"
)]
fn number(path: &str, index: &mut usize) -> f64 {
    let start = *index;
    while *index < path.len()
        && (path.as_bytes()[*index].is_ascii_digit()
            || matches!(path.as_bytes()[*index], b'.' | b'-'))
    {
        *index += 1;
    }
    path[start..*index]
        .parse()
        .expect("canonical MARK coordinate")
}
fn polygons() -> Vec<Vec<Point>> {
    let (path, bounds) = crate::MARK;
    let scale = (bounds[2] - bounds[0]).max(bounds[3] - bounds[1]) / 2.0;
    let mut result = Vec::new();
    let mut polygon = Vec::new();
    let mut point = [0.0; 2];
    let mut i = 0;
    while i < path.len() {
        let command = path.as_bytes()[i];
        i += 1;
        if command.is_ascii_whitespace() {
            continue;
        }
        match command {
            b'M' | b'L' => {
                point[0] = number(path, &mut i);
                while i < path.len() && path.as_bytes()[i].is_ascii_whitespace() {
                    i += 1;
                }
                point[1] = number(path, &mut i);
            }
            b'H' => point[0] = number(path, &mut i),
            b'V' => point[1] = number(path, &mut i),
            b'Z' => {
                result.push(std::mem::take(&mut polygon));
                continue;
            }
            _ => panic!("unsupported canonical MARK command"),
        }
        polygon.push(std::array::from_fn(|axis| {
            (point[axis] - (bounds[axis] + bounds[axis + 2]) / 2.0) / scale
        }));
    }
    result
}

// Sutherland–Hodgman clipping retains the diagonal edge fragments exactly.
fn clip(polygon: Vec<Point>, axis: usize, edge: f64, sign: f64) -> Vec<Point> {
    let mut out = Vec::new();
    if polygon.is_empty() {
        return out;
    }
    let mut a = polygon[polygon.len() - 1];
    for &b in &polygon {
        let da = (a[axis] - edge) * sign;
        let db = (b[axis] - edge) * sign;
        if (da <= 0.0) != (db <= 0.0) {
            let t = da / (da - db);
            out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
        }
        if db <= 0.0 {
            out.push(b);
        }
        a = b;
    }
    out
}
fn mesh() -> Vec<Cubelet> {
    let mut pieces = Vec::new();
    for polygon in polygons() {
        for row in 0..6 {
            for col in 0..6 {
                let mut face = polygon.clone();
                for (axis, cell) in [(0, col), (1, row)] {
                    // Preserve exterior coordinates; gaps only cut grid boundaries.
                    face = clip(
                        face,
                        axis,
                        -1.0 + f64::from(cell) * PITCH + if cell > 0 { GAP / 2.0 } else { 0.0 },
                        -1.0,
                    );
                    face = clip(
                        face,
                        axis,
                        -1.0 + f64::from(cell + 1) * PITCH - if cell < 5 { GAP / 2.0 } else { 0.0 },
                        1.0,
                    );
                }
                if face.len() < 3 {
                    continue;
                }
                let area: f64 = face
                    .iter()
                    .zip(face.iter().cycle().skip(1))
                    .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
                    .sum();
                if area.abs() < 1e-10 {
                    continue;
                }
                if area < 0.0 {
                    face.reverse();
                }
                {
                    let low_z = -(PITCH - GAP) / 2.0;
                    let high_z = (PITCH - GAP) / 2.0;
                    let mut planes = vec![
                        Plane {
                            normal: [0.0, 0.0, -1.0],
                            offset: -low_z,
                        },
                        Plane {
                            normal: [0.0, 0.0, 1.0],
                            offset: high_z,
                        },
                    ];
                    for (a, b) in face
                        .iter()
                        .zip(face.iter().cycle().skip(1))
                        .take(face.len())
                    {
                        let n = [b[1] - a[1], a[0] - b[0]];
                        let len = n[0].hypot(n[1]);
                        if len < 1e-10 {
                            continue;
                        }
                        planes.push(Plane {
                            normal: [n[0] / len, n[1] / len, 0.0],
                            offset: (n[0] * a[0] + n[1] * a[1]) / len,
                        });
                    }
                    // Cap/rim bevels create physical edge facets. They preserve the
                    // XY silhouette (the rim continues behind the bevel) and make
                    // individual blocks readable when rear layers fill projected gaps.
                    let side_count = planes.len();
                    for cap in 0..2 {
                        for side in 2..side_count {
                            let a = &planes[cap];
                            let b = &planes[side];
                            let normal = std::array::from_fn(|i| {
                                (a.normal[i] + b.normal[i]) / std::f64::consts::SQRT_2
                            });
                            let offset = (a.offset + b.offset - 0.04) / std::f64::consts::SQRT_2;
                            planes.push(Plane { normal, offset });
                        }
                    }
                    pieces.push(Cubelet {
                        planes,
                        low_y: face.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
                        high_y: face.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max),
                    });
                }
            }
        }
    }
    pieces
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}
fn intersect(piece: &Cubelet, origin: [f64; 3], direction: [f64; 3]) -> Option<(f64, [f64; 3])> {
    let (mut near, mut far) = (0.0_f64, 10.0_f64);
    let mut normal = [0.0; 3];
    for plane in &piece.planes {
        let denom = dot(plane.normal, direction);
        let distance = plane.offset - dot(plane.normal, origin);
        if denom.abs() < 1e-10 {
            if distance < -1e-10 {
                return None;
            }
        } else {
            let t = distance / denom;
            if denom < 0.0 {
                if t > near {
                    near = t;
                    normal = plane.normal;
                }
            } else {
                far = far.min(t);
            }
            if near > far + 1e-10 {
                return None;
            }
        }
    }
    Some((near, normal))
}

/// Terminal palette conversion is shared by both callers.
pub struct Lighting {
    background: [f64; 3],
    ink: f64,
}
impl Lighting {
    pub fn terminal(fg: (u8, u8, u8), bg: (u8, u8, u8)) -> Self {
        Self {
            background: [f64::from(bg.0), f64::from(bg.1), f64::from(bg.2)],
            ink: (f64::from(fg.0) + f64::from(fg.1) + f64::from(fg.2)) / 3.0,
        }
    }
    fn shade(&self, n: [f64; 3]) -> u32 {
        // SVG Y points down: the fixed key light is above and left.
        let diffuse = dot(n, [-0.45, -0.55, 0.70]).max(0.0);
        let strength = 0.30 + 0.70 * diffuse;
        let [r, g, b] = self
            .background
            .map(|bg| (bg + (self.ink - bg) * strength).round().clamp(0.0, 255.0) as u8);
        u32::from_be_bytes([0, r, g, b])
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cell {
    pub dots: u8,
    pub rgb: u32,
}
#[derive(Default)]
pub struct Renderer {
    cells: Vec<Cell>,
}
impl Renderer {
    pub fn frame(&mut self, columns: u16, rows: u16, phase: f64, light: &Lighting) -> &[Cell] {
        assert!(columns <= MAX_COLUMNS && rows <= MAX_ROWS);
        let (cols, rows) = (usize::from(columns), usize::from(rows));
        self.cells.resize(cols * rows, Cell::default());
        self.cells.fill(Cell::default());
        let mut totals = vec![[0_u32; 3]; cols * rows];
        if cols == 0 || rows == 0 {
            return &self.cells;
        }
        // Equal world scale on both axes, including narrow terminal resizes.
        let pitch = (2.65 / (rows * 4) as f64).max(2.65 / (cols * 2) as f64);
        let (s, c) = (phase.rem_euclid(1.0) * TAU).sin_cos();
        let direction = [s, 0.0, -c];
        for dy in 0..rows * 4 {
            let y = (dy as f64 + 0.5 - (rows * 2) as f64) * pitch;
            let candidates: Vec<_> = MESH
                .iter()
                .filter(|p| y >= p.low_y && y <= p.high_y)
                .collect();
            for dx in 0..cols * 2 {
                let x = (dx as f64 + 0.5 - cols as f64) * pitch;
                let origin = [c * x - s * 4.5, y, s * x + c * 4.5];
                let mut closest = 10.0;
                let mut hit = None;
                for piece in &candidates {
                    if let Some((t, n)) = intersect(piece, origin, direction)
                        && t < closest
                    {
                        closest = t;
                        hit = Some(n);
                    }
                }
                if let Some(n) = hit {
                    let color = light.shade([n[0] * c + n[2] * s, n[1], -n[0] * s + n[2] * c]);
                    let cell = &mut self.cells[(dy / 4) * cols + dx / 2];
                    // Average only covered dots; empty gaps retain the terminal background.
                    let [_, r, g, b] = color.to_be_bytes();
                    let total = &mut totals[(dy / 4) * cols + dx / 2];
                    for (sum, value) in total.iter_mut().zip([r, g, b]) {
                        *sum += u32::from(value);
                    }
                    cell.dots |= DOT_BITS[(dy % 4) * 2 + dx % 2];
                }
            }
        }
        for (cell, total) in self.cells.iter_mut().zip(totals) {
            let count = cell.dots.count_ones();
            if count > 0 {
                let [r, g, b] = total.map(|v| (v / count) as u8);
                cell.rgb = u32::from_be_bytes([0, r, g, b]);
            }
        }
        &self.cells
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn inside(p: Point, polygon: &[Point]) -> bool {
        let mut result = false;
        for (a, b) in polygon
            .iter()
            .zip(polygon.iter().cycle().skip(1))
            .take(polygon.len())
        {
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
            {
                result = !result;
            }
        }
        result
    }
    #[test]
    fn canonical_face_and_clipped_diagonals() {
        let polygons = polygons();
        assert_eq!(polygons.len(), 5);
        assert!(MESH.iter().any(|p| {
            p.planes
                .iter()
                .any(|f| f.normal[0].abs() > 0.1 && f.normal[1].abs() > 0.1)
        }));
        // Independent point-in-polygon reference: exact SVG coverage except the
        // intentional empty grid seams. Edge fragments must not become full cubes.
        for row in 0..133 {
            for col in 0..133 {
                let p = [
                    -1.0 + (f64::from(col) + 0.37) * 2.0 / 133.0,
                    -1.0 + (f64::from(row) + 0.61) * 2.0 / 133.0,
                ];
                let seam = p.iter().any(|v| {
                    let t = (v + 1.0) / PITCH;
                    let k = t.round();
                    k > 0.0 && k < 6.0 && (t - k).abs() * PITCH < GAP / 2.0
                });
                let expected = polygons.iter().any(|poly| inside(p, poly)) && !seam;
                let hit = MESH
                    .iter()
                    .any(|piece| intersect(piece, [p[0], p[1], 4.5], [0.0, 0.0, -1.0]).is_some());
                assert_eq!(hit, expected, "canonical sample {p:?}");
            }
        }
    }
    #[test]
    fn every_face_is_real_and_nearest_depth_occludes() {
        // Select an uncut cubelet rather than a clipped boundary piece.
        let piece = MESH
            .iter()
            .find(|p| {
                p.planes.len() == 14
                    && p.planes
                        .iter()
                        .take(6)
                        .all(|f| f.normal.iter().filter(|v| v.abs() > 1e-8).count() == 1)
            })
            .unwrap();
        let mut center = [0.0; 3];
        for (axis, value) in center.iter_mut().enumerate() {
            let low = piece.planes.iter().find(|p| p.normal[axis] < -0.9).unwrap();
            let high = piece.planes.iter().find(|p| p.normal[axis] > 0.9).unwrap();
            *value = (high.offset - low.offset) / 2.0;
        }
        for axis in 0..3 {
            for sign in [-1.0, 1.0] {
                let mut origin = center;
                origin[axis] += sign * 3.0;
                let mut direction = [0.0; 3];
                direction[axis] = -sign;
                let (depth, n) = intersect(piece, origin, direction).unwrap();
                assert!(depth > 2.0 && depth < 3.0);
                assert_eq!(n[axis], sign);
            }
        }
        let origin = [4.5, center[1], 0.0];
        let mut hits = MESH
            .iter()
            .filter_map(|p| intersect(p, origin, [-1.0, 0.0, 0.0]))
            .collect::<Vec<_>>();
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert!(hits.len() >= 2);
        assert!(hits[0].0 > 3.4 && hits[0].0 < 3.6);
        assert!(hits[1].0 > hits[0].0 + PITCH * 0.9);
    }
    #[test]
    fn rigid_turn_bounds_palette_and_small_resizes() {
        let dark = Lighting::terminal((210, 221, 235), (15, 20, 37));
        let light = Lighting::terminal((32, 32, 32), (250, 250, 250));
        let mut renderer = Renderer::default();
        for (cols, rows) in [(40, 14), (60, 21), (1, 1), (0, 0)] {
            let front = renderer.frame(cols, rows, 0.0, &dark).to_vec();
            assert_eq!(front, renderer.frame(cols, rows, 1.0, &dark));
            for phase in [0.0, 0.125, 0.25, 0.5, 0.75] {
                let frame = renderer.frame(cols, rows, phase, &dark).to_vec();
                assert_eq!(frame.len(), usize::from(cols) * usize::from(rows));
                assert_eq!(
                    frame.iter().map(|c| c.dots).collect::<Vec<_>>(),
                    renderer
                        .frame(cols, rows, phase, &light)
                        .iter()
                        .map(|c| c.dots)
                        .collect::<Vec<_>>()
                );
                if cols >= 40 {
                    assert!(frame.iter().any(|c| c.dots != 0));
                    assert!(frame[..usize::from(cols)].iter().all(|c| c.dots == 0));
                    assert!(
                        frame[frame.len() - usize::from(cols)..]
                            .iter()
                            .all(|c| c.dots == 0)
                    );
                }
            }
        }
        let a = Lighting::terminal((230, 230, 230), (0, 0, 0));
        assert!(a.shade([-1.0, 0.0, 0.0]) > a.shade([1.0, 0.0, 0.0]));
        assert!(a.shade([0.0, -1.0, 0.0]) > a.shade([0.0, 1.0, 0.0]));
    }
}
