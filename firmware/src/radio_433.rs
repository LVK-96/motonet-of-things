use core::future::Future;

use cc1101::{
    AutoCalibration, Cc1101, DecisionBoundary, FilterLength, GdoCfg, MaxLnaGain, ModulationFormat,
    PacketLength, RadioMode, SyncMode, TargetAmplitude,
};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use embedded_hal::digital::InputPin;
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use esp_hal::Blocking;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::gpio::{InputPin as EspInputPin, OutputPin};
use esp_hal::spi::{Mode, master::Spi};
use esp_hal::time::Rate;

use crate::messages::{
    CARRIER_SENSE_MAX, CHANNEL_BANDWIDTH_MAX_INDEX, DEFAULT_RADIO_SETTINGS, MAGN_TARGET_MAX,
    channel_bandwidth_hz, channel_bandwidth_index, quantize_detection_threshold_db,
};

/// Error type for radio operations
#[derive(Debug, defmt::Format)]
pub enum RadioError {
    /// SPI communication error
    Spi,
    /// Radio not responding
    NotResponding,
    /// Configuration error
    ConfigError,
}

/// Async trait for 433 MHz OOK radio receivers.
///
/// This trait abstracts the radio hardware, allowing different
/// implementations (CC1101, RXB6, etc.) to be used interchangeably.
pub trait Radio433 {
    /// The type of the data output pin (e.g., GDO0 for CC1101)
    type DataPin: InputPin;

    /// Get hardware info (part number, version)
    ///
    /// # Errors
    ///
    /// Returns `RadioError` if SPI fails
    fn get_hw_info(&mut self) -> impl Future<Output = Result<(u8, u8), RadioError>>;

    /// Set the radio to receive mode
    ///
    /// # Errors
    ///
    /// Returns `RadioError` if command fails
    fn set_receive_mode(&mut self) -> impl Future<Output = Result<(), RadioError>>;

    /// Get current RSSI in dBm
    ///
    /// # Errors
    ///
    /// Returns `RadioError` if SPI fails
    fn get_rssi_dbm(&mut self) -> impl Future<Output = Result<i16, RadioError>>;

    /// Get the configured detection threshold in dB.
    /// This is the minimum signal-to-noise ratio required for OOK detection.
    fn get_detection_threshold(&self) -> u8;

    /// Set the detection threshold (decision boundary) for OOK detection.
    /// Valid values: 4, 8, 12, 16 dB
    ///
    /// # Errors
    ///
    /// Returns `RadioError` if configuration fails
    fn set_detection_threshold(&mut self, db: u8) -> Result<(), RadioError>;

    /// Get the current filter output level (AGC target amplitude).
    /// Returns 0-7, corresponding to 24-42 dB.
    fn get_filter_level(&self) -> u8;

    /// Set the filter output level (AGC target amplitude).
    /// Valid values: 0-7 (corresponding to 24, 27, 30, 33, 36, 38, 40, 42 dB)
    ///
    /// # Errors
    ///
    /// Returns `RadioError` if configuration fails
    fn set_filter_level(&mut self, level: u8) -> Result<(), RadioError>;

    /// Get the current channel bandwidth option index (0-3).
    fn get_channel_bandwidth_index(&self) -> u8;

    /// Set the channel bandwidth option index (0-3).
    ///
    /// # Errors
    ///
    /// Returns `RadioError` if configuration fails.
    fn set_channel_bandwidth_index(&mut self, index: u8) -> Result<(), RadioError>;

    /// Get the current carrier sense threshold (0-7).
    /// 0 = at `MAGN_TARGET`, 1 = +1 dB above, ..., 7 = +7 dB above
    fn get_carrier_sense_threshold(&self) -> u8;

    /// Set the carrier sense threshold (0-7).
    /// 0 = at `MAGN_TARGET`, 1 = +1 dB above, ..., 7 = +7 dB above
    /// Higher values require stronger signals to trigger carrier sense.
    ///
    /// # Errors
    ///
    /// Returns `RadioError` if configuration fails.
    fn set_carrier_sense_threshold(&mut self, threshold: u8) -> Result<(), RadioError>;

    /// Take ownership of the data pin for use with `PulseCapture`.
    /// Returns None if the pin has already been taken.
    fn take_data_pin(&mut self) -> Option<Self::DataPin>;
}

/// CC1101-based 433 MHz radio implementation.
pub struct Cc1101Radio {
    driver: Cc1101<ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, NoDelay>>,
    data_pin: Option<Input<'static>>,
    #[allow(dead_code)]
    gdo2: Input<'static>,
    detection_threshold_db: u8,
    filter_level: u8,
    channel_bandwidth_index: u8,
    carrier_sense_threshold: u8,
}

#[derive(Clone, Copy, Debug, defmt::Format)]
#[allow(clippy::struct_excessive_bools)]
pub struct SignalSnapshot {
    pub carrier_sense: bool,
    pub preamble_quality_reached: bool,
    pub pktstatus_gdo0: bool,
    pub pktstatus_gdo2: bool,
    pub pin_gdo0: bool,
    pub pin_gdo2: bool,
}

fn decision_boundary_for_threshold(db: u8) -> DecisionBoundary {
    match quantize_detection_threshold_db(db) {
        4 => DecisionBoundary::Db4,
        8 => DecisionBoundary::Db8,
        12 => DecisionBoundary::Db12,
        _ => DecisionBoundary::Db16,
    }
}

fn target_amplitude_for_level(level: u8) -> TargetAmplitude {
    match level {
        0 => TargetAmplitude::Db24,
        1 => TargetAmplitude::Db27,
        2 => TargetAmplitude::Db30,
        3 => TargetAmplitude::Db33,
        4 => TargetAmplitude::Db36,
        5 => TargetAmplitude::Db38,
        6 => TargetAmplitude::Db40,
        _ => TargetAmplitude::Db42, // 7 or higher
    }
}

impl Cc1101Radio {
    /// Create and configure a new CC1101 radio instance.
    ///
    /// Configures the radio for 433.92 MHz OOK reception suitable for
    /// weather sensors like Rubicson.
    ///
    /// # Panics
    ///
    /// Panics if SPI or GPIO configuration fails.
    #[allow(clippy::expect_used)]
    pub fn new(
        spimaster: impl esp_hal::spi::master::Instance + 'static,
        sck: impl OutputPin + 'static,
        mosi: impl OutputPin + 'static,
        miso: impl EspInputPin + 'static,
        cs: impl OutputPin + 'static,
        gdo0: impl EspInputPin + 'static,
        gdo2: impl EspInputPin + 'static,
    ) -> Self {
        // Configure SPI (this is internal to the ESP32 and won't fail with these parameters)
        let spi = Spi::new(
            spimaster,
            esp_hal::spi::master::Config::default()
                .with_frequency(Rate::from_hz(1_000_000))
                .with_mode(Mode::_0),
        )
        .expect("SPI configuration failed")
        .with_sck(sck)
        .with_mosi(mosi)
        .with_miso(miso);

        // Chip select (active low)
        let cs = Output::new(cs, Level::High, OutputConfig::default());

        // Wrap SPI + CS into SpiDevice
        let spi_device = ExclusiveDevice::new_no_delay(spi, cs).expect("SpiDevice creation failed");

        // Create CC1101 driver (this just wraps the SPI device, doesn't communicate)
        // Note: The library returns Result but new() is actually infallible
        let driver = Cc1101::new(spi_device).expect("Cc1101::new is infallible");

        // Configure GPIO pins
        let gdo0 = Input::new(gdo0, InputConfig::default().with_pull(Pull::Down));
        let gdo2 = Input::new(gdo2, InputConfig::default().with_pull(Pull::Down));
        let default_settings = DEFAULT_RADIO_SETTINGS;

        Self {
            driver,
            data_pin: Some(gdo0),
            gdo2,
            detection_threshold_db: default_settings.detection_threshold_db,
            filter_level: default_settings.magn_target,
            channel_bandwidth_index: default_settings.channel_bandwidth_index,
            carrier_sense_threshold: default_settings.carrier_sense_threshold,
        }
    }

    /// Initialize and configure the radio hardware.
    ///
    /// This performs the actual SPI communication and can be called multiple times.
    ///
    /// # Errors
    ///
    /// Returns `RadioError::ConfigError` if any configuration command fails.
    pub fn init(&mut self) -> Result<(), RadioError> {
        // Reset and configure
        self.driver
            .reset_chip()
            .map_err(|_| RadioError::ConfigError)?;

        // Configure for 433 MHz OOK operation
        self.driver
            .white_data_enable(false)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_freq_if(203_125)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_autocalibration(AutoCalibration::FromIdle)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_max_lna_gain(MaxLnaGain::BelowMax9_2)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_frequency(433_920_000)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_modulation_format(ModulationFormat::AmplitudeShiftOnOffKeying)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_sync_mode(SyncMode::Disabled)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_packet_length(PacketLength::Infinite)
            .map_err(|_| RadioError::ConfigError)?;

        // OOK-specific settings for Rubicson reception
        self.driver
            .set_data_rate(4800)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_channel_bandwidth(u64::from(channel_bandwidth_hz(
                self.channel_bandwidth_index,
            )))
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_magn_target(target_amplitude_for_level(self.filter_level))
            .map_err(|_| RadioError::ConfigError)?;
        let detection_threshold_db = quantize_detection_threshold_db(self.detection_threshold_db);
        self.driver
            .set_filter_length(FilterLength::AmplitudeModulation(
                decision_boundary_for_threshold(detection_threshold_db),
            ))
            .map_err(|_| RadioError::ConfigError)?;
        self.detection_threshold_db = detection_threshold_db;

        // Configure carrier sense threshold to filter noise
        self.driver
            .set_carrier_sense_threshold(self.carrier_sense_threshold)
            .map_err(|_| RadioError::ConfigError)?;

        self.driver
            .set_gdo0_config(GdoCfg::SERIAL_DATA_OUT)
            .map_err(|_| RadioError::ConfigError)?;

        // Enable async serial mode for raw OOK data output
        self.driver
            .set_raw_mode()
            .map_err(|_| RadioError::ConfigError)?;

        Ok(())
    }

    /// Read CC1101 packet-status bits together with current GPIO pin levels.
    ///
    /// # Errors
    ///
    /// Returns `RadioError::Spi` if SPI communcation to CC1101 fails
    pub fn signal_snapshot(&mut self) -> Result<SignalSnapshot, RadioError> {
        let status = self
            .driver
            .get_packet_status()
            .map_err(|_| RadioError::Spi)?;

        Ok(SignalSnapshot {
            carrier_sense: status.carrier_sense,
            preamble_quality_reached: status.preamble_quality_reached,
            pktstatus_gdo0: status.gdo0,
            pktstatus_gdo2: status.gdo2,
            pin_gdo0: self.data_pin.as_ref().is_some_and(Input::is_high),
            pin_gdo2: self.gdo2.is_high(),
        })
    }

    /// Apply a frequency/bandwidth profile and return to RX mode.
    ///
    /// # Errors
    ///
    /// Returns `RadioError::ConfigError` if any configuration command fails
    pub fn apply_rf_profile(&mut self, freq_hz: u32, bandwidth_hz: u32) -> Result<(), RadioError> {
        self.driver
            .set_radio_mode(RadioMode::Idle)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_frequency(u64::from(freq_hz))
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_channel_bandwidth(u64::from(bandwidth_hz))
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_radio_mode(RadioMode::Receive)
            .map_err(|_| RadioError::ConfigError)?;
        self.channel_bandwidth_index = channel_bandwidth_index(bandwidth_hz);
        Ok(())
    }

    /// Route a clock to GDO0 and verify that the MCU sees at least one edge.
    ///
    /// # Errors
    ///
    /// Returns `RadioError::ConfigError` if configurging CC1101 to output the clock fails
    pub async fn probe_gdo0_edge_path(&mut self) -> Result<bool, RadioError> {
        self.driver
            .set_gdo0_config(GdoCfg::CLK_XOSC_192)
            .map_err(|_| RadioError::ConfigError)?;

        // Let the new GDO0 function settle before waiting for an interrupt edge.
        Timer::after(Duration::from_micros(100)).await;

        let edge_seen = if let Some(pin) = self.data_pin.as_mut() {
            matches!(
                select(
                    pin.wait_for_any_edge(),
                    Timer::after(Duration::from_millis(20))
                )
                .await,
                Either::First(())
            )
        } else {
            false
        };

        self.driver
            .set_gdo0_config(GdoCfg::SERIAL_DATA_OUT)
            .map_err(|_| RadioError::ConfigError)?;

        Ok(edge_seen)
    }
}

impl Radio433 for Cc1101Radio {
    type DataPin = Input<'static>;

    fn get_hw_info(&mut self) -> impl Future<Output = Result<(u8, u8), RadioError>> {
        let result = self
            .driver
            .get_hw_info()
            .map_err(|_| RadioError::NotResponding);
        async move { result }
    }

    fn set_receive_mode(&mut self) -> impl Future<Output = Result<(), RadioError>> {
        let result = self
            .driver
            .set_radio_mode(RadioMode::Receive)
            .map_err(|_| RadioError::ConfigError);
        async move { result }
    }

    fn get_rssi_dbm(&mut self) -> impl Future<Output = Result<i16, RadioError>> {
        let result = self.driver.get_rssi_dbm().map_err(|_| RadioError::Spi);
        async move { result }
    }

    fn get_detection_threshold(&self) -> u8 {
        quantize_detection_threshold_db(self.detection_threshold_db)
    }

    fn set_detection_threshold(&mut self, db: u8) -> Result<(), RadioError> {
        let quantized_db = quantize_detection_threshold_db(db);
        self.driver
            .set_radio_mode(RadioMode::Idle)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_filter_length(FilterLength::AmplitudeModulation(
                decision_boundary_for_threshold(quantized_db),
            ))
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_radio_mode(RadioMode::Receive)
            .map_err(|_| RadioError::ConfigError)?;

        self.detection_threshold_db = quantized_db;
        Ok(())
    }

    fn get_filter_level(&self) -> u8 {
        self.filter_level
    }

    fn set_filter_level(&mut self, level: u8) -> Result<(), RadioError> {
        let clamped_level = level.min(MAGN_TARGET_MAX);
        self.driver
            .set_radio_mode(RadioMode::Idle)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_magn_target(target_amplitude_for_level(clamped_level))
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_radio_mode(RadioMode::Receive)
            .map_err(|_| RadioError::ConfigError)?;

        self.filter_level = clamped_level;
        Ok(())
    }

    fn get_channel_bandwidth_index(&self) -> u8 {
        self.channel_bandwidth_index
    }

    fn set_channel_bandwidth_index(&mut self, index: u8) -> Result<(), RadioError> {
        let clamped_index = index.min(CHANNEL_BANDWIDTH_MAX_INDEX);
        self.driver
            .set_radio_mode(RadioMode::Idle)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_channel_bandwidth(u64::from(channel_bandwidth_hz(clamped_index)))
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_radio_mode(RadioMode::Receive)
            .map_err(|_| RadioError::ConfigError)?;

        self.channel_bandwidth_index = clamped_index;
        Ok(())
    }

    fn get_carrier_sense_threshold(&self) -> u8 {
        self.carrier_sense_threshold
    }

    fn set_carrier_sense_threshold(&mut self, threshold: u8) -> Result<(), RadioError> {
        let clamped_threshold = threshold.min(CARRIER_SENSE_MAX);
        self.driver
            .set_radio_mode(RadioMode::Idle)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_carrier_sense_threshold(clamped_threshold)
            .map_err(|_| RadioError::ConfigError)?;
        self.driver
            .set_radio_mode(RadioMode::Receive)
            .map_err(|_| RadioError::ConfigError)?;

        self.carrier_sense_threshold = clamped_threshold;
        Ok(())
    }

    fn take_data_pin(&mut self) -> Option<Self::DataPin> {
        self.data_pin.take()
    }
}
