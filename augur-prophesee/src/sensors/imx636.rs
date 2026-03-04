use std::{collections::HashSet, fs, thread, time::Duration};

use augur_core::{
    config::{BiasConfig, DigitalFilterConfig, PixelMaskConfig, RoiConfig},
    CameraError, Result,
};

use crate::{debug, sensors::PseeSensor, transport::Transport, treuzell::Treuzell};

const SENSOR_WIDTH: u16 = 1280;
const SENSOR_HEIGHT: u16 = 720;

const REG_SENSOR_ID: u32 = 0x0014;
const REG_SENSOR_MODE: u32 = 0xF128;

const REG_ROI_CTRL: u32 = 0x0004;
const REG_LIFO_CTRL: u32 = 0x000C;
const REG_ROI_WIN_CTRL: u32 = 0x0034;
const REG_ROI_WIN_START_ADDR: u32 = 0x0038;
const REG_ROI_WIN_END_ADDR: u32 = 0x003C;
const REG_IPH_MIRR_CTRL: u32 = 0x0074;

const REG_BIAS_FO: u32 = 0x1004;
const REG_BIAS_HPF: u32 = 0x100C;
const REG_BIAS_DIFF_ON: u32 = 0x1010;
const REG_BIAS_DIFF_OFF: u32 = 0x1018;
const REG_BIAS_REFR: u32 = 0x1020;

const REG_DEM_BASE: u32 = 0x9100;
const DEM_SLOTS: usize = 64;

const REG_STC_PIPELINE_CONTROL: u32 = 0xD000;
const REG_STC_PARAM: u32 = 0xD004;
const REG_TRAIL_PARAM: u32 = 0xD008;
const REG_STC_TIMESTAMPING: u32 = 0xD00C;
const REG_STC_INVALIDATION: u32 = 0xD0C0;
const REG_STC_INITIALIZATION: u32 = 0xD0C4;

const REG_TIME_BASE_CTRL: u32 = 0x9008;

const BIAS_CONF: u32 = 0x11A1_0000;

#[derive(Debug, Clone, Copy)]
struct BiasDefaults {
    fo: u8,
    hpf: u8,
    diff_on: u8,
    diff_off: u8,
    refr: u8,
}

#[derive(Debug, Clone, Copy)]
struct StcThresholdParam {
    prescaler: u32,
    multiplier: u32,
    dt_fifo_timeout: u32,
}

#[derive(Debug, Clone, Copy)]
enum RegOp {
    Write { address: u32, value: u32 },
    WriteField { address: u32, value: u32, mask: u32 },
    DelayUs(u32),
}

impl RegOp {
    const fn write(address: u32, value: u32) -> Self {
        Self::Write { address, value }
    }

    const fn write_field(address: u32, value: u32, mask: u32) -> Self {
        Self::WriteField {
            address,
            value,
            mask,
        }
    }

    const fn delay_us(usec: u32) -> Self {
        Self::DelayUs(usec)
    }
}

#[derive(Debug, Clone)]
pub struct Imx636 {
    device_id: u32,
    compatible: Vec<String>,
    bias_defaults: Option<BiasDefaults>,
}

impl Default for Imx636 {
    fn default() -> Self {
        Self::new(0, Vec::new())
    }
}

impl Imx636 {
    pub fn new(device_id: u32, compatible: Vec<String>) -> Self {
        Self {
            device_id,
            compatible,
            bias_defaults: None,
        }
    }

    pub fn device_id(&self) -> u32 {
        self.device_id
    }

    pub fn compatible(&self) -> &[String] {
        &self.compatible
    }

    pub fn probe(tz: &mut Treuzell<'_>, device_id: u32) -> Result<bool> {
        let chip_id = tz.read_reg32(device_id, REG_SENSOR_ID)?;
        let mode = tz.read_reg32(device_id, REG_SENSOR_MODE)?;
        Ok(chip_id == 0xA040_1806 && (mode & 0x3) == 0)
    }

    fn load_bias_defaults(&mut self, transport: &mut Transport) -> Result<BiasDefaults> {
        if let Some(defaults) = self.bias_defaults {
            return Ok(defaults);
        }

        let mut tz = Treuzell::new(transport);
        let defaults = BiasDefaults {
            fo: (tz.read_reg32(self.device_id, REG_BIAS_FO)? & 0xFF) as u8,
            hpf: (tz.read_reg32(self.device_id, REG_BIAS_HPF)? & 0xFF) as u8,
            diff_on: (tz.read_reg32(self.device_id, REG_BIAS_DIFF_ON)? & 0xFF) as u8,
            diff_off: (tz.read_reg32(self.device_id, REG_BIAS_DIFF_OFF)? & 0xFF) as u8,
            refr: (tz.read_reg32(self.device_id, REG_BIAS_REFR)? & 0xFF) as u8,
        };
        self.bias_defaults = Some(defaults);
        Ok(defaults)
    }

    fn apply_sequence(&self, transport: &mut Transport, sequence: &[RegOp]) -> Result<()> {
        let mut tz = Treuzell::new(transport);
        for op in sequence {
            match *op {
                RegOp::Write { address, value } => {
                    tz.write_reg32(self.device_id, address, value)?;
                }
                RegOp::WriteField {
                    address,
                    value,
                    mask,
                } => {
                    let cur = tz.read_reg32(self.device_id, address)?;
                    let next = (cur & !mask) | (value & mask);
                    tz.write_reg32(self.device_id, address, next)?;
                }
                RegOp::DelayUs(usec) => {
                    thread::sleep(Duration::from_micros(usec as u64));
                }
            }
        }
        Ok(())
    }

    fn write_masked(
        &self,
        transport: &mut Transport,
        address: u32,
        value: u32,
        mask: u32,
    ) -> Result<()> {
        let mut tz = Treuzell::new(transport);
        let cur = tz.read_reg32(self.device_id, address)?;
        let next = (cur & !mask) | (value & mask);
        tz.write_reg32(self.device_id, address, next)
    }

    fn set_time_base_standalone(&self, transport: &mut Transport) -> Result<()> {
        let mut value = 0_u32;
        value |= 0 << 1; // time_base_mode: internal
        value |= 1 << 2; // external_mode: master when external mode is selected
        value |= 0 << 3; // external_mode_enable: disabled
        value |= 100 << 4; // 1us period
        let mask = (1 << 1) | (1 << 2) | (1 << 3) | (0x7F << 4);
        self.write_masked(transport, REG_TIME_BASE_CTRL, value, mask)
    }

    fn iph_mirror_control(&self, transport: &mut Transport, enable: bool) -> Result<()> {
        let mut tz = Treuzell::new(transport);
        let mut reg = tz.read_reg32(self.device_id, REG_IPH_MIRR_CTRL)?;
        if enable {
            reg |= 1 << 0;
        } else {
            reg &= !(1 << 0);
        }
        tz.write_reg32(self.device_id, REG_IPH_MIRR_CTRL, reg)?;

        thread::sleep(Duration::from_micros(20));

        let mut reg = tz.read_reg32(self.device_id, REG_IPH_MIRR_CTRL)?;
        if enable {
            reg |= 1 << 1;
        } else {
            reg &= !(1 << 1);
        }
        tz.write_reg32(self.device_id, REG_IPH_MIRR_CTRL, reg)?;

        thread::sleep(Duration::from_micros(20));
        Ok(())
    }

    fn lifo_control(
        &self,
        transport: &mut Transport,
        enable: bool,
        out_en: bool,
        cnt_en: bool,
    ) -> Result<()> {
        let mut tz = Treuzell::new(transport);
        if enable && out_en {
            let mut reg = tz.read_reg32(self.device_id, REG_LIFO_CTRL)?;
            reg |= 1 << 0;
            tz.write_reg32(self.device_id, REG_LIFO_CTRL, reg)?;
            thread::sleep(Duration::from_millis(1));

            let mut reg = tz.read_reg32(self.device_id, REG_LIFO_CTRL)?;
            reg |= 1 << 1;
            tz.write_reg32(self.device_id, REG_LIFO_CTRL, reg)?;
            thread::sleep(Duration::from_millis(1));
        } else {
            let mut reg = tz.read_reg32(self.device_id, REG_LIFO_CTRL)?;
            reg = if enable {
                reg | (1 << 0)
            } else {
                reg & !(1 << 0)
            };
            reg = if out_en {
                reg | (1 << 1)
            } else {
                reg & !(1 << 1)
            };
            tz.write_reg32(self.device_id, REG_LIFO_CTRL, reg)?;
        }

        let mut reg = tz.read_reg32(self.device_id, REG_LIFO_CTRL)?;
        reg = if cnt_en {
            reg | (1 << 2)
        } else {
            reg & !(1 << 2)
        };
        tz.write_reg32(self.device_id, REG_LIFO_CTRL, reg)
    }

    fn encode_bias(base: u8, offset: i32) -> u32 {
        let value = (base as i32 + offset).clamp(0, 255) as u32;
        value | BIAS_CONF
    }

    fn validate_bias_ranges(cfg: &BiasConfig) -> Result<()> {
        check_range("fo", cfg.fo, -35, 55)?;
        check_range("hpf", cfg.hpf, 0, 120)?;
        check_range("diff_on", cfg.diff_on, -85, 140)?;
        check_range("diff_off", cfg.diff_off, -35, 190)?;
        check_range("refr", cfg.refr, -20, 235)?;
        Ok(())
    }

    fn decode_masked_pixels(mask: &PixelMaskConfig) -> Result<Vec<(u16, u16)>> {
        let mut pixels = mask.masked_pixels.clone();

        if let Some(path) = &mask.mask_file {
            let bytes = fs::read(path)?;
            let expected_size = (SENSOR_WIDTH as usize * SENSOR_HEIGHT as usize).div_ceil(8);
            if bytes.len() != expected_size {
                return Err(CameraError::Config(format!(
                    "mask file '{}' has {} bytes, expected {} for {}x{} bitfield",
                    path.display(),
                    bytes.len(),
                    expected_size,
                    SENSOR_WIDTH,
                    SENSOR_HEIGHT
                )));
            }

            for y in 0..SENSOR_HEIGHT {
                for x in 0..SENSOR_WIDTH {
                    let idx = y as usize * SENSOR_WIDTH as usize + x as usize;
                    let b = bytes[idx / 8];
                    let bit = idx % 8;
                    if (b >> bit) & 1 == 1 {
                        pixels.push((x, y));
                    }
                }
            }
        }

        let mut seen = HashSet::new();
        pixels.retain(|p| seen.insert(*p));

        if pixels.len() > DEM_SLOTS {
            return Err(CameraError::Config(format!(
                "IMX636 digital mask supports up to {DEM_SLOTS} pixels, got {}",
                pixels.len()
            )));
        }

        Ok(pixels)
    }

    fn stc_param_for(threshold_us: u32) -> StcThresholdParam {
        let ms =
            ((threshold_us as f32 / 1000.0).round() as usize).clamp(1, STC_THRESHOLD_PARAMS.len());
        STC_THRESHOLD_PARAMS[ms - 1]
    }
}

impl PseeSensor for Imx636 {
    fn name(&self) -> &'static str {
        "IMX636"
    }

    fn geometry(&self) -> (u16, u16) {
        (SENSOR_WIDTH, SENSOR_HEIGHT)
    }

    fn init(&mut self, transport: &mut Transport) -> Result<()> {
        let compatible = if self.compatible.is_empty() {
            "<unknown>".to_string()
        } else {
            self.compatible.join(",")
        };
        debug::log(format!(
            "initializing IMX636 on Treuzell device {} (compatible={compatible})",
            self.device_id
        ));

        self.apply_sequence(transport, ISSD_STOP)?;
        self.apply_sequence(transport, ISSD_DESTROY)?;
        self.apply_sequence(transport, ISSD_INIT)?;

        self.set_time_base_standalone(transport)?;
        self.iph_mirror_control(transport, true)?;
        thread::sleep(Duration::from_millis(1));
        self.lifo_control(transport, true, true, true)?;

        let _ = self.load_bias_defaults(transport)?;
        Ok(())
    }

    fn set_biases(&mut self, transport: &mut Transport, cfg: &BiasConfig) -> Result<()> {
        Self::validate_bias_ranges(cfg)?;

        let defaults = self.load_bias_defaults(transport)?;
        let mut tz = Treuzell::new(transport);

        tz.write_reg32(
            self.device_id,
            REG_BIAS_FO,
            Self::encode_bias(defaults.fo, cfg.fo),
        )?;
        tz.write_reg32(
            self.device_id,
            REG_BIAS_HPF,
            Self::encode_bias(defaults.hpf, cfg.hpf),
        )?;
        tz.write_reg32(
            self.device_id,
            REG_BIAS_DIFF_ON,
            Self::encode_bias(defaults.diff_on, cfg.diff_on),
        )?;
        tz.write_reg32(
            self.device_id,
            REG_BIAS_DIFF_OFF,
            Self::encode_bias(defaults.diff_off, cfg.diff_off),
        )?;
        tz.write_reg32(
            self.device_id,
            REG_BIAS_REFR,
            Self::encode_bias(defaults.refr, cfg.refr),
        )?;

        Ok(())
    }

    fn set_roi(&mut self, transport: &mut Transport, roi: &RoiConfig) -> Result<()> {
        roi.validate(SENSOR_WIDTH, SENSOR_HEIGHT)?;

        let start = (roi.x as u32 & 0x7FF) | ((roi.y as u32 & 0x3FF) << 16);
        let end_x = roi.x as u32 + roi.width as u32;
        let end_y = roi.y as u32 + roi.height as u32;
        let end = (end_x & 0x7FF) | ((end_y & 0x3FF) << 16);

        {
            let mut tz = Treuzell::new(transport);

            // Disable window trigger and clear completion flag.
            let mut win_ctrl = tz.read_reg32(self.device_id, REG_ROI_WIN_CTRL)?;
            win_ctrl &= !((1 << 0) | (1 << 1));
            tz.write_reg32(self.device_id, REG_ROI_WIN_CTRL, win_ctrl)?;

            tz.write_reg32(self.device_id, REG_ROI_WIN_START_ADDR, start)?;
            tz.write_reg32(self.device_id, REG_ROI_WIN_END_ADDR, end)?;

            // Trigger ROI window application.
            win_ctrl |= 1 << 0;
            tz.write_reg32(self.device_id, REG_ROI_WIN_CTRL, win_ctrl)?;

            let mut done = false;
            for _ in 0..100 {
                let reg = tz.read_reg32(self.device_id, REG_ROI_WIN_CTRL)?;
                if ((reg >> 1) & 1) == 1 {
                    done = true;
                    break;
                }
                thread::sleep(Duration::from_micros(50));
            }
            if !done {
                return Err(CameraError::Transport(
                    "ROI window programming timed out (roi_win_done=0)".into(),
                ));
            }

            // Enable ROI path in ROI mode.
            let mut roi_ctrl = tz.read_reg32(self.device_id, REG_ROI_CTRL)?;
            roi_ctrl |= 1 << 1; // roi_td_en
            roi_ctrl |= 1 << 5; // roi_td_shadow_trigger
            roi_ctrl |= 1 << 6; // td_roi_roni_n_en => ROI mode
            roi_ctrl |= 1 << 10; // px_td_rstn
            tz.write_reg32(self.device_id, REG_ROI_CTRL, roi_ctrl)?;
        }

        Ok(())
    }

    fn set_pixel_mask(&mut self, transport: &mut Transport, mask: &PixelMaskConfig) -> Result<()> {
        let pixels = Self::decode_masked_pixels(mask)?;
        let mut tz = Treuzell::new(transport);

        for i in 0..DEM_SLOTS {
            tz.write_reg32(self.device_id, REG_DEM_BASE + (i as u32 * 4), 0)?;
        }

        for (i, (x, y)) in pixels.iter().copied().enumerate() {
            let value = (x as u32 & 0x7FF) | ((y as u32 & 0x7FF) << 16) | (1_u32 << 31);
            tz.write_reg32(self.device_id, REG_DEM_BASE + (i as u32 * 4), value)?;
        }

        Ok(())
    }

    fn set_digital_filter(
        &mut self,
        transport: &mut Transport,
        filter: &DigitalFilterConfig,
    ) -> Result<()> {
        if filter.stc_enabled && filter.trail_enabled {
            return Err(CameraError::Config(
                "STC and Trail cannot be enabled simultaneously on IMX636".into(),
            ));
        }

        if filter.stc_threshold_us < 1_000 || filter.stc_threshold_us > 100_000 {
            return Err(CameraError::Config(
                "stc_threshold_us must be in [1000, 100000]".into(),
            ));
        }

        let mut tz = Treuzell::new(transport);

        // Bypass filter pipeline by default.
        tz.write_reg32(self.device_id, REG_STC_PIPELINE_CONTROL, 0b101)?;

        if !filter.stc_enabled && !filter.trail_enabled {
            return Ok(());
        }

        let params = Self::stc_param_for(filter.stc_threshold_us);

        // Start STC SRAM initialization: stc_flag_init_done=1, stc_req_init=1.
        tz.write_reg32(self.device_id, REG_STC_INITIALIZATION, (1 << 2) | (1 << 0))?;

        if filter.stc_enabled {
            let mut stc_param = 0_u32;
            stc_param |= 1 << 0; // stc_enable
            stc_param |= (filter.stc_threshold_us & 0x7FFFF) << 1; // stc_threshold
                                                                   // disable_stc_cut_trail left to 0 => STC_CUT_TRAIL behavior
            tz.write_reg32(self.device_id, REG_STC_PARAM, stc_param)?;
            tz.write_reg32(self.device_id, REG_TRAIL_PARAM, 0)?;
        } else {
            tz.write_reg32(self.device_id, REG_STC_PARAM, 0)?;
            let mut trail_param = 0_u32;
            trail_param |= 1 << 0; // trail_enable
            trail_param |= (filter.stc_threshold_us & 0x7FFFF) << 1; // trail_threshold
            tz.write_reg32(self.device_id, REG_TRAIL_PARAM, trail_param)?;
        }

        let timestamping =
            (params.prescaler & 0x1F) | ((params.multiplier & 0x0F) << 5) | (1 << 9) | (1 << 16);
        tz.write_reg32(self.device_id, REG_STC_TIMESTAMPING, timestamping)?;

        // Keep dt_fifo_wait_time at default (4), set timeout and reserved defaults.
        let invalidation = 4_u32 | ((params.dt_fifo_timeout & 0xFFF) << 12) | (0xA << 24);
        tz.write_reg32(self.device_id, REG_STC_INVALIDATION, invalidation)?;

        let mut init_done = false;
        for _ in 0..3 {
            let init_reg = tz.read_reg32(self.device_id, REG_STC_INITIALIZATION)?;
            if ((init_reg >> 2) & 1) == 1 {
                init_done = true;
                break;
            }
            thread::sleep(Duration::from_micros(50));
        }
        if !init_done {
            return Err(CameraError::Transport(
                "STC initialization did not complete".into(),
            ));
        }

        tz.write_reg32(self.device_id, REG_STC_PIPELINE_CONTROL, 0b001)?;
        Ok(())
    }

    fn start_streaming(&mut self, transport: &mut Transport) -> Result<()> {
        self.apply_sequence(transport, ISSD_START)
    }

    fn stop_streaming(&mut self, transport: &mut Transport) -> Result<()> {
        self.apply_sequence(transport, ISSD_STOP)
    }
}

fn check_range(name: &str, value: i32, min: i32, max: i32) -> Result<()> {
    if value < min || value > max {
        return Err(CameraError::Config(format!(
            "bias '{name}' offset {value} outside [{min}, {max}]"
        )));
    }
    Ok(())
}

const ISSD_INIT: &[RegOp] = &[
    RegOp::write(0x0000001C, 0x00000001),
    RegOp::delay_us(1000000),
    RegOp::write(0x00400004, 0x00000001),
    RegOp::delay_us(500000),
    RegOp::write(0x00400004, 0x00000000),
    RegOp::delay_us(1000000),
    RegOp::write(0x0000B000, 0x00000158),
    RegOp::delay_us(300),
    RegOp::write(0x0000B044, 0x00000000),
    RegOp::write(0x0000B004, 0x0000000A),
    RegOp::write(0x0000B040, 0x00000000),
    RegOp::write(0x0000B0C8, 0x00000000),
    RegOp::write(0x0000B040, 0x00000000),
    RegOp::write(0x0000B040, 0x00000000),
    RegOp::write(0x00000000, 0x4F006442),
    RegOp::write(0x00000000, 0x0F006442),
    RegOp::write(0x000000B8, 0x00000400),
    RegOp::write(0x000000B8, 0x00000400),
    RegOp::write(0x0000B07C, 0x00000000),
    RegOp::write(0x0000B074, 0x00000002),
    RegOp::write(0x0000B078, 0x000000A0),
    RegOp::write(0x000000C0, 0x00000110),
    RegOp::write(0x000000C0, 0x00000210),
    RegOp::write(0x0000B120, 0x00000001),
    RegOp::write(0x0000E120, 0x00000000),
    RegOp::write(0x0000B068, 0x00000004),
    RegOp::write(0x0000B07C, 0x00000001),
    RegOp::delay_us(10),
    RegOp::write(0x0000B07C, 0x00000003),
    RegOp::delay_us(1000),
    RegOp::write(0x000000B8, 0x00000401),
    RegOp::write(0x000000B8, 0x00000409),
    RegOp::write(0x00000000, 0x4F006442),
    RegOp::write(0x00000000, 0x4F00644A),
    RegOp::write(0x0000B080, 0x00000077),
    RegOp::write(0x0000B084, 0x0000000F),
    RegOp::write(0x0000B088, 0x00000037),
    RegOp::write(0x0000B08C, 0x00000037),
    RegOp::write(0x0000B090, 0x000000DF),
    RegOp::write(0x0000B094, 0x00000057),
    RegOp::write(0x0000B098, 0x00000037),
    RegOp::write(0x0000B09C, 0x00000067),
    RegOp::write(0x0000B0A0, 0x00000037),
    RegOp::write(0x0000B0A4, 0x0000002F),
    RegOp::write(0x0000B0AC, 0x00000028),
    RegOp::write(0x0000B0CC, 0x00000001),
    RegOp::write(0x0000B000, 0x000002F8),
    RegOp::write(0x0000B004, 0x0000008A),
    RegOp::write(0x0000B01C, 0x00000030),
    RegOp::write(0x0000B020, 0x00002000),
    RegOp::write(0x0000B02C, 0x000000FF),
    RegOp::write(0x0000B030, 0x00003E80),
    RegOp::write(0x0000B028, 0x00000FA0),
    RegOp::write(0x0000A000, 0x000B0501),
    RegOp::delay_us(200),
    RegOp::write(0x0000A008, 0x00002405),
    RegOp::delay_us(200),
    RegOp::write(0x0000A004, 0x000B0501),
    RegOp::delay_us(200),
    RegOp::write(0x0000A020, 0x00000150),
    RegOp::delay_us(200),
    RegOp::write(0x0000B040, 0x00000007),
    RegOp::write(0x0000B064, 0x00000006),
    RegOp::write(0x0000B040, 0x0000000F),
    RegOp::delay_us(100),
    RegOp::write(0x0000B004, 0x0000008A),
    RegOp::delay_us(200),
    RegOp::write(0x0000B0C8, 0x00000003),
    RegOp::delay_us(200),
    RegOp::write(0x0000B044, 0x00000001),
    RegOp::write(0x0000B000, 0x000002F9),
    RegOp::write(0x00007008, 0x00000001),
    RegOp::write(0x00007000, 0x00070001),
    RegOp::write(0x00008000, 0x0001E085),
    RegOp::write(0x00009008, 0x00000644),
    RegOp::write(0x00000004, 0xF0005042),
    RegOp::write(0x00000018, 0x00000200),
    RegOp::write(0x00001014, 0x11A1504D),
    RegOp::write(0x00009004, 0x00000000),
    RegOp::delay_us(1000),
    RegOp::write(0x00009000, 0x00000200),
];

const ISSD_START: &[RegOp] = &[
    RegOp::write(0x0000B000, 0x000002F9),
    RegOp::write(0x00009028, 0x00000000),
    RegOp::write_field(0x00009008, 0x645, 0x00000001),
    RegOp::write(0x0000002C, 0x0022C724),
    RegOp::write_field(0x00000004, 0xF0005442, 0x00000400),
];

const ISSD_STOP: &[RegOp] = &[
    RegOp::write_field(0x00000004, 0xF0005042, 0x00000400),
    RegOp::write(0x0000002C, 0x0022C324),
    RegOp::write(0x00009028, 0x00000002),
    RegOp::delay_us(1000),
    RegOp::write_field(0x00009008, 0x00000644, 0x00000001),
    RegOp::write(0x0000B000, 0x000002F8),
    RegOp::delay_us(300),
];

const ISSD_DESTROY: &[RegOp] = &[
    RegOp::write(0x00000070, 0x00400008),
    RegOp::write(0x0000006C, 0x0EE47114),
    RegOp::delay_us(500),
    RegOp::write(0x0000A00C, 0x00020400),
    RegOp::delay_us(500),
    RegOp::write(0x0000A010, 0x00008068),
    RegOp::delay_us(200),
    RegOp::write(0x00001104, 0x00000000),
    RegOp::delay_us(200),
    RegOp::write(0x0000A020, 0x00000050),
    RegOp::delay_us(200),
    RegOp::write(0x0000A004, 0x000B0500),
    RegOp::delay_us(200),
    RegOp::write(0x0000A008, 0x00002404),
    RegOp::delay_us(200),
    RegOp::write(0x0000A000, 0x000B0500),
    RegOp::write(0x0000B044, 0x00000000),
    RegOp::write(0x0000B004, 0x0000000A),
    RegOp::write(0x0000B040, 0x0000000E),
    RegOp::write(0x0000B0C8, 0x00000000),
    RegOp::write(0x0000B040, 0x00000006),
    RegOp::write(0x0000B040, 0x00000004),
    RegOp::write(0x00000000, 0x4F006442),
    RegOp::write(0x00000000, 0x0F006442),
    RegOp::write(0x000000B8, 0x00000401),
    RegOp::write(0x000000B8, 0x00000400),
    RegOp::write(0x0000B07C, 0x00000000),
];

const STC_THRESHOLD_PARAMS: [StcThresholdParam; 100] = [
    StcThresholdParam {
        prescaler: 12,
        multiplier: 15,
        dt_fifo_timeout: 90,
    }, // 1ms
    StcThresholdParam {
        prescaler: 10,
        multiplier: 3,
        dt_fifo_timeout: 90,
    }, // 2ms
    StcThresholdParam {
        prescaler: 11,
        multiplier: 5,
        dt_fifo_timeout: 95,
    }, // 3ms
    StcThresholdParam {
        prescaler: 9,
        multiplier: 1,
        dt_fifo_timeout: 102,
    }, // 4ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 15,
        dt_fifo_timeout: 90,
    }, // 5ms
    StcThresholdParam {
        prescaler: 11,
        multiplier: 3,
        dt_fifo_timeout: 114,
    }, // 6ms
    StcThresholdParam {
        prescaler: 11,
        multiplier: 3,
        dt_fifo_timeout: 90,
    }, // 7ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 5,
        dt_fifo_timeout: 109,
    }, // 8ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 9,
        dt_fifo_timeout: 122,
    }, // 9ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 9,
        dt_fifo_timeout: 90,
    }, // 10ms
    StcThresholdParam {
        prescaler: 10,
        multiplier: 1,
        dt_fifo_timeout: 102,
    }, // 11ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 15,
        dt_fifo_timeout: 109,
    }, // 12ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 7,
        dt_fifo_timeout: 117,
    }, // 13ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 13,
        dt_fifo_timeout: 127,
    }, // 14ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 3,
        dt_fifo_timeout: 138,
    }, // 15ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 3,
        dt_fifo_timeout: 90,
    }, // 16ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 3,
        dt_fifo_timeout: 90,
    }, // 17ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 11,
        dt_fifo_timeout: 99,
    }, // 18ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 5,
        dt_fifo_timeout: 109,
    }, // 19ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 5,
        dt_fifo_timeout: 109,
    }, // 20ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 9,
        dt_fifo_timeout: 122,
    }, // 21ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 9,
        dt_fifo_timeout: 122,
    }, // 22ms
    StcThresholdParam {
        prescaler: 11,
        multiplier: 1,
        dt_fifo_timeout: 209,
    }, // 23ms
    StcThresholdParam {
        prescaler: 11,
        multiplier: 1,
        dt_fifo_timeout: 138,
    }, // 24ms
    StcThresholdParam {
        prescaler: 11,
        multiplier: 1,
        dt_fifo_timeout: 138,
    }, // 25ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 15,
        dt_fifo_timeout: 147,
    }, // 26ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 15,
        dt_fifo_timeout: 147,
    }, // 27ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 7,
        dt_fifo_timeout: 158,
    }, // 28ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 7,
        dt_fifo_timeout: 158,
    }, // 29ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 13,
        dt_fifo_timeout: 170,
    }, // 30ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 13,
        dt_fifo_timeout: 170,
    }, // 31ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 32ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 33ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 34ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 3,
        dt_fifo_timeout: 90,
    }, // 35ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 3,
        dt_fifo_timeout: 90,
    }, // 36ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 11,
        dt_fifo_timeout: 202,
    }, // 37ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 11,
        dt_fifo_timeout: 99,
    }, // 38ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 11,
        dt_fifo_timeout: 99,
    }, // 39ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 11,
        dt_fifo_timeout: 99,
    }, // 40ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 5,
        dt_fifo_timeout: 109,
    }, // 41ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 5,
        dt_fifo_timeout: 109,
    }, // 42ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 5,
        dt_fifo_timeout: 109,
    }, // 43ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 5,
        dt_fifo_timeout: 109,
    }, // 44ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 45ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 9,
        dt_fifo_timeout: 122,
    }, // 46ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 9,
        dt_fifo_timeout: 122,
    }, // 47ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 9,
        dt_fifo_timeout: 122,
    }, // 48ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 9,
        dt_fifo_timeout: 122,
    }, // 49ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 1,
        dt_fifo_timeout: 280,
    }, // 50ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 1,
        dt_fifo_timeout: 280,
    }, // 51ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 1,
        dt_fifo_timeout: 138,
    }, // 52ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 1,
        dt_fifo_timeout: 138,
    }, // 53ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 1,
        dt_fifo_timeout: 138,
    }, // 54ms
    StcThresholdParam {
        prescaler: 12,
        multiplier: 1,
        dt_fifo_timeout: 138,
    }, // 55ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 15,
        dt_fifo_timeout: 147,
    }, // 56ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 15,
        dt_fifo_timeout: 147,
    }, // 57ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 15,
        dt_fifo_timeout: 147,
    }, // 58ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 7,
        dt_fifo_timeout: 158,
    }, // 59ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 7,
        dt_fifo_timeout: 158,
    }, // 60ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 7,
        dt_fifo_timeout: 158,
    }, // 61ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 7,
        dt_fifo_timeout: 158,
    }, // 62ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 7,
        dt_fifo_timeout: 158,
    }, // 63ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 13,
        dt_fifo_timeout: 171,
    }, // 64ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 13,
        dt_fifo_timeout: 171,
    }, // 65ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 13,
        dt_fifo_timeout: 171,
    }, // 66ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 13,
        dt_fifo_timeout: 171,
    }, // 67ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 13,
        dt_fifo_timeout: 171,
    }, // 68ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 69ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 70ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 71ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 72ms
    StcThresholdParam {
        prescaler: 14,
        multiplier: 3,
        dt_fifo_timeout: 185,
    }, // 73ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 11,
        dt_fifo_timeout: 409,
    }, // 74ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 11,
        dt_fifo_timeout: 202,
    }, // 75ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 11,
        dt_fifo_timeout: 202,
    }, // 76ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 11,
        dt_fifo_timeout: 202,
    }, // 77ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 11,
        dt_fifo_timeout: 202,
    }, // 78ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 11,
        dt_fifo_timeout: 202,
    }, // 79ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 11,
        dt_fifo_timeout: 202,
    }, // 80ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 451,
    }, // 81ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 223,
    }, // 82ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 223,
    }, // 83ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 223,
    }, // 84ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 223,
    }, // 85ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 223,
    }, // 86ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 223,
    }, // 87ms
    StcThresholdParam {
        prescaler: 15,
        multiplier: 5,
        dt_fifo_timeout: 223,
    }, // 88ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 501,
    }, // 89ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 501,
    }, // 90ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 501,
    }, // 91ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 92ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 93ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 94ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 95ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 96ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 97ms
    StcThresholdParam {
        prescaler: 16,
        multiplier: 9,
        dt_fifo_timeout: 248,
    }, // 98ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 1,
        dt_fifo_timeout: 564,
    }, // 99ms
    StcThresholdParam {
        prescaler: 13,
        multiplier: 1,
        dt_fifo_timeout: 564,
    }, // 100ms
];
