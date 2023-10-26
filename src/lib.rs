#![no_std]

mod registers;
use embedded_hal_async::i2c::I2c;
use registers::{
    AutoCalibrationCompensationBackEmfReg, AutoCalibrationCompensationReg,
    BrakeTimeOffsetReg, Control1Reg, Control2Reg, Control3Reg, Control4Reg, Control5Reg,
    FeedbackControlReg, GoReg, LibrarySelectionReg, ModeReg, OverdriveClampReg,
    OverdriveTimeOffsetReg, RatedVoltageReg, RealTimePlaybackInputReg, Register, StatusReg,
    SustainTimeOffsetNegativeReg, SustainTimeOffsetPositiveReg, Waveform0Reg,
};
pub use registers::{Effect, Library};

/// A Texas instruments Drv2605 haptic motor driver for LRA and ERM motors
pub struct Drv2605l<I2C, E>
where
    I2C: I2c<Error = E>,
{
    i2c: I2C,
    lra: bool,
}

#[allow(unused)]
impl<I2C, E> Drv2605l<I2C, E>
where
    I2C: I2c<Error = E>,
{
    /// Returns a calibrated Drv2605l device configured to standby mode for
    /// power savings. Closed loop is hardcoded for all motors and modes except
    /// ERM motors in rom mode where open loop is automatically enabled.
    ///
    /// Use a `set_mode` and `set_go` to trigger a vibration.
    pub async fn new(
        i2c: I2C,
        calibration: Calibration,
        lra: bool,
    ) -> Result<Self, DrvError> {
        let mut haptic = Self { i2c, lra };
        haptic.check_id(7).await?;

        // todo reset so registers are defaulted. Currently timing out..  need a
        // solution for delaying and retrying. Currently we send default values
        // to all registers we track so were probably fine without it for now
        // haptic.reset()?;

        match calibration {
            // device will get c/alibration values out of the otp if the otp bit is set
            Calibration::Otp => {
                if !haptic.is_otp().await? {
                    return Err(DrvError::OTPNotProgrammed);
                }
            }
            // load up previously calibrated values
            Calibration::Load(c) => haptic.set_calibration(c).await?,
            Calibration::Auto(c) => {
                let mut feedback: FeedbackControlReg = Default::default();
                let mut ctrl2: Control2Reg = Default::default();
                let mut ctrl4: Control4Reg = Default::default();
                let mut ctrl1: Control1Reg = Default::default();

                let mut rated = RatedVoltageReg(c.rated_voltage);
                let mut clamp = OverdriveClampReg(c.overdrive_voltage_clamp);

                feedback.set_fb_brake_factor(c.brake_factor);
                feedback.set_loop_gain(c.loop_gain);
                if (lra) {
                    feedback.set_n_erm_lra(true);
                }
                ctrl2.set_sample_time(c.lra_sample_time);
                ctrl2.set_blanking_time(c.lra_blanking_time);
                ctrl2.set_idiss_time(c.lra_idiss_time);
                ctrl4.set_auto_cal_time(c.auto_cal_time);
                ctrl4.set_zc_det_time(c.lra_zc_det_time);
                ctrl1.set_drive_time(c.drive_time);

                haptic.write(feedback).await?;
                haptic.write(ctrl2).await?;
                haptic.write(ctrl4).await?;
                haptic.write(rated).await?;
                haptic.write(clamp).await?;
                haptic.write(ctrl1).await?;
                haptic.calibrate().await?;
            }
        }

        haptic.set_standby(true).await?;

        Ok(haptic)
    }

    pub async fn set_mode(&mut self, mode: Mode) -> Result<(), DrvError> {
        let mut m: ModeReg = self.read().await?;

        let mut ctrl3: Control3Reg = self.read().await?;

        match mode {
            Mode::Pwm => {
                // unset in case coming from rom mode
                if !self.lra {
                    ctrl3.set_erm_open_loop(false);
                }
                ctrl3.set_n_pwm_analog(false);
                self.write(ctrl3).await?;

                m.set_mode(registers::Mode::PwmInputAndAnalogInput as u8);
                self.write(m).await
            }
            Mode::Rom(library, options) => {
                let mut ctrl5: Control5Reg = self.read().await?;
                ctrl5.set_playback_interval(options.decrease_playback_interval);
                self.write(ctrl5).await?;

                let mut overdrive = OverdriveTimeOffsetReg(options.overdrive_time_offset);
                self.write(overdrive).await?;

                let mut sustain_p =
                    SustainTimeOffsetPositiveReg(options.sustain_positive_offset);
                self.write(sustain_p).await?;

                let mut sustain_n =
                    SustainTimeOffsetNegativeReg(options.sustain_negative_offset);
                self.write(sustain_n).await?;

                let mut brake = BrakeTimeOffsetReg(options.brake_time_offset);
                self.write(brake).await?;

                // erm requires open loop mode
                if !self.lra {
                    ctrl3.set_erm_open_loop(true);
                }
                self.write(ctrl3).await?;

                let mut lib: LibrarySelectionReg = self.read().await?;
                lib.set_library_selection(library as u8);
                self.write(lib).await?;

                m.set_mode(registers::Mode::InternalTrigger as u8);
                self.write(m).await
            }
            Mode::Analog => {
                // unset in case coming from rom mode
                if !self.lra {
                    ctrl3.set_erm_open_loop(false);
                }
                ctrl3.set_n_pwm_analog(true);
                self.write(ctrl3).await?;

                m.set_mode(registers::Mode::PwmInputAndAnalogInput as u8);
                self.write(m).await
            }
            Mode::RealTimePlayback => {
                // We won't need to unset as no other modes use this bit
                ctrl3.set_data_format_rtp(true);
                // unset in case coming from rom mode
                if !self.lra {
                    ctrl3.set_erm_open_loop(false);
                }
                self.write(ctrl3).await?;

                m.set_mode(registers::Mode::RealTimePlayback as u8);
                self.write(m).await
            }
        }
    }

    /// Sets up to 8 Effects to play in order when `set_go` is called. Stops
    /// playing early if `Effect::None` is used.
    // todo dont hardcode to 8, pass slice? but then need to assert <=8
    pub async fn set_rom(&mut self, roms: &[Effect; 8]) -> Result<(), DrvError> {
        let buf: [u8; 9] = [
            Waveform0Reg::ADDRESS,
            roms[0].into(),
            roms[1].into(),
            roms[2].into(),
            roms[3].into(),
            roms[4].into(),
            roms[5].into(),
            roms[6].into(),
            roms[7].into(),
        ];
        self.i2c
            .write(ADDRESS, &buf)
            .await
            .map_err(|_| DrvError::ConnectionError)
    }

    /// Set a single `Effect` into rom storage during rom mode when `set_go` is
    /// called
    pub async fn set_rom_single(&mut self, rom: Effect) -> Result<(), DrvError> {
        let buf: [u8; 3] = [Waveform0Reg::ADDRESS, rom.into(), Effect::Stop.into()];
        self.i2c
            .write(ADDRESS, &buf)
            .await
            .map_err(|_| DrvError::ConnectionError)
    }

    /// Change the duty cycle for rtp mode
    pub async fn set_rtp(&mut self, duty: u8) -> Result<(), DrvError> {
        let rtp = RealTimePlaybackInputReg(duty);
        self.write(rtp).await
    }

    /// Get the current rtp duty cycle
    pub async fn rtp(&mut self) -> Result<u8, DrvError> {
        let rtp: RealTimePlaybackInputReg = self.read().await?;

        Ok(rtp.value())
    }

    /// Trigger a GO for whatever mode is enabled
    pub async fn set_go(&mut self) -> Result<(), DrvError> {
        let mut go: GoReg = self.read().await?;

        go.set_go(true);
        self.write(go).await
    }

    /// Get the go bit. For some modes the go bit can be polled to see when it
    /// clears indicating a waveform has completed playback.
    pub async fn go(&mut self) -> Result<bool, DrvError> {
        Ok(self.read::<GoReg>().await?.go())
    }

    /// Enabling standby goes into a low power state but maintains all mode
    /// configuration
    pub async fn set_standby(&mut self, enable: bool) -> Result<(), DrvError> {
        let mut mode: ModeReg = self.read().await?;
        mode.set_standby(enable);
        self.write(mode).await
    }

    /// Get the status bits
    pub async fn status(&mut self) -> Result<u8, DrvError> {
        let status: StatusReg = self.read().await?;
        Ok(status.value())
    }

    /// Get the LoadParams that were loaded at startup or calculated via
    /// Calibration
    pub async fn calibration(&mut self) -> Result<LoadParams, DrvError> {
        let feedback: FeedbackControlReg = self.read().await?;

        let compenstation: AutoCalibrationCompensationReg = self.read().await?;
        let back_emf: AutoCalibrationCompensationBackEmfReg = self.read().await?;

        Ok(LoadParams {
            back_emf_gain: feedback.bemf_gain(),
            compenstation: compenstation.value(),
            back_emf: back_emf.value(),
        })
    }

    /* Private calls */

    /// Write `value` to `register`
    async fn write<REG>(&mut self, register: REG) -> Result<(), DrvError>
    where
        REG: Register,
    {
        self.i2c
            .write(ADDRESS, &[REG::ADDRESS, register.value()])
            .await
            .map_err(|_| DrvError::ConnectionError)
    }

    /// Read the register
    async fn read<REG>(&mut self) -> Result<REG, DrvError>
    where
        REG: Register + From<u8>,
    {
        let mut buf = [0u8; 1];
        self.i2c
            .write_read(ADDRESS, &[REG::ADDRESS], &mut buf)
            .await
            .map_err(|_| DrvError::ConnectionError)?;
        Ok(buf[0].into())
    }

    async fn check_id(&mut self, id: u8) -> Result<(), DrvError> {
        let reg = StatusReg(self.status().await?);
        if reg.device_id() != id {
            return Err(DrvError::WrongDeviceId);
        }

        Ok(())
    }

    // performs the equivalent operation of power cycling the device. Any
    // playback operations are immediately interrupted, and all registers are
    // reset to the default values.
    async fn reset(&mut self) -> Result<(), DrvError> {
        let mut mode = ModeReg::default();
        mode.set_dev_reset(true);
        self.write(mode).await?;

        while self.read::<ModeReg>().await?.dev_reset() {}

        Ok(())
    }

    /// Send calibration `LoadParams`
    async fn set_calibration(&mut self, load: LoadParams) -> Result<(), DrvError> {
        let mut fbcr: FeedbackControlReg = self.read().await?;
        fbcr.set_bemf_gain(load.back_emf_gain);
        self.write(fbcr).await?;

        let auto_cal_comp = AutoCalibrationCompensationReg(load.compenstation);
        self.write(auto_cal_comp).await?;

        let back_emf = AutoCalibrationCompensationBackEmfReg(load.back_emf);
        self.write(back_emf).await
    }

    /// Run diagnostics
    async fn diagnostics(&mut self) -> Result<(), DrvError> {
        let mut mode: ModeReg = self.read().await?;
        mode.set_standby(false);
        mode.set_mode(registers::Mode::Diagnostics as u8);
        self.write(mode).await?;

        self.set_go().await?;

        //todo timeout
        while self.read::<GoReg>().await?.go() {}

        let reg = StatusReg(self.status().await?);
        if reg.diagnostic_result() {
            return Err(DrvError::DeviceDiagnosticFailed);
        }

        Ok(())
    }

    /// Run auto calibration which and return the resulting LoadParams
    async fn calibrate(&mut self) -> Result<LoadParams, DrvError> {
        let mut mode: ModeReg = self.read().await?;
        mode.set_standby(false);
        mode.set_mode(registers::Mode::AutoCalibration as u8);
        self.write(mode).await?;

        self.set_go().await?;

        //todo timeout
        while self.read::<GoReg>().await?.go() {}

        let reg = StatusReg(self.status().await?);
        if reg.diagnostic_result() {
            return Err(DrvError::CalibrationFailed);
        }

        self.calibration().await
    }

    /// Check if the device's LoadParams have been set in the nonvolatile memory
    async fn is_otp(&mut self) -> Result<bool, DrvError> {
        let reg4: Control4Reg = self.read().await?;
        Ok(reg4.otp_status())
    }
}

/// Possible runtime errors
#[allow(unused)]
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(::defmt::Format))]
pub enum DrvError {
    WrongMotorType,
    WrongDeviceId,
    ConnectionError,
    DeviceDiagnosticFailed,
    CalibrationFailed,
    OTPNotProgrammed,
}

/// The hardcoded address of the driver.  All drivers share the same address so
/// that it is possible to broadcast on the bus and have multiple units emit the
/// same waveform
const ADDRESS: u8 = 0x5a;

/// Selection of calibration options required for initial device construction
#[cfg_attr(feature = "defmt", derive(::defmt::Format))]
pub enum Calibration {
    /// Many calibration params can be defaulted, and maybe the entire thing for
    /// some motors. Required params for LRA motors especially though should
    /// calculated from the drv2605l and motor datasheets.
    ///
    /// NOTE: In general, but when doing autocalibration, be sure to secure the
    /// motor to some kind of mass. It can't calibrate if it is jumping around
    /// on a board or a desk.
    Auto(CalibrationParams),
    /// Load previously calibrated values. It is common to do an autocalibration
    /// and then read back the calibration parameters so you can hardcode them
    Load(LoadParams),
    /// Values were previously programmed into nonvolatile memory. This is not common.
    Otp,
}

/// Previously computed calibration parameters. Can be fetched after calibration
/// and hardcoded during construction instead of auto calibration.
#[cfg_attr(feature = "defmt", derive(::defmt::Format))]
pub struct LoadParams {
    /// Auto-Calibration Compensation Result
    pub compenstation: u8,
    /// Auto-Calibration Back-EMF Result
    pub back_emf: u8,
    /// Auto-Calibration BEMF_GAIN Result
    pub back_emf_gain: u8,
}

/// Calibration configuration for both ERM and LRA motor types. Some params
/// really need to be computed from the drv2605l and motor datasheets,
/// especially for LRA motors
#[non_exhaustive]
#[cfg_attr(feature = "defmt", derive(::defmt::Format))]
pub struct CalibrationParams {
    /// Required: Datasheet 8.5.2.1 Rated Voltage Programming
    pub rated_voltage: u8,
    /// Required: Datasheet 8.5.2.2 Overdrive Voltage-Clamp Programming
    pub overdrive_voltage_clamp: u8,
    /// Required: Datasheet 8.5.1.1 Drive-Time Programming
    pub drive_time: u8,
    /// Default advised: Brake Factor
    pub brake_factor: u8,
    /// Default advised: Loop-Gain Control
    pub loop_gain: u8,
    /// Default advised: Auto Calibration Time Adjustment
    pub auto_cal_time: u8,
    /// Default advised: LRA auto-resonance sampling time
    pub lra_sample_time: u8,
    /// Default advised: LRA auto-resonance sampling time
    pub lra_blanking_time: u8,
    /// Default advised: LRA Current dissipation time
    pub lra_idiss_time: u8,
    /// Default advised: LRA Zero Crossing Detect
    pub lra_zc_det_time: u8,
}

impl Default for CalibrationParams {
    fn default() -> Self {
        Self {
            brake_factor: 2,
            loop_gain: 2,
            lra_sample_time: 3,
            lra_blanking_time: 1,
            lra_idiss_time: 1,
            auto_cal_time: 3,
            lra_zc_det_time: 0,
            rated_voltage: 0x3E,
            overdrive_voltage_clamp: 0x8C,
            drive_time: 0x13,
        }
    }
}

/// Advanced configuration for rom waveforms offering time stretching (or time
/// shrinking) to the built in waveforms
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(::defmt::Format))]
pub struct RomParams {
    /// Overdrive Time Offset (ms) = overdrive_time * playback_interval
    pub overdrive_time_offset: u8,
    /// Sustain-Time Positive Offset (ms) = sustain_positive_offset * playback_interval
    pub sustain_positive_offset: u8,
    /// Sustain-Time Negative Offset (ms) = sustain_negative_time * playback_interval
    pub sustain_negative_offset: u8,
    /// Bake Time Offset (ms) = brake_time_offset * playback_interval
    pub brake_time_offset: u8,
    /// Default Playback Interval. By default each waveform in memory has a
    /// granularity of 5 ms, but can be decreased to 1ms by enabling
    /// decrease_playback_interval to 1ms
    pub decrease_playback_interval: bool,
}

impl Default for RomParams {
    fn default() -> Self {
        Self {
            overdrive_time_offset: 0,
            sustain_positive_offset: 0,
            sustain_negative_offset: 0,
            brake_time_offset: 0,
            decrease_playback_interval: false,
        }
    }
}

/// Selection of modes of device operation, some of which take their
/// configuration via the enum
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(::defmt::Format))]
pub enum Mode {
    /// Select the Immersion TS2200 library that matches your motor
    /// characteristic. For ERM Motors, open loop operation will be enabled as
    /// all ERM libraries are tuned for open loop.
    ///
    /// Use set rom setters and then GO bit to play an `Effect`
    Rom(Library, RomParams),
    /// Enable Pulse Width Modulated mod (closed loop unidirectional )
    ///
    /// 0% full braking, 50% 1/2 Rated Voltage, 100% Rated Voltage
    Pwm,
    /// Set analog input mode.
    ///
    /// Send an analog voltage to the IN/TRIG to set a duty cycle which will
    /// persist until mode change or standby. The reference voltage in standby
    /// mode is 1.8 V thus 100% is 1.8V, 50% is .9V, 0% is 0V analogous to the
    /// duty-cycle percentage in PWM mode
    Analog,
    /// Enable Real Time Playback (closed loop unidirectional unsigned )
    ///
    /// Use `set_rtp` to update the duty cycle which will persist until another
    /// call to `set_rtp`, change to standby, or mode change.
    /// 0x00 full braking, 0x7F 1/2 Rated Voltage, 0xFF Rated Voltage
    RealTimePlayback,
}
