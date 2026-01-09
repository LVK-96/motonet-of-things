use cc1101::{Cc1101, GdoCfg, ModulationFormat, PacketLength, SyncMode};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use esp_hal::Blocking;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::gpio::{InputPin, OutputPin};
use esp_hal::spi::{Mode, master::Spi};
use esp_hal::time::Rate;

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

    // Apply default configuration
    cc1101.set_defaults().unwrap();

    // Configure for 433 MHz OOK operation
    cc1101.set_frequency(433_920_000).unwrap(); // 433.92 MHz
    cc1101
        .set_modulation_format(ModulationFormat::AmplitudeShiftOnOffKeying)
        .unwrap();
    cc1101.set_sync_mode(SyncMode::Disabled).unwrap(); // No sync for raw mode
    cc1101.set_packet_length(PacketLength::Infinite).unwrap();

    // Configure GDO0 for raw serial data output (async OOK mode)
    cc1101.set_gdo0_config(GdoCfg::SERIAL_DATA_OUT).unwrap();

    // Module status data input
    let gdo0 = Input::new(gdo0, InputConfig::default().with_pull(Pull::Down));
    let gdo2 = Input::new(gdo2, InputConfig::default().with_pull(Pull::Down));

    (cc1101, gdo0, gdo2)
}
