//! Fixed, monochrome matte lighting for the Hanzo terminal monogram.
//!
//! The light stays in camera/world space while only the solid H rotates.
//! No per-cell random grain, moving reflections, bounce streaks or gloss.

pub(super) struct Lighting {
    background: [f64; 3],
    ink: [f64; 3],
}

impl Lighting {
    pub(super) fn terminal(fg: (u8, u8, u8), bg: (u8, u8, u8)) -> Self {
        let background = [f64::from(bg.0), f64::from(bg.1), f64::from(bg.2)];
        // Monochrome, while inheriting terminal brightness and supporting light themes.
        let value = (f64::from(fg.0) + f64::from(fg.1) + f64::from(fg.2)) / 3.0;
        Self { background, ink: [value; 3] }
    }

    pub(super) fn shade(&self, [nx, ny, nz]: [f64; 3], _depth: f64, _grain: f64) -> u32 {
        // Directional key from upper-left of screen, without a specular lobe.
        let diffuse = (nx * -0.45 + ny * 0.55 + nz * 0.70).max(0.0);
        let strength = 0.26 + 0.74 * diffuse;
        let rgb: [u8; 3] = std::array::from_fn(|i| {
            (self.background[i] + (self.ink[i] - self.background[i]) * strength)
                .round().clamp(0.0, 255.0) as u8
        });
        u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]])
    }
}
