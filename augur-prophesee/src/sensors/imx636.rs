use std::{collections::HashSet, fs, thread, time::Duration};

use augur_core::{
    camera::{BiasCodes, BiasReadback, SensorMonitoring, SensorMonitoringSelection},
    config::{BiasConfig, DigitalFilterConfig, ExternalTriggerConfig, PixelMaskConfig, RoiConfig},
    CameraError, Result,
};

use crate::{debug, sensors::PseeSensor, transport::Transport, treuzell::Treuzell};

const SENSOR_WIDTH: u16 = 1280;
const SENSOR_HEIGHT: u16 = 720;

const REG_SENSOR_ID: u32 = 0x0014;
const REG_SENSOR_MODE: u32 = 0xF128;

const REG_ROI_CTRL: u32 = 0x0004;
const REG_LIFO_CTRL: u32 = 0x000C;
const REG_LIFO_STATUS: u32 = 0x0010;
const REG_REFRACTORY_CTRL: u32 = 0x0020;
const REG_ROI_WIN_CTRL: u32 = 0x0034;
const REG_ROI_WIN_START_ADDR: u32 = 0x0038;
const REG_ROI_WIN_END_ADDR: u32 = 0x003C;
const REG_IPH_MIRR_CTRL: u32 = 0x0074;
const REG_DIG_PAD2_CTRL: u32 = 0x0044;
const REG_ADC_CONTROL: u32 = 0x004C;
const REG_ADC_STATUS: u32 = 0x0050;
const REG_ADC_MISC_CTRL: u32 = 0x0054;
const REG_TEMP_CTRL: u32 = 0x005C;

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
const REG_EDF_RESERVED_7004: u32 = 0x7004;

const BIAS_CONF: u32 = 0x11A1_0000;

// Sensor monitoring block: the fields behind Metavision's `I_Monitoring`
// facility, which is the only place the IMX636 reports an abstract setting
// back as an absolute physical quantity. Field offsets come from OpenEB's
// `Imx636RegisterMap`, the conversions from `TzImx636`.

/// `refractory_ctrl.refr_counter` — 28-bit dead-time measurement.
const REFR_COUNTER_MASK: u32 = (1 << 28) - 1;
/// `refractory_ctrl.refr_valid` — the counter holds a completed measurement.
const REFR_VALID: u32 = 1 << 28;
/// `refractory_ctrl.refr_cnt_en` — run the dead-time counter.
const REFR_CNT_EN: u32 = 1 << 30;
/// `refractory_ctrl.refr_en` — power up the refractory monitor.
const REFR_EN: u32 = 1 << 31;
/// The refractory counter ticks on both edges of the 100 MHz sensor clock.
const REFRACTORY_TICKS_PER_US: f32 = 200.0;

/// `lifo_status.lifo_ton`, masked as OpenEB's `get_illumination` masks it. The
/// register map declares 29 bits; the two dropped bits only matter far below
/// 0.01 lux, so matching the reference implementation keeps the calibration
/// constants below meaningful.
const LIFO_TON_MASK: u32 = (1 << 27) - 1;
/// `lifo_status.lifo_ton_valid`.
const LIFO_TON_VALID: u32 = 1 << 29;

/// `adc_control.adc_en`, `.adc_clk_en`, `.adc_start`.
const ADC_EN: u32 = 1 << 0;
const ADC_CLK_EN: u32 = 1 << 1;
const ADC_START: u32 = 1 << 2;
/// `adc_status.adc_dac_dyn` — 10-bit conversion result.
const ADC_DAC_DYN_MASK: u32 = (1 << 10) - 1;
/// `adc_status.adc_done_dyn`.
const ADC_DONE_DYN: u32 = 1 << 11;
/// `adc_misc_ctrl.adc_buf_cal_en`, `.adc_temp`.
const ADC_BUF_CAL_EN: u32 = 1 << 1;
const ADC_TEMP: u32 = 1 << 12;
/// `temp_ctrl.temp_buf_cal_en`, `.temp_buf_en`.
const TEMP_BUF_CAL_EN: u32 = 1 << 0;
const TEMP_BUF_EN: u32 = 1 << 1;

/// Polling budgets for the three monitoring readbacks. Each iteration is one
/// USB control transfer, so these stay as tight as OpenEB's.
const REFRACTORY_READ_RETRIES: u8 = 10;
const LIFO_READ_RETRIES: u8 = 10;
const TEMPERATURE_READ_RETRIES: u8 = 5;

#[derive(Debug, Clone, Copy)]
struct StcThresholdParam {
    prescaler: u32,
    multiplier: u32,
    dt_fifo_timeout: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    bias_defaults: Option<BiasCodes>,
    /// Whether the temperature ADC has been brought up. The init is deferred
    /// to the first temperature read so a session that never opens the
    /// settings panel pays nothing for it.
    temperature_adc_ready: bool,
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
            temperature_adc_ready: false,
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

    /// Factory-trimmed bias codes, cached from the first read. `init` runs this
    /// before any bias write, so the cached codes are the ones the configured
    /// offsets are relative to.
    fn load_bias_defaults(&mut self, transport: &mut Transport) -> Result<BiasCodes> {
        if let Some(defaults) = self.bias_defaults {
            return Ok(defaults);
        }

        let defaults = self.read_bias_codes(transport)?;
        self.bias_defaults = Some(defaults);
        Ok(defaults)
    }

    /// Absolute 8-bit codes currently sitting in the five bias registers.
    fn read_bias_codes(&self, transport: &mut Transport) -> Result<BiasCodes> {
        let mut tz = Treuzell::new(transport);
        Ok(BiasCodes {
            fo: (tz.read_reg32(self.device_id, REG_BIAS_FO)? & 0xFF) as u8,
            hpf: (tz.read_reg32(self.device_id, REG_BIAS_HPF)? & 0xFF) as u8,
            diff_on: (tz.read_reg32(self.device_id, REG_BIAS_DIFF_ON)? & 0xFF) as u8,
            diff_off: (tz.read_reg32(self.device_id, REG_BIAS_DIFF_OFF)? & 0xFF) as u8,
            refr: (tz.read_reg32(self.device_id, REG_BIAS_REFR)? & 0xFF) as u8,
        })
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
        for bit in 0..2_u32 {
            let mut reg = tz.read_reg32(self.device_id, REG_IPH_MIRR_CTRL)?;
            if enable {
                reg |= 1 << bit;
            } else {
                reg &= !(1 << bit);
            }
            tz.write_reg32(self.device_id, REG_IPH_MIRR_CTRL, reg)?;
            thread::sleep(Duration::from_micros(20));
        }
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

    /// Measured pixel dead time (refractory period) in µs — the absolute
    /// counterpart to the `refr` bias offset.
    ///
    /// Mirrors OpenEB's `TzImx636::get_pixel_dead_time`. The enable bits stay
    /// set after the read, as they do in OpenEB: the monitor needs them on to
    /// produce a value, and re-arming it on every poll would only add USB
    /// traffic. `refractory_ctrl` (0x0020) is a monitoring register, distinct
    /// from the `bias_refr` DAC at 0x1020, so this does not alter the pixel
    /// configuration.
    fn read_pixel_dead_time_us(&self, transport: &mut Transport) -> Result<Option<f32>> {
        let enable = REFR_EN | REFR_CNT_EN;
        self.write_masked(transport, REG_REFRACTORY_CTRL, enable, enable)?;

        let mut tz = Treuzell::new(transport);
        for _ in 0..REFRACTORY_READ_RETRIES {
            // One read per attempt: taking `refr_valid` and `refr_counter`
            // from the same word rules out the counter updating between two
            // separate reads.
            let reg = tz.read_reg32(self.device_id, REG_REFRACTORY_CTRL)?;
            if reg & REFR_VALID != 0 {
                let counter = reg & REFR_COUNTER_MASK;
                return Ok(Some(counter as f32 / REFRACTORY_TICKS_PER_US));
            }
        }
        Ok(None)
    }

    /// Scene illumination in lux, from the LIFO integration counter.
    ///
    /// Mirrors OpenEB's `TzImx636::get_illumination`. `init` already enables
    /// the LIFO and its counter, so no arming is needed here.
    fn read_illumination_lux(&self, transport: &mut Transport) -> Result<Option<f32>> {
        let mut tz = Treuzell::new(transport);
        for _ in 0..LIFO_READ_RETRIES {
            let reg = tz.read_reg32(self.device_id, REG_LIFO_STATUS)?;
            if reg & LIFO_TON_VALID == 0 {
                continue;
            }
            let counter = reg & LIFO_TON_MASK;
            if counter == 0 || counter == LIFO_TON_MASK {
                // An empty or saturated integration window carries no value
                // the logarithmic conversion could turn into a lux figure.
                return Ok(None);
            }
            let integration_us = counter as f32 / 100.0;
            return Ok(Some(10_f32.powf(3.5 - (integration_us * 0.37).log10())));
        }
        Ok(None)
    }

    /// Sensor die temperature in °C, from the on-chip ADC.
    ///
    /// Mirrors OpenEB's `TzImx636::get_temperature`, including the ADC bring-up
    /// that OpenEB performs during device init.
    fn read_temperature_c(&mut self, transport: &mut Transport) -> Result<Option<f32>> {
        if !self.temperature_adc_ready {
            self.apply_sequence(transport, IMX636_TEMPERATURE_ADC_INIT)?;
            self.temperature_adc_ready = true;
        }
        self.apply_sequence(transport, IMX636_TEMPERATURE_ADC_START)?;

        let mut reading = None;
        {
            let mut tz = Treuzell::new(transport);
            for _ in 0..TEMPERATURE_READ_RETRIES {
                let reg = tz.read_reg32(self.device_id, REG_ADC_STATUS)?;
                if reg & ADC_DONE_DYN != 0 {
                    let code = (reg & ADC_DAC_DYN_MASK) as f32;
                    reading = Some(0.190 * code - 56.0);
                    break;
                }
            }
        }

        // Gate the ADC clock again even when the conversion never completed,
        // so a failed read cannot leave the converter running.
        self.write_masked(transport, REG_ADC_CONTROL, 0, ADC_CLK_EN)?;
        Ok(reading)
    }
}

impl Imx636 {
    fn external_trigger_sequence(cfg: &ExternalTriggerConfig) -> Result<&'static [RegOp]> {
        if cfg.channel != 0 {
            return Err(CameraError::Config(
                "only external trigger channel 0 is supported on IMX636".into(),
            ));
        }
        Ok(if cfg.enabled {
            // OpenEB: hal_psee_plugins/src/devices/gen41/gen41_tz_trigger_event.cpp
            // (Gen41TzTriggerEvent::enable)
            IMX636_EXTERNAL_TRIGGER_ENABLE_CH0
        } else {
            IMX636_EXTERNAL_TRIGGER_DISABLE_CH0
        })
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

    fn set_external_trigger(
        &mut self,
        transport: &mut Transport,
        cfg: &ExternalTriggerConfig,
    ) -> Result<()> {
        let sequence = Self::external_trigger_sequence(cfg)?;
        self.apply_sequence(transport, sequence)
    }

    fn start_streaming(&mut self, transport: &mut Transport) -> Result<()> {
        self.apply_sequence(transport, ISSD_START)
    }

    fn stop_streaming(&mut self, transport: &mut Transport) -> Result<()> {
        self.apply_sequence(transport, ISSD_STOP)
    }

    fn read_monitoring(&mut self, transport: &mut Transport) -> Result<SensorMonitoring> {
        self.read_monitoring_selected(transport, SensorMonitoringSelection::ALL)
    }

    fn read_monitoring_selected(
        &mut self,
        transport: &mut Transport,
        selection: SensorMonitoringSelection,
    ) -> Result<SensorMonitoring> {
        if selection.is_empty() {
            return Ok(SensorMonitoring::default());
        }
        let biases = if selection.biases {
            Some(BiasReadback {
                current: self.read_bias_codes(transport)?,
                factory_default: self.load_bias_defaults(transport)?,
            })
        } else {
            None
        };
        Ok(SensorMonitoring {
            pixel_dead_time_us: selection
                .pixel_dead_time
                .then(|| self.read_pixel_dead_time_us(transport))
                .transpose()?
                .flatten(),
            illumination_lux: selection
                .illumination
                .then(|| self.read_illumination_lux(transport))
                .transpose()?
                .flatten(),
            temperature_c: selection
                .temperature
                .then(|| self.read_temperature_c(transport))
                .transpose()?
                .flatten(),
            biases,
        })
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

/// Temperature ADC and buffer bring-up, from OpenEB's
/// `TzImx636::temperature_init`. Runs once before the first temperature read.
const IMX636_TEMPERATURE_ADC_INIT: &[RegOp] = &[
    RegOp::write_field(REG_ADC_CONTROL, ADC_EN, ADC_EN),
    RegOp::write_field(REG_ADC_CONTROL, ADC_CLK_EN, ADC_CLK_EN),
    RegOp::write_field(REG_ADC_MISC_CTRL, ADC_BUF_CAL_EN, ADC_BUF_CAL_EN),
    RegOp::delay_us(100),
    RegOp::write_field(REG_TEMP_CTRL, TEMP_BUF_EN, TEMP_BUF_EN),
    RegOp::write_field(REG_TEMP_CTRL, TEMP_BUF_CAL_EN, TEMP_BUF_CAL_EN),
    RegOp::delay_us(100),
    RegOp::write_field(REG_ADC_CONTROL, 0, ADC_CLK_EN),
];

/// Ungate the ADC clock, select the temperature channel, start one conversion.
const IMX636_TEMPERATURE_ADC_START: &[RegOp] = &[
    RegOp::write_field(REG_ADC_CONTROL, ADC_CLK_EN, ADC_CLK_EN),
    RegOp::write_field(REG_ADC_MISC_CTRL, ADC_TEMP, ADC_TEMP),
    RegOp::write_field(REG_ADC_CONTROL, ADC_START, ADC_START),
];

const IMX636_EXTERNAL_TRIGGER_ENABLE_CH0: &[RegOp] = &[
    RegOp::write_field(REG_DIG_PAD2_CTRL, 0xF000, 0xF000),
    RegOp::write_field(REG_EDF_RESERVED_7004, 0x0400, 0x0400),
];

const IMX636_EXTERNAL_TRIGGER_DISABLE_CH0: &[RegOp] =
    &[RegOp::write_field(REG_EDF_RESERVED_7004, 0, 0x0400)];

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_trigger_enable_sequence_enables_channel_zero() {
        let cfg = ExternalTriggerConfig {
            enabled: true,
            channel: 0,
        };

        let sequence = Imx636::external_trigger_sequence(&cfg).expect("channel 0 is supported");

        assert_eq!(
            sequence,
            [
                RegOp::write_field(REG_DIG_PAD2_CTRL, 0xF000, 0xF000),
                RegOp::write_field(REG_EDF_RESERVED_7004, 0x0400, 0x0400),
            ]
        );
    }

    #[test]
    fn external_trigger_disable_sequence_clears_channel_zero() {
        let cfg = ExternalTriggerConfig {
            enabled: false,
            channel: 0,
        };

        let sequence = Imx636::external_trigger_sequence(&cfg).expect("channel 0 is supported");

        assert_eq!(
            sequence,
            [RegOp::write_field(REG_EDF_RESERVED_7004, 0, 0x0400)]
        );
    }

    #[test]
    fn refractory_fields_do_not_overlap_and_cover_the_counter() {
        // A misplaced bit here would silently turn a valid-flag bit into part
        // of the counter and skew every dead-time reading.
        assert_eq!(REFR_COUNTER_MASK, 0x0FFF_FFFF);
        assert_eq!(
            REFR_COUNTER_MASK & (REFR_VALID | REFR_CNT_EN | REFR_EN),
            0,
            "control and status bits must sit outside the counter",
        );
        assert_eq!(
            (REFR_VALID | REFR_CNT_EN | REFR_EN).count_ones(),
            3,
            "the three control/status bits must be distinct",
        );
    }

    #[test]
    fn refractory_counter_converts_to_microseconds() {
        // 200 counts per µs: one full microsecond, and the 6.35 µs a default
        // refr bias produces on a typical unit.
        assert_eq!(200.0 / REFRACTORY_TICKS_PER_US, 1.0);
        assert_eq!(1270.0 / REFRACTORY_TICKS_PER_US, 6.35);
    }

    #[test]
    fn temperature_code_converts_to_celsius() {
        let celsius = |code: u32| 0.190 * code as f32 - 56.0;
        assert!((celsius(512) - 41.28).abs() < 0.01);
        // The ADC result is 10 bits; anything wider would be a field-mask bug.
        assert_eq!(ADC_DAC_DYN_MASK, 0x3FF);
        assert_eq!(ADC_DAC_DYN_MASK & ADC_DONE_DYN, 0);
    }

    #[test]
    fn external_trigger_sequence_rejects_other_channels() {
        let cfg = ExternalTriggerConfig {
            enabled: true,
            channel: 1,
        };

        let err = Imx636::external_trigger_sequence(&cfg).expect_err("channel must be rejected");

        assert!(err.to_string().contains("channel 0"));
    }
}
