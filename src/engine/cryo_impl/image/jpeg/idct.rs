//! The inverse discrete cosine transform, and the zigzag that feeds it.

/// The order the 64 coefficients of a block are transmitted in.
///
/// Low frequencies first, walking the diagonals, so the trailing
/// coefficients of a smooth block are zeros that run-length coding
/// collapses to almost nothing.
pub(super) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The 8-point basis: `COSINE[u][x] = cos((2x + 1) u pi / 16)`, with
/// the `1/sqrt(2)` of the zero term folded in.
///
/// Built once and shared, because a transform that rebuilt it per
/// block would spend more time on cosines than on pixels.
fn cosine_table() -> &'static [[f32; 8]; 8] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [[0.0f32; 8]; 8];
        for (u, row) in table.iter_mut().enumerate() {
            let scale = if u == 0 {
                1.0 / std::f32::consts::SQRT_2
            } else {
                1.0
            };
            for (x, cell) in row.iter_mut().enumerate() {
                let angle = (2.0 * x as f32 + 1.0) * u as f32 * std::f32::consts::PI / 16.0;
                *cell = scale * angle.cos();
            }
        }
        table
    })
}

/// The inverse DCT of one block, back to 0..=255 samples.
///
/// Separable: the 2-D transform is eight row transforms followed by
/// eight column ones, which is 1024 multiplications a block rather
/// than the 4096 a direct 2-D sum would take.
pub(super) fn idct(block: &[i32; 64]) -> [u8; 64] {
    let cosine = cosine_table();
    let mut rows = [0.0f32; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0.0;
            for u in 0..8 {
                let coefficient = block[y * 8 + u];
                if coefficient != 0 {
                    sum += cosine[u][x] * coefficient as f32;
                }
            }
            rows[y * 8 + x] = sum / 2.0;
        }
    }

    let mut out = [0u8; 64];
    for x in 0..8 {
        for y in 0..8 {
            let mut sum = 0.0;
            for v in 0..8 {
                sum += cosine[v][y] * rows[v * 8 + x];
            }
            // The samples were level-shifted down by 128 before the
            // forward transform, so they shift back up here.
            out[y * 8 + x] = (sum / 2.0 + 128.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_block_is_mid_grey() {
        // All zeros means no deviation from the level shift.
        assert_eq!(idct(&[0i32; 64]), [128u8; 64]);
    }

    #[test]
    fn a_dc_only_block_is_one_flat_tone() {
        let mut block = [0i32; 64];
        block[0] = 8 * 16;
        let samples = idct(&block);
        assert!(
            samples.iter().all(|&s| s == samples[0]),
            "got {:?}",
            &samples[..8]
        );
        assert!(samples[0] > 128, "a positive DC lifts the block");
    }

    #[test]
    fn a_negative_dc_darkens_the_whole_block() {
        let mut block = [0i32; 64];
        block[0] = -8 * 16;
        let samples = idct(&block);
        assert!(samples.iter().all(|&s| s == samples[0]));
        assert!(samples[0] < 128);
    }

    #[test]
    fn the_first_horizontal_harmonic_varies_across_the_row() {
        // Coefficient (0, 1): one half-cycle left to right, and every
        // row the same.
        let mut block = [0i32; 64];
        block[1] = 200;
        let samples = idct(&block);
        assert!(samples[0] > samples[7], "bright on the left");
        for row in 1..8 {
            assert_eq!(samples[row * 8], samples[0], "every row alike");
        }
    }

    #[test]
    fn samples_are_clamped_into_range() {
        let mut block = [0i32; 64];
        block[0] = 100_000;
        assert_eq!(idct(&block), [255u8; 64]);
        block[0] = -100_000;
        assert_eq!(idct(&block), [0u8; 64]);
    }

    #[test]
    fn the_zigzag_covers_every_slot_once() {
        let mut seen = [false; 64];
        for &slot in &ZIGZAG {
            assert!(!seen[slot], "slot {slot} appears twice");
            seen[slot] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }
}
