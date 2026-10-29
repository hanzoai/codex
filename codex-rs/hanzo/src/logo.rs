//! Shared terminal cubelet mesh, clipped analytically to the canonical mark.
//! Each convex prism has front/back caps and every clipped side face. Rays select
//! the nearest face across all pieces; seams are empty space, never painted lines.
use std::f64::consts::TAU;
use std::sync::LazyLock;

pub const MAX_COLUMNS: u16 = 60;
pub const MAX_ROWS: u16 = 24;
pub const LOOP_SECONDS: f64 = 7.2;
/// Small, rigid mouse-controlled angles in radians; independent of the idle spin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tilt {
    pub x: f64,
    pub y: f64,
}

/// Shared pointer response for the welcome screen and native idle player.
#[derive(Default)]
pub struct MouseTilt {
    current: Tilt,
    target: Tilt,
}
impl MouseTilt {
    pub fn point(&mut self, horizontal: f64, vertical: f64) {
        self.target = Tilt {
            x: -vertical.clamp(-1.0, 1.0) * 0.18,
            y: horizontal.clamp(-1.0, 1.0) * 0.22,
        };
    }

    pub fn advance(&mut self, elapsed: std::time::Duration) -> Tilt {
        // Exponential easing is independent of pointer event/frame frequency.
        let weight = 1.0 - (-elapsed.as_secs_f64() / 0.22).exp();
        self.current.x += (self.target.x - self.current.x) * weight;
        self.current.y += (self.target.y - self.current.y) * weight;
        self.current
    }
}

const PITCH: f64 = 2.0 / 6.0;
const GAP: f64 = 0.02;
const BEVEL: f64 = 0.012;
const CAMERA_DISTANCE: f64 = 4.8;
const DOT_THRESHOLDS: [f64; 8] = [
    0.0625, 0.5625, 0.8125, 0.3125, 0.4375, 0.9375, 0.6875, 0.1875,
];
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
                            let offset = (a.offset + b.offset - BEVEL) / std::f64::consts::SQRT_2;
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

fn camera_ray(x: f64, y: f64, sin_y: f64, cos_y: f64) -> ([f64; 3], [f64; 3]) {
    let front_distance = CAMERA_DISTANCE - (PITCH - GAP) / 2.0;
    let world_direction = [x / front_distance, y / front_distance, -1.0];
    (
        [-sin_y * CAMERA_DISTANCE, 0.0, cos_y * CAMERA_DISTANCE],
        [
            cos_y * world_direction[0] - sin_y * world_direction[2],
            world_direction[1],
            sin_y * world_direction[0] + cos_y * world_direction[2],
        ],
    )
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
    fn strength(n: [f64; 3]) -> f64 {
        0.13 + 0.87 * dot(n, [-0.25, -0.80, 0.55]).max(0.0)
    }
    fn shade(&self, n: [f64; 3]) -> u32 {
        // SVG Y points down: the fixed key light is above and left.
        let strength = 0.25 + 0.75 * Self::strength(n);
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
    shade: u8,
    edge_dots: u8,
}
impl Cell {
    /// Surface density carries lighting even with NO_COLOR or an unknown palette.
    /// Partial cells retain sub-cell silhouettes rather than filling their gaps.
    pub fn glyph(&self) -> char {
        if self.dots == 255 {
            match self.shade {
                0..=5 => '░',
                6..=12 => '▒',
                13..=18 => '▓',
                _ => '█',
            }
        } else if self.edge_dots == 0 {
            ' '
        } else {
            char::from_u32(0x2800 + u32::from(self.edge_dots)).unwrap_or(' ')
        }
    }
}
#[derive(Default)]
pub struct Renderer {
    cells: Vec<Cell>,
}
impl Renderer {
    pub fn frame(&mut self, columns: u16, rows: u16, phase: f64, light: &Lighting) -> &[Cell] {
        self.frame_with_tilt(columns, rows, phase, Tilt::default(), light)
    }

    pub fn frame_with_tilt(
        &mut self,
        columns: u16,
        rows: u16,
        phase: f64,
        tilt: Tilt,
        light: &Lighting,
    ) -> &[Cell] {
        assert!(columns <= MAX_COLUMNS && rows <= MAX_ROWS);
        let (cols, rows) = (usize::from(columns), usize::from(rows));
        self.cells.resize(cols * rows, Cell::default());
        self.cells.fill(Cell::default());
        let mut strengths = vec![0.0; cols * rows];
        let mut totals = vec![[0_u32; 3]; cols * rows];
        if cols == 0 || rows == 0 {
            return &self.cells;
        }
        // Equal world scale on both axes, including narrow terminal resizes.
        let pitch = (2.9 / (rows * 4) as f64).max(2.9 / (cols * 2) as f64);
        let (s, c) = (phase.rem_euclid(1.0) * TAU + tilt.y.clamp(-0.22, 0.22)).sin_cos();
        let (sx, cx) = tilt.x.clamp(-0.18, 0.18).sin_cos();
        // A fixed perspective camera reveals top/bottom faces as well as sides.
        // Calibrate the front cap plane to the canonical face-on XY coordinates.
        let front_distance = CAMERA_DISTANCE - (PITCH - GAP) / 2.0;
        for dy in 0..rows * 4 {
            let y = (dy as f64 + 0.5 - (rows * 2) as f64) * pitch;
            // Perspective rays change Y with depth; bound every depth along the
            // sculpture before selecting candidate cubelets for this scanline.
            let y0 = y * (CAMERA_DISTANCE - 1.1) / front_distance;
            let y1 = y * (CAMERA_DISTANCE + 1.1) / front_distance;
            let candidates: Vec<_> = MESH
                .iter()
                .filter(|p| {
                    let margin = sx.abs() * 1.6;
                    p.high_y >= cx * y0.min(y1) - margin && p.low_y <= cx * y0.max(y1) + margin
                })
                .collect();
            for dx in 0..cols * 2 {
                let x = (dx as f64 + 0.5 - cols as f64) * pitch;
                let (origin, direction) = camera_ray(x, y, s, c);
                let inverse_x = |v: [f64; 3]| [v[0], cx * v[1] + sx * v[2], -sx * v[1] + cx * v[2]];
                let (origin, direction) = (inverse_x(origin), inverse_x(direction));
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
                    let n = [n[0], cx * n[1] - sx * n[2], sx * n[1] + cx * n[2]];
                    let normal = [n[0] * c + n[2] * s, n[1], -n[0] * s + n[2] * c];
                    let color = light.shade(normal);
                    let strength = Lighting::strength(normal);
                    strengths[(dy / 4) * cols + dx / 2] += strength;
                    let cell = &mut self.cells[(dy / 4) * cols + dx / 2];
                    // Average only covered dots; empty gaps retain the terminal background.
                    let [_, r, g, b] = color.to_be_bytes();
                    let total = &mut totals[(dy / 4) * cols + dx / 2];
                    for (sum, value) in total.iter_mut().zip([r, g, b]) {
                        *sum += u32::from(value);
                    }
                    let dot = (dy % 4) * 2 + dx % 2;
                    cell.dots |= DOT_BITS[dot];
                    if strength >= DOT_THRESHOLDS[dot] {
                        cell.edge_dots |= DOT_BITS[dot];
                    }
                }
            }
        }
        for ((cell, total), strength) in self.cells.iter_mut().zip(totals).zip(strengths) {
            let count = cell.dots.count_ones();
            if count > 0 {
                let [r, g, b] = total.map(|v| (v / count) as u8);
                cell.rgb = u32::from_be_bytes([0, r, g, b]);
                cell.shade = (strength / f64::from(count) * 23.0).round() as u8;
            }
        }
        &self.cells
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn mouse_tilt_is_bounded_eased_and_frame_rate_independent() {
        use std::time::Duration;
        let mut slow = MouseTilt::default();
        let mut fast = MouseTilt::default();
        slow.point(100.0, -100.0);
        fast.point(1.0, -1.0);
        let first = slow.advance(Duration::from_millis(20));
        assert!(first.x > 0.0 && first.x < 0.18);
        slow.advance(Duration::from_millis(980));
        for _ in 0..50 {
            fast.advance(Duration::from_millis(20));
        }
        assert!((slow.current.x - fast.current.x).abs() < 1e-12);
        assert!((slow.current.y - fast.current.y).abs() < 1e-12);
        let mut renderer = Renderer::default();
        let light = Lighting::terminal((230, 230, 230), (0, 0, 0));
        for (width, height) in [(40, 14), (56, 19), (60, 21)] {
            for x in [-0.18, 0.18] {
                for y in [-0.22, 0.22] {
                    for step in 0..72 {
                        let cells = renderer.frame_with_tilt(
                            width,
                            height,
                            f64::from(step) / 72.0,
                            Tilt { x, y },
                            &light,
                        );
                        let w = usize::from(width);
                        assert!(
                            cells[..w]
                                .iter()
                                .chain(&cells[cells.len() - w..])
                                .all(|c| c.dots == 0)
                        );
                        assert!(
                            cells
                                .chunks(w)
                                .all(|row| row[0].dots == 0 && row[w - 1].dots == 0)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn monochrome_keeps_lit_faces_distinct_without_changing_geometry() {
        let mut renderer = Renderer::default();
        let dark = Lighting::terminal((230, 230, 230), (0, 0, 0));
        let light = Lighting::terminal((32, 32, 32), (250, 250, 250));
        let a = renderer.frame(60, 21, 0.125, &dark).to_vec();
        let b = renderer.frame(60, 21, 0.125, &light);
        assert_eq!(
            a.iter().map(Cell::glyph).collect::<String>(),
            b.iter().map(Cell::glyph).collect::<String>()
        );
        let glyphs: String = a.iter().map(Cell::glyph).collect();
        assert!(glyphs.contains('█'));
        assert!(glyphs.contains('▒'));
        assert!(a.iter().filter(|c| c.dots == 0).all(|c| c.glyph() == ' '));
        assert!(
            a.iter()
                .filter(|c| c.dots != 255)
                .all(|c| c.edge_dots & !c.dots == 0)
        );
    }

    #[test]
    fn fixed_perspective_reveals_three_cubical_faces() {
        let mut faces = [false; 3];
        for row in 0..80 {
            for col in 0..80 {
                let (origin, direction) = camera_ray(
                    (f64::from(col) + 0.5) / 40.0 - 1.0,
                    (f64::from(row) + 0.5) / 40.0 - 1.0,
                    0.0,
                    1.0,
                );
                if let Some((_, n)) = MESH
                    .iter()
                    .filter_map(|piece| intersect(piece, origin, direction))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                {
                    for axis in 0..3 {
                        faces[axis] |= n[axis].abs() > 0.99;
                    }
                }
            }
        }
        assert!(
            faces.into_iter().all(|visible| visible),
            "front, side and top/bottom faces must all be visible"
        );
        // Only the object's inverse Y transform changes: the camera and rays
        // remain fixed in world space for every pose.
        for step in 0..72 {
            let (s, c) = (f64::from(step) / 72.0 * TAU).sin_cos();
            let (o, d) = camera_ray(0.6, -0.7, s, c);
            let world_o = [c * o[0] + s * o[2], o[1], -s * o[0] + c * o[2]];
            let world_d = [c * d[0] + s * d[2], d[1], -s * d[0] + c * d[2]];
            assert!(world_o[0].abs() < 1e-9 && world_o[1].abs() < 1e-9);
            assert!((world_o[2] - CAMERA_DISTANCE).abs() < 1e-9);
            let distance = CAMERA_DISTANCE - (PITCH - GAP) / 2.0;
            for (a, b) in world_d
                .into_iter()
                .zip([0.6 / distance, -0.7 / distance, -1.0])
            {
                assert!((a - b).abs() < 1e-9);
            }
        }
    }

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
            for phase in (0..72).map(|step| f64::from(step) / 72.0) {
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
