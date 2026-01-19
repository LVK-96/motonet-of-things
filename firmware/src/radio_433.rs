use core::future::Future;

use cc1101::{
    Cc1101, DecisionBoundary, FilterLength, ModulationFormat, PacketLength, RadioMode, SyncMode,
    TargetAmplitude,
};
use embedded_hal::digital::InputPin;
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use esp_hal::Blocking;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::gpio::{InputPin as EspInputPin, OutputPin};
use esp_hal::spi::{Mode, master::Spi};
use esp_hal::time::Rate;

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
    fn get_hw_info(&mut self) -> impl Future<Output = Result<(u8, u8), RadioError>>;

    /// Set the radio to receive mode
    fn set_receive_mode(&mut self) -> impl Future<Output = Result<(), RadioError>>;

    /// Get current RSSI in dBm
    fn get_rssi_dbm(&mut self) -> impl Future<Output = Result<i16, RadioError>>;

    /// Get the configured detection threshold in dB.
    /// This is the minimum signal-to-noise ratio required for OOK detection.
    fn get_detection_threshold(&self) -> u8;

    /// Set the detection threshold (decision boundary) for OOK detection.
    /// Valid values: 4, 8, 12, 16 dB
    fn set_detection_threshold(&mut self, db: u8) -> impl Future<Output = Result<(), RadioError>>;

    /// Get the current filter output level (AGC target amplitude).
    /// Returns 0-7, corresponding to 24-42 dB.
    fn get_filter_level(&self) -> u8;

    /// Set the filter output level (AGC target amplitude).
    /// Valid values: 0-7 (corresponding to 24, 27, 30, 33, 36, 38, 40, 42 dB)
    fn set_filter_level(&mut self, level: u8) -> impl Future<Output = Result<(), RadioError>>;

    /// Take ownership of the data pin for use with PulseCapture.
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
}

impl Cc1101Radio {
    /// Create and configure a new CC1101 radio instance.
    ///
    /// Configures the radio for 433.92 MHz OOK reception suitable for
    /// weather sensors like Rubicson.
    pub fn new(
        spimaster: impl esp_hal::spi::master::Instance + 'static,
        sck: impl OutputPin + 'static,
        mosi: impl OutputPin + 'static,
        miso: impl EspInputPin + 'static,
        cs: impl OutputPin + 'static,
        gdo0: impl EspInputPin + 'static,
        gdo2: impl EspInputPin + 'static,
    ) -> Result<Self, RadioError> {
        // Configure SPI
        let spi = Spi::new(
            spimaster,
            esp_hal::spi::master::Config::default()
                .with_frequency(Rate::from_hz(1_000_000))
                .with_mode(Mode::_0),
        )
        .map_err(|_| RadioError::Spi)?
        .with_sck(sck)
        .with_mosi(mosi)
        .with_miso(miso);

        // Chip select (active low)
        let cs = Output::new(cs, Level::High, OutputConfig::default());

        // Wrap SPI + CS into SpiDevice
        let spi_device = ExclusiveDevice::new_no_delay(spi, cs).map_err(|_| RadioError::Spi)?;

        // Create CC1101 driver
        let mut driver = Cc1101::new(spi_device).map_err(|_| RadioError::NotResponding)?;

        // Reset and configure
        driver.reset_chip().map_err(|_| RadioError::ConfigError)?;
        driver.set_defaults().map_err(|_| RadioError::ConfigError)?;

        // Configure for 433 MHz OOK operation
        driver
            .set_frequency(433_920_000)
            .map_err(|_| RadioError::ConfigError)?;
        driver
            .set_modulation_format(ModulationFormat::AmplitudeShiftOnOffKeying)
            .map_err(|_| RadioError::ConfigError)?;
        driver
            .set_sync_mode(SyncMode::Disabled)
            .map_err(|_| RadioError::ConfigError)?;
        driver
            .set_packet_length(PacketLength::Infinite)
            .map_err(|_| RadioError::ConfigError)?;

        // OOK-specific settings for Rubicson reception
        driver
            .set_data_rate(4800)
            .map_err(|_| RadioError::ConfigError)?;
        driver
            .set_channel_bandwidth(325_000)
            .map_err(|_| RadioError::ConfigError)?;
        driver
            .set_magn_target(TargetAmplitude::Db42)
            .map_err(|_| RadioError::ConfigError)?;
        driver
            .set_filter_length(FilterLength::AmplitudeModulation(DecisionBoundary::Db16))
            .map_err(|_| RadioError::ConfigError)?;

        // Enable async serial mode for raw OOK data output
        driver.set_raw_mode().map_err(|_| RadioError::ConfigError)?;

        // Configure GPIO pins
        let gdo0 = Input::new(gdo0, InputConfig::default().with_pull(Pull::Down));
        let gdo2 = Input::new(gdo2, InputConfig::default().with_pull(Pull::Down));

        Ok(Self {
            driver,
            data_pin: Some(gdo0),
            gdo2,
            detection_threshold_db: 16, // Default from DecisionBoundary::Db16
            filter_level: 7,            // Default from TargetAmplitude::Db42
        })
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
        self.detection_threshold_db
    }

    fn set_detection_threshold(&mut self, db: u8) -> impl Future<Output = Result<(), RadioError>> {
        // Map dB value to DecisionBoundary enum
        let boundary = match db {
            0..=6 => DecisionBoundary::Db4,
            7..=10 => DecisionBoundary::Db8,
            11..=14 => DecisionBoundary::Db12,
            _ => DecisionBoundary::Db16, // 15+ dB, default to max
        };

        let result = self
            .driver
            .set_filter_length(FilterLength::AmplitudeModulation(boundary))
            .map_err(|_| RadioError::ConfigError);

        if result.is_ok() {
            self.detection_threshold_db = db;
        }

        async move { result }
    }

    fn get_filter_level(&self) -> u8 {
        self.filter_level
    }

    fn set_filter_level(&mut self, level: u8) -> impl Future<Output = Result<(), RadioError>> {
        // Map 0-7 to TargetAmplitude enum (24-42 dB)
        let target = match level {
            0 => TargetAmplitude::Db24,
            1 => TargetAmplitude::Db27,
            2 => TargetAmplitude::Db30,
            3 => TargetAmplitude::Db33,
            4 => TargetAmplitude::Db36,
            5 => TargetAmplitude::Db38,
            6 => TargetAmplitude::Db40,
            _ => TargetAmplitude::Db42, // 7 or higher
        };

        let result = self
            .driver
            .set_magn_target(target)
            .map_err(|_| RadioError::ConfigError);

        if result.is_ok() {
            self.filter_level = level.min(7);
        }

        async move { result }
    }

    fn take_data_pin(&mut self) -> Option<Self::DataPin> {
        self.data_pin.take()
    }
}
