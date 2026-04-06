use std::sync::LazyLock;

use egui::Color32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Colormap {
    Grays,
    Fire,
    RedHot,
    Green,
    CyanHot,
    MagentaHot,
    Ice,
    BlueWhiteRed,
}

impl Colormap {
    pub const ALL: [Self; 8] = [
        Self::Grays,
        Self::Fire,
        Self::RedHot,
        Self::Green,
        Self::CyanHot,
        Self::MagentaHot,
        Self::Ice,
        Self::BlueWhiteRed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Grays => "Grays",
            Self::Fire => "Fire",
            Self::RedHot => "Red Hot",
            Self::Green => "Green",
            Self::CyanHot => "Cyan Hot",
            Self::MagentaHot => "Magenta Hot",
            Self::Ice => "Ice",
            Self::BlueWhiteRed => "Blue White Red",
        }
    }

    pub fn lookup(self, value: f32) -> Color32 {
        self.table()[lookup_index(value)]
    }

    pub(crate) fn table(self) -> &'static [Color32; 256] {
        match self {
            Self::Grays => &GRAYS_LUT,
            Self::Fire => &FIRE_LUT,
            Self::RedHot => &RED_HOT_LUT,
            Self::Green => &GREEN_LUT,
            Self::CyanHot => &CYAN_HOT_LUT,
            Self::MagentaHot => &MAGENTA_HOT_LUT,
            Self::Ice => &ICE_LUT,
            Self::BlueWhiteRed => &BLUE_WHITE_RED_LUT,
        }
    }

    pub(crate) fn index(self) -> u32 {
        match self {
            Self::Grays => 0,
            Self::Fire => 1,
            Self::RedHot => 2,
            Self::Green => 3,
            Self::CyanHot => 4,
            Self::MagentaHot => 5,
            Self::Ice => 6,
            Self::BlueWhiteRed => 7,
        }
    }
}

static GRAYS_LUT: LazyLock<[Color32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let channel = index as u8;
        Color32::from_rgb(channel, channel, channel)
    })
});

static FIRE_LUT: LazyLock<[Color32; 256]> =
    LazyLock::new(|| interpolated_lut(FIRE_RED, FIRE_GREEN, FIRE_BLUE));

static RED_HOT_LUT: LazyLock<[Color32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let index = index as u8;
        Color32::from_rgb(
            ramp(index, 0, 88),
            ramp(index, 86, 168),
            ramp(index, 165, 255),
        )
    })
});

static GREEN_LUT: LazyLock<[Color32; 256]> =
    LazyLock::new(|| std::array::from_fn(|index| Color32::from_rgb(0, index as u8, 0)));

static CYAN_HOT_LUT: LazyLock<[Color32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let index = index as u8;
        Color32::from_rgb(
            ramp(index, 170, 255),
            ramp(index, 84, 169),
            ramp(index, 0, 83),
        )
    })
});

static MAGENTA_HOT_LUT: LazyLock<[Color32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let index = index as u8;
        let primary = ramp(index, 0, 127);
        Color32::from_rgb(primary, ramp(index, 128, 255), primary)
    })
});

static ICE_LUT: LazyLock<[Color32; 256]> =
    LazyLock::new(|| interpolated_lut(ICE_RED, ICE_GREEN, ICE_BLUE));

static BLUE_WHITE_RED_LUT: LazyLock<[Color32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        if index < 128 {
            let t = index as f32 / 127.0;
            Color32::from_rgb(lerp_u8(0, 255, t), lerp_u8(0, 255, t), 255)
        } else {
            let t = (index - 128) as f32 / 127.0;
            Color32::from_rgb(255, lerp_u8(255, 0, t), lerp_u8(255, 0, t))
        }
    })
});

const FIRE_RED: [u8; 32] = [
    0, 0, 1, 25, 49, 73, 98, 122, 146, 162, 173, 184, 195, 207, 217, 229, 240, 252, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
];
const FIRE_GREEN: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14, 35, 57, 79, 101, 117, 133, 147, 161, 175, 190, 205,
    219, 234, 248, 255, 255, 255, 255,
];
const FIRE_BLUE: [u8; 32] = [
    0, 61, 96, 130, 165, 192, 220, 227, 210, 181, 151, 122, 93, 64, 35, 5, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 35, 98, 160, 223, 255,
];

const ICE_RED: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 19, 29, 50, 48, 79, 112, 134, 158, 186, 201, 217, 229, 242, 250, 250, 250,
    250, 251, 250, 250, 250, 250, 251, 251, 243, 230,
];
const ICE_GREEN: [u8; 32] = [
    156, 165, 176, 184, 190, 196, 193, 184, 171, 162, 146, 125, 107, 93, 81, 87, 92, 97, 95, 93,
    93, 90, 85, 69, 64, 54, 47, 35, 19, 0, 4, 0,
];
const ICE_BLUE: [u8; 32] = [
    140, 147, 158, 166, 170, 176, 209, 220, 234, 225, 236, 246, 250, 251, 250, 250, 245, 230, 230,
    222, 202, 180, 163, 142, 123, 114, 106, 94, 84, 64, 26, 27,
];

fn lookup_index(value: f32) -> usize {
    (value.clamp(0.0, 1.0) * 255.0).round() as usize
}

fn interpolated_lut(red: [u8; 32], green: [u8; 32], blue: [u8; 32]) -> [Color32; 256] {
    const SCALE: f32 = 32.0 / 256.0;
    std::array::from_fn(|index| {
        let position = index as f32 * SCALE;
        let i1 = position.floor() as usize;
        let i2 = (i1 + 1).min(31);
        let frac = position - i1 as f32;
        Color32::from_rgb(
            lerp_u8(red[i1], red[i2], frac),
            lerp_u8(green[i1], green[i2], frac),
            lerp_u8(blue[i1], blue[i2], frac),
        )
    })
}

fn lerp_u8(start: u8, end: u8, frac: f32) -> u8 {
    let start = f32::from(start);
    let end = f32::from(end);
    (start + (end - start) * frac).round().clamp(0.0, 255.0) as u8
}

fn ramp(index: u8, start: u8, end: u8) -> u8 {
    if index <= start {
        return 0;
    }
    if index >= end {
        return 255;
    }

    let span = (f32::from(end) - f32::from(start)).max(1.0);
    (((f32::from(index) - f32::from(start)) / span) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::{Colormap, BLUE_WHITE_RED_LUT, FIRE_LUT, ICE_LUT, MAGENTA_HOT_LUT, RED_HOT_LUT};
    use egui::Color32;

    #[test]
    fn lookup_clamps_out_of_range_values() {
        assert_eq!(Colormap::Fire.lookup(-1.0), Colormap::Fire.lookup(0.0));
        assert_eq!(Colormap::Fire.lookup(2.0), Colormap::Fire.lookup(1.0));
    }

    #[test]
    fn fire_matches_imagej_endpoints() {
        assert_eq!(FIRE_LUT[0], Color32::from_rgb(0, 0, 0));
        assert_eq!(FIRE_LUT[255], Color32::from_rgb(255, 255, 255));
    }

    #[test]
    fn ice_matches_reference_endpoints() {
        assert_eq!(ICE_LUT[0], Color32::from_rgb(0, 156, 140));
        assert_eq!(ICE_LUT[255], Color32::from_rgb(230, 0, 27));
    }

    #[test]
    fn red_hot_hits_expected_phase_plateaus() {
        assert_eq!(RED_HOT_LUT[0], Color32::from_rgb(0, 0, 0));
        assert_eq!(RED_HOT_LUT[88], Color32::from_rgb(255, 6, 0));
        assert_eq!(RED_HOT_LUT[168], Color32::from_rgb(255, 255, 9));
        assert_eq!(RED_HOT_LUT[255], Color32::from_rgb(255, 255, 255));
    }

    #[test]
    fn magenta_hot_keeps_red_and_blue_in_lockstep() {
        assert_eq!(MAGENTA_HOT_LUT[0], Color32::from_rgb(0, 0, 0));
        assert_eq!(MAGENTA_HOT_LUT[127], Color32::from_rgb(255, 0, 255));
        assert_eq!(MAGENTA_HOT_LUT[255], Color32::from_rgb(255, 255, 255));
    }

    #[test]
    fn blue_white_red_matches_diverging_endpoints() {
        assert_eq!(BLUE_WHITE_RED_LUT[0], Color32::from_rgb(0, 0, 255));
        assert_eq!(BLUE_WHITE_RED_LUT[127], Color32::from_rgb(255, 255, 255));
        assert_eq!(BLUE_WHITE_RED_LUT[255], Color32::from_rgb(255, 0, 0));
    }
}
