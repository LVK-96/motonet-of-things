use cc1101::{Cc1101, Modulation, PacketLength, SyncMode};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use esp_hal::Blocking;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::gpio::{InputPin, OutputPin};
use esp_hal::spi::{Mode, master::Spi};
use esp_hal::time::Rate;

/// GDO output configuration values (from CC1101 datasheet)
pub mod gdo_config {
    /// Asserts when sync word has been sent / received
    pub const SYNC_WORD: u8 = 0x06;
    /// Asserts when a packet has been received with CRC OK
    pub const CRC_OK: u8 = 0x07;
    /// High impedance (tri-state)
    pub const HIGH_Z: u8 = 0x2F;
}

pub fn setup_cc1101(
    spimaster: impl esp_hal::spi::master::Instance + 'static,
    sck: impl OutputPin + 'static,
    mosi: impl OutputPin + 'static,
    miso: impl InputPin + 'static,
    cs: impl OutputPin + 'static,
    gdo0: impl InputPin + 'static,
    gdo2: impl InputPin + 'static,
) -> (
    Cc1101<ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, NoDelay>>,
    Input<'static>, // GDO0
    Input<'static>, // GDO2
) {
    // Configure SPI
    let spi = Spi::new(
        spimaster,
        esp_hal::spi::master::Config::default()
            .with_frequency(Rate::from_hz(1_000_000)) // CC1101 max is ~10MHz, start low
            .with_mode(Mode::_0), // CPOL=0, CPHA=0
    )
    .unwrap()
    .with_sck(sck)
    .with_mosi(mosi)
    .with_miso(miso);

    // Chip select (active low)
    let cs = Output::new(
        cs,
        Level::High, // Start deselected
        OutputConfig::default(),
    );

    // Wrap SPI + CS into SpiDevice (required by cc1101 crate)
    let spi_device = ExclusiveDevice::new_no_delay(spi, cs).unwrap();

    // Create CC1101 driver
    let mut cc1101 = Cc1101::new(spi_device).unwrap();

    // Apply default configuration (sets up GDO pins, FIFOs, etc.)
    // This should configure GDO0 to a useful signal
    cc1101.set_defaults().unwrap();

    // Configure for 433 MHz operation
    cc1101.set_frequency(433_920_000).unwrap(); // 433.92 MHz
    cc1101.set_modulation(Modulation::OnOffKeying).unwrap();
    cc1101.set_sync_mode(SyncMode::MatchFull(0xD391)).unwrap();
    cc1101
        .set_packet_length(PacketLength::Variable(61))
        .unwrap();

    // Module status data input
    let gdo0 = Input::new(gdo0, InputConfig::default().with_pull(Pull::Down));
    let gdo2 = Input::new(gdo2, InputConfig::default().with_pull(Pull::Down));

    (cc1101, gdo0, gdo2)
}
