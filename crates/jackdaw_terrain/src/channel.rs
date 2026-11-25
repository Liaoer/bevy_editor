//! Named per-cell integer layers painted over a heightmap.
//!
//! Jackdaw stores the name, the values and the width, and never interprets
//! them. A channel holds one small number per terrain cell: a material
//! index, a biome id, walkability, a spawn weight.

use bevy_math::Vec2;

use crate::brush::compute_falloff;
use crate::tint::brush_weight;

/// Storage width a channel's values are written with on disk.
///
/// Values are held in memory as `u16` whatever the width. The width decides
/// how many bytes a channel costs in the sidecar and the largest paintable
/// value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChannelElement {
    /// One byte per cell, values `0..=255`.
    #[default]
    U8,
    /// Two bytes per cell, values `0..=65535`.
    U16,
}

impl ChannelElement {
    /// Largest value this width can hold. Painting clamps to it.
    pub fn max_value(self) -> u16 {
        match self {
            Self::U8 => u16::from(u8::MAX),
            Self::U16 => u16::MAX,
        }
    }

    /// Bytes one cell occupies on disk.
    pub fn byte_width(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
        }
    }

    /// Wire tag used by the sidecar format.
    pub fn tag(self) -> u8 {
        match self {
            Self::U8 => 0,
            Self::U16 => 1,
        }
    }

    /// Inverse of [`ChannelElement::tag`]. Returns `None` for unknown tags.
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::U8),
            1 => Some(Self::U16),
            _ => None,
        }
    }
}

/// What a terrain declares one channel to be: its name and its width.
///
/// The directory entry only. Per-cell values live in the regions, beside
/// the heights they describe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelDescriptor {
    /// Project-defined name. Opaque to jackdaw; used as the export filename.
    pub name: String,
    /// Storage width, and therefore the value ceiling.
    pub element: ChannelElement,
}

impl ChannelDescriptor {
    pub fn new(name: impl Into<String>, element: ChannelElement) -> Self {
        Self {
            name: name.into(),
            element,
        }
    }
}

/// One named layer of per-cell values, row-major, `resolution^2` long.
///
/// The inlet for reading a sidecar written as a dense grid over the whole
/// terrain. Loading slices it into the regions it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelData {
    /// Project-defined name. Opaque to jackdaw; used as the export filename.
    pub name: String,
    /// Storage width, and therefore the value ceiling.
    pub element: ChannelElement,
    /// Row-major values, length `resolution^2`.
    pub values: Vec<u16>,
}

impl ChannelData {
    /// A channel of `resolution^2` zeroes.
    pub fn new(name: impl Into<String>, element: ChannelElement, resolution: u32) -> Self {
        Self {
            name: name.into(),
            element,
            values: vec![0; (resolution as usize) * (resolution as usize)],
        }
    }

    /// Value at integer grid coordinates, or 0 out of bounds.
    ///
    /// A channel whose length does not match `resolution^2` reads 0
    /// everywhere rather than only past its end: the row stride is the
    /// caller's, so an index landing inside the stored values names a
    /// different cell than the one asked for.
    pub fn get(&self, resolution: u32, x: u32, z: u32) -> u16 {
        let side = resolution as usize;
        if x >= resolution || z >= resolution || self.values.len() != side * side {
            return 0;
        }
        self.values[z as usize * side + x as usize]
    }

    /// Grow or shrink to `resolution^2`, zero-filling any new cells.
    pub fn resize_to(&mut self, resolution: u32) {
        let want = (resolution as usize) * (resolution as usize);
        self.values.resize(want, 0);
    }
}

/// Stamp `value` into a channel under a circular brush.
///
/// Integer channels cannot blend, so the brush is a threshold stamp rather
/// than an accumulation: a cell is written when its falloff is at least
/// `threshold`, and left alone otherwise. `threshold` of 0 writes the whole
/// disc; raising it shrinks the written area inside the same radius.
///
/// `center` and `radius` are in grid cells. `value` is clamped to `element`.
/// Returns the number of cells changed, so a caller can skip an undo entry
/// for a stroke that did nothing.
pub fn apply_channel_brush(
    values: &mut [u16],
    resolution: u32,
    element: ChannelElement,
    center: Vec2,
    radius: f32,
    falloff: f32,
    threshold: f32,
    value: u16,
) -> usize {
    if resolution == 0 || radius <= 0.0 {
        return 0;
    }
    let value = value.min(element.max_value());
    let res = resolution as i32;

    let min_x = ((center.x - radius).floor() as i32).clamp(0, res - 1);
    let max_x = ((center.x + radius).ceil() as i32).clamp(0, res - 1);
    let min_z = ((center.y - radius).floor() as i32).clamp(0, res - 1);
    let max_z = ((center.y + radius).ceil() as i32).clamp(0, res - 1);

    let mut changed = 0;
    for gz in min_z..=max_z {
        for gx in min_x..=max_x {
            let dist = ((gx as f32 - center.x).powi(2) + (gz as f32 - center.y).powi(2)).sqrt();
            if compute_falloff(dist, radius, falloff) < threshold.max(f32::EPSILON) {
                continue;
            }
            let idx = (gz * res + gx) as usize;
            if values[idx] != value {
                values[idx] = value;
                changed += 1;
            }
        }
    }
    changed
}

/// Ease cells under a circular brush toward the channel's ceiling, or toward
/// zero when `erase` is set; returns the number of cells changed.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors apply_color_brush's flat parameter list"
)]
pub fn apply_density_brush(
    values: &mut [u16],
    resolution: u32,
    element: ChannelElement,
    center: Vec2,
    radius: f32,
    hardness: f32,
    falloff: f32,
    opacity: f32,
    dt: f32,
    erase: bool,
) -> usize {
    let density = if erase { 0.0 } else { 1.0 };
    apply_density_brush_to(
        values, resolution, element, center, radius, hardness, falloff, opacity, dt, erase, density,
    )
}

/// Add up to `density`, or erase down to it. Coverage already past the target
/// in the direction of the stroke stays as it is. Density is normalized to 0–1.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors apply_density_brush with an explicit coverage target"
)]
pub fn apply_density_brush_to(
    values: &mut [u16],
    resolution: u32,
    element: ChannelElement,
    center: Vec2,
    radius: f32,
    hardness: f32,
    falloff: f32,
    opacity: f32,
    dt: f32,
    erase: bool,
    density: f32,
) -> usize {
    if resolution == 0
        || !radius.is_finite()
        || radius <= 0.0
        || !opacity.is_finite()
        || opacity <= 0.0
        || !dt.is_finite()
        || dt <= 0.0
        || !density.is_finite()
    {
        return 0;
    }
    let ceiling = element.max_value();
    let target = (density.clamp(0.0, 1.0) * f32::from(ceiling)).round() as u16;
    let res = resolution as i32;

    let min_x = ((center.x - radius).floor() as i32).clamp(0, res - 1);
    let max_x = ((center.x + radius).ceil() as i32).clamp(0, res - 1);
    let min_z = ((center.y - radius).floor() as i32).clamp(0, res - 1);
    let max_z = ((center.y + radius).ceil() as i32).clamp(0, res - 1);

    let mut changed = 0;
    for gz in min_z..=max_z {
        for gx in min_x..=max_x {
            let dist = ((gx as f32 - center.x).powi(2) + (gz as f32 - center.y).powi(2)).sqrt();
            let weight = brush_weight(dist, radius, falloff, hardness);
            if weight <= 0.0 {
                continue;
            }
            let idx = (gz * res + gx) as usize;
            let Some(before) = values.get(idx).copied() else {
                continue;
            };
            let after = density_cell(before, ceiling, target, erase, opacity * weight * dt);
            if after != values[idx] {
                values[idx] = after;
                changed += 1;
            }
        }
    }
    changed
}

/// Parameters of one update of a Detail stroke.
pub struct DensityBrush {
    pub radius: f32,
    pub hardness: f32,
    pub falloff: f32,
    pub opacity: f32,
    pub dt: f32,
    pub erase: bool,
    pub density: f32,
}

impl DensityBrush {
    /// Paint the disc swept between two in-terrain samples. Each cell gets
    /// one update per frame, so widely spaced samples cannot stack opacity.
    pub fn sweep(
        &self,
        resolution: u32,
        element: ChannelElement,
        from: Vec2,
        to: Vec2,
        read: impl Fn(u32, u32) -> u16,
    ) -> Option<(crate::GridRect, Vec<u16>)> {
        let strength = self.opacity * self.dt;
        if !strength.is_finite() || strength <= 0.0 || !self.density.is_finite() {
            return None;
        }
        let ceiling = element.max_value();
        let target = (self.density.clamp(0.0, 1.0) * f32::from(ceiling)).round() as u16;
        self.sweep_values(resolution, from, to, read, |_, _, before, weight| {
            density_cell(before, ceiling, target, self.erase, strength * weight)
        })
    }

    /// Blend a stored height reduction or an inverse RGB565 tint along a
    /// Detail stroke. The default zero value keeps full height and white tint.
    pub fn sweep_appearance(
        &self,
        resolution: u32,
        target: DetailAppearanceValue,
        from: Vec2,
        to: Vec2,
        read: impl Fn(u32, u32) -> u16,
    ) -> Option<(crate::GridRect, Vec<u16>)> {
        let strength = self.opacity * self.dt;
        if !strength.is_finite() || strength <= 0.0 {
            return None;
        }
        self.sweep_values(resolution, from, to, read, |_, _, before, weight| {
            let t = (strength * weight).clamp(0.0, 1.0);
            match target {
                DetailAppearanceValue::Height(percent) => {
                    step_toward(before.min(255), u16::from(255 - percent), t)
                }
                DetailAppearanceValue::Color(rgb) => {
                    let current = unpack_tint(before);
                    let target = unpack_tint(encode_detail_tint(rgb));
                    pack_tint(std::array::from_fn(|axis| {
                        step_toward(current[axis], target[axis], t)
                    }))
                }
            }
        })
    }

    /// Place a categorical type along the stroke. A stable per-cell threshold
    /// feathers the edge without creating intermediate type IDs.
    pub fn sweep_species(
        &self,
        resolution: u32,
        species: u8,
        from: Vec2,
        to: Vec2,
        read: impl Fn(u32, u32) -> u16,
    ) -> Option<(crate::GridRect, Vec<u16>)> {
        if !self.opacity.is_finite() || self.opacity <= 0.0 {
            return None;
        }
        self.sweep_values(resolution, from, to, read, |x, z, before, weight| {
            let mut hash = x.wrapping_mul(0x9e37_79b9) ^ z.wrapping_mul(0x85eb_ca6b);
            hash ^= hash >> 16;
            hash = hash.wrapping_mul(0x7feb_352d);
            hash ^= hash >> 15;
            let threshold = (hash >> 8) as f32 / 16_777_216.0;
            if threshold < self.opacity * weight {
                u16::from(species)
            } else {
                before
            }
        })
    }

    fn sweep_values(
        &self,
        resolution: u32,
        from: Vec2,
        to: Vec2,
        read: impl Fn(u32, u32) -> u16,
        blend: impl Fn(u32, u32, u16, f32) -> u16,
    ) -> Option<(crate::GridRect, Vec<u16>)> {
        let rect = crate::GridRect::brush(resolution, from, self.radius)?
            .union(crate::GridRect::brush(resolution, to, self.radius)?);
        let segment = to - from;
        let inverse = 1.0 / segment.length_squared().max(f32::EPSILON);
        let mut values = Vec::with_capacity(rect.cells());
        let mut changed = false;
        for z in rect.z..rect.z + rect.height {
            for x in rect.x..rect.x + rect.width {
                let offset = Vec2::new(x as f32, z as f32) - from;
                let t = (offset.dot(segment) * inverse).clamp(0.0, 1.0);
                let distance = (offset - t * segment).length();
                let weight = brush_weight(distance, self.radius, self.falloff, self.hardness);
                let before = read(x, z);
                let after = if weight > 0.0 {
                    blend(x, z, before, weight)
                } else {
                    before
                };
                changed |= before != after;
                values.push(after);
            }
        }
        changed.then_some((rect, values))
    }
}

#[derive(Clone, Copy)]
pub enum DetailAppearanceValue {
    /// Blade-length multiplier in the range 0–255.
    Height(u8),
    /// RGB multiplier encoded in sRGB.
    Color([u8; 3]),
}

const RGB565_MAX: [u16; 3] = [31, 63, 31];

fn unpack_tint(value: u16) -> [u16; 3] {
    [(value >> 11) & 31, (value >> 5) & 63, value & 31]
}

fn pack_tint(rgb: [u16; 3]) -> u16 {
    rgb[0] << 11 | rgb[1] << 5 | rgb[2]
}

pub fn encode_detail_tint(rgb: [u8; 3]) -> u16 {
    pack_tint(std::array::from_fn(|axis| {
        ((255 - u16::from(rgb[axis])) * RGB565_MAX[axis] + 127) / 255
    }))
}

pub fn decode_detail_tint(value: u16) -> [u8; 3] {
    let rgb = unpack_tint(value);
    std::array::from_fn(|axis| {
        (255 - (rgb[axis] * 255 + RGB565_MAX[axis] / 2) / RGB565_MAX[axis]) as u8
    })
}

fn density_cell(before: u16, ceiling: u16, target: u16, erase: bool, strength: f32) -> u16 {
    let value = before.min(ceiling);
    if (erase && value < target) || (!erase && value > target) {
        before
    } else {
        step_toward(value, target, strength.clamp(0.0, 1.0))
    }
}

/// One cell eased `t` of the way from `from` to `to`, never passing it, and
/// never standing still while `t` is positive.
fn step_toward(from: u16, to: u16, t: f32) -> u16 {
    if from == to || !t.is_finite() || t <= 0.0 {
        return from;
    }
    let delta = (f32::from(to) - f32::from(from)) * t;
    let step = if delta > 0.0 {
        delta.max(1.0)
    } else {
        delta.min(-1.0)
    };
    let next = (f32::from(from) + step)
        .round()
        .clamp(0.0, f32::from(u16::MAX)) as u16;
    if to > from {
        next.min(to)
    } else {
        next.max(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grass_appearance_keeps_neutral_zero_and_blends_color_components_independently() {
        assert_eq!(encode_detail_tint([255; 3]), 0);
        assert_eq!(decode_detail_tint(0), [255; 3]);
        assert_eq!(
            decode_detail_tint(encode_detail_tint([255, 0, 0])),
            [255, 0, 0]
        );
        let brush = DensityBrush {
            radius: 2.0,
            hardness: 1.0,
            falloff: 2.0,
            opacity: 1.0,
            dt: 0.5,
            erase: false,
            density: 1.0,
        };
        let at = Vec2::splat(3.0);
        let (rect, patch) = brush
            .sweep_appearance(
                8,
                DetailAppearanceValue::Color([255, 0, 0]),
                at,
                at,
                |_, _| 0,
            )
            .unwrap();
        let center = ((3 - rect.z) * rect.width + 3 - rect.x) as usize;
        let [r, g, b] = decode_detail_tint(patch[center]);
        assert_eq!(r, 255);
        assert!((120..=135).contains(&g) && (120..=135).contains(&b));
    }
    /// Every read of a mismatched channel is 0, including the ones whose
    /// index lands inside the stored values: at the caller's stride those
    /// name a different cell.
    #[test]
    fn a_resolution_the_stored_values_do_not_match_reads_zero() {
        let mut channel = ChannelData::new("mask", ChannelElement::U16, 4);
        channel.values[5] = 9;
        assert_eq!(channel.get(4, 1, 1), 9);
        assert_eq!(channel.get(8, 7, 7), 0);
        assert_eq!(channel.get(8, 1, 1), 0);
        assert_eq!(channel.get(2, 1, 1), 0);
    }

    #[test]
    fn element_widths_and_ceilings() {
        assert_eq!(ChannelElement::U8.byte_width(), 1);
        assert_eq!(ChannelElement::U8.max_value(), 255);
        assert_eq!(ChannelElement::U16.byte_width(), 2);
        assert_eq!(ChannelElement::U16.max_value(), 65535);
    }

    #[test]
    fn element_tags_round_trip() {
        for e in [ChannelElement::U8, ChannelElement::U16] {
            assert_eq!(ChannelElement::from_tag(e.tag()), Some(e));
        }
        assert_eq!(ChannelElement::from_tag(7), None);
    }

    #[test]
    fn new_channel_is_zeroed_at_the_right_length() {
        let c = ChannelData::new("biome", ChannelElement::U8, 8);
        assert_eq!(c.values.len(), 64);
        assert!(c.values.iter().all(|v| *v == 0));
    }

    #[test]
    fn resize_zero_fills_new_cells_and_keeps_old_ones() {
        let mut c = ChannelData::new("m", ChannelElement::U16, 2);
        c.values = vec![1, 2, 3, 4];
        c.resize_to(3);
        assert_eq!(c.values.len(), 9);
        assert_eq!(&c.values[..4], &[1, 2, 3, 4]);
        assert!(c.values[4..].iter().all(|v| *v == 0));
    }

    #[test]
    fn brush_stamps_a_disc_and_reports_changed_cells() {
        let mut values = vec![0u16; 100];
        let changed = apply_channel_brush(
            &mut values,
            10,
            ChannelElement::U8,
            Vec2::new(5.0, 5.0),
            2.0,
            1.0,
            0.0,
            3,
        );
        assert!(changed > 0);
        assert_eq!(values[5 * 10 + 5], 3);
        // A cell well outside the radius is untouched.
        assert_eq!(values[0], 0);
        // Painting the same value again changes nothing.
        let again = apply_channel_brush(
            &mut values,
            10,
            ChannelElement::U8,
            Vec2::new(5.0, 5.0),
            2.0,
            1.0,
            0.0,
            3,
        );
        assert_eq!(again, 0);
    }

    #[test]
    fn brush_clamps_value_to_the_element_ceiling() {
        let mut values = vec![0u16; 25];
        apply_channel_brush(
            &mut values,
            5,
            ChannelElement::U8,
            Vec2::new(2.0, 2.0),
            1.5,
            1.0,
            0.0,
            9000,
        );
        assert_eq!(values[2 * 5 + 2], 255);
    }

    #[test]
    fn brush_at_the_edge_does_not_wrap_or_panic() {
        let mut values = vec![0u16; 25];
        apply_channel_brush(
            &mut values,
            5,
            ChannelElement::U16,
            Vec2::new(0.0, 0.0),
            3.0,
            1.0,
            0.0,
            42,
        );
        assert_eq!(values[0], 42);
        // Nothing wrapped onto the opposite edge of row 0.
        assert_eq!(values[4], 0);
    }

    #[test]
    fn brush_outside_the_terrain_is_a_no_op() {
        let mut values = vec![0u16; 25];
        let changed = apply_channel_brush(
            &mut values,
            5,
            ChannelElement::U8,
            Vec2::new(50.0, 50.0),
            2.0,
            1.0,
            0.0,
            1,
        );
        assert_eq!(changed, 0);
        assert!(values.iter().all(|v| *v == 0));
    }

    #[test]
    fn a_high_threshold_writes_fewer_cells_than_a_low_one() {
        let mut soft = vec![0u16; 400];
        let wide = apply_channel_brush(
            &mut soft,
            20,
            ChannelElement::U8,
            Vec2::new(10.0, 10.0),
            6.0,
            1.0,
            0.0,
            1,
        );
        let mut hard = vec![0u16; 400];
        let narrow = apply_channel_brush(
            &mut hard,
            20,
            ChannelElement::U8,
            Vec2::new(10.0, 10.0),
            6.0,
            1.0,
            0.7,
            1,
        );
        assert!(narrow < wide, "narrow {narrow} should be under wide {wide}");
        assert!(narrow > 0);
    }

    #[test]
    fn a_density_stamp_accumulates_toward_the_channel_ceiling() {
        let mut values = vec![0u16; 100];
        let mut last = 0;
        for _ in 0..4 {
            apply_density_brush(
                &mut values,
                10,
                ChannelElement::U8,
                Vec2::new(5.0, 5.0),
                3.0,
                0.5,
                1.0,
                2.0,
                1.0 / 60.0,
                false,
            );
            let now = values[5 * 10 + 5];
            assert!(now > last, "density {now} should rise above {last}");
            last = now;
        }
        for _ in 0..2000 {
            apply_density_brush(
                &mut values,
                10,
                ChannelElement::U8,
                Vec2::new(5.0, 5.0),
                3.0,
                0.5,
                1.0,
                2.0,
                1.0 / 60.0,
                false,
            );
        }
        assert_eq!(values[5 * 10 + 5], ChannelElement::U8.max_value());
        assert_eq!(values[0], 0, "a cell outside the radius is untouched");
    }

    #[test]
    fn erasing_density_takes_a_cell_back_to_nothing() {
        let mut values = vec![200u16; 100];
        apply_density_brush(
            &mut values,
            10,
            ChannelElement::U8,
            Vec2::new(5.0, 5.0),
            3.0,
            0.5,
            1.0,
            2.0,
            1.0 / 60.0,
            true,
        );
        assert!(values[5 * 10 + 5] < 200);
        for _ in 0..2000 {
            apply_density_brush(
                &mut values,
                10,
                ChannelElement::U8,
                Vec2::new(5.0, 5.0),
                3.0,
                0.5,
                1.0,
                2.0,
                1.0 / 60.0,
                true,
            );
        }
        assert_eq!(values[5 * 10 + 5], 0);
    }

    #[test]
    fn the_density_hardness_plateau_paints_flat() {
        let mut values = vec![0u16; 1600];
        apply_density_brush(
            &mut values,
            40,
            ChannelElement::U16,
            Vec2::new(20.0, 20.0),
            10.0,
            0.6,
            1.0,
            0.5,
            1.0 / 60.0,
            false,
        );
        let centre = values[20 * 40 + 20];
        assert!(centre > 0);
        let outermost_plateau_cell = 5;
        for offset in 1..=outermost_plateau_cell {
            assert_eq!(
                values[20 * 40 + 20 + offset],
                centre,
                "cell {offset} inside the plateau matches the centre"
            );
        }
        let beyond_the_plateau = outermost_plateau_cell + 4;
        assert!(
            values[20 * 40 + 20 + beyond_the_plateau] < centre,
            "the ring beyond the plateau has not fallen off"
        );
    }

    #[test]
    fn density_targets_add_or_remove_only_in_the_brush_direction() {
        for element in [ChannelElement::U8, ChannelElement::U16] {
            let full = element.max_value();
            let target = (0.4 * f32::from(full)).round() as u16;
            let mut cells = [0, full, 0, full];
            for erase in [false, true] {
                let changed = apply_density_brush_to(
                    &mut cells,
                    2,
                    element,
                    Vec2::ZERO,
                    4.0,
                    1.0,
                    1.0,
                    1.0,
                    1.0,
                    erase,
                    0.4,
                );
                assert_eq!(changed, 2);
                assert_eq!(
                    cells,
                    if erase {
                        [target; 4]
                    } else {
                        [target, full, target, full]
                    }
                );
            }
            assert_eq!(
                apply_density_brush_to(
                    &mut cells,
                    2,
                    element,
                    Vec2::ZERO,
                    4.0,
                    1.0,
                    1.0,
                    1.0,
                    1.0,
                    false,
                    0.4,
                ),
                0,
            );
        }
    }

    #[test]
    fn density_sweep_joined_distant_samples_without_repainting_outside() {
        let brush = DensityBrush {
            radius: 2.0,
            hardness: 0.5,
            falloff: 2.0,
            opacity: 1.0,
            dt: 1.0,
            erase: true,
            density: 0.2,
        };
        let (rect, patch) = brush
            .sweep(
                32,
                ChannelElement::U8,
                Vec2::new(3.0, 12.0),
                Vec2::new(25.0, 12.0),
                |_, _| u8::MAX.into(),
            )
            .expect("sweep changes coverage");
        let mut values = vec![255; 32 * 32];
        rect.write(&mut values, 32, &patch);
        assert!(
            values[12 * 32 + 3..=12 * 32 + 25]
                .iter()
                .all(|&value| value == 51)
        );
        assert!(values[9 * 32..10 * 32].iter().all(|&value| value == 255));
        assert!(
            brush
                .sweep(32, ChannelElement::U8, Vec2::ZERO, Vec2::X, |_, _| 51)
                .is_none()
        );
    }

    #[test]
    fn a_density_brush_outside_the_terrain_changes_nothing() {
        let mut values = vec![0u16; 25];
        let changed = apply_density_brush(
            &mut values,
            5,
            ChannelElement::U8,
            Vec2::new(50.0, 50.0),
            2.0,
            0.5,
            1.0,
            1.0,
            1.0 / 60.0,
            false,
        );
        assert_eq!(changed, 0);
        assert!(values.iter().all(|v| *v == 0));
    }
}
