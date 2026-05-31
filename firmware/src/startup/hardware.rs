use defmt::info;
#[cfg(feature = "pulse_rmt")]
use esp_hal::Async;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::ledc::{
    LSGlobalClkSource, Ledc, LowSpeed,
    channel::{self, Channel as LedcChannel, ChannelIFace},
    timer::{self, TimerIFace},
};
use esp_hal::peripherals::Peripherals;
#[cfg(feature = "pulse_rmt")]
use esp_hal::rmt::{Channel as RmtChannel, Rmt, Rx, RxChannelConfig, RxChannelCreator};
use esp_hal::time::Rate;
use static_cell::StaticCell;

use crate::display_driver::{Display, Sh1106Display};
use crate::radio_433::{Cc1101Radio, Radio433};
use crate::ui_input::EC11RotaryEncoderInput;

pub(crate) fn system_setup() -> Peripherals {
    info!("Initializing system...");
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    info!("Initializing heap...");
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    info!("System setup complete!");

    peripherals
}

pub(crate) fn setup_led_channel(
    ledc: esp_hal::peripherals::LEDC<'static>,
    led_pin: esp_hal::peripherals::GPIO2<'static>,
) -> Option<LedcChannel<'static, LowSpeed>> {
    static LEDC_TIMER: StaticCell<timer::Timer<'static, LowSpeed>> = StaticCell::new();

    let mut ledc = Ledc::new(ledc);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    let lstimer0 = LEDC_TIMER.init(ledc.timer::<LowSpeed>(timer::Number::Timer0));
    let mut channel0 = ledc.channel(channel::Number::Channel0, led_pin);

    let timer_ok = lstimer0
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty8Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_khz(1),
        })
        .is_ok();

    if !timer_ok {
        return None;
    }

    if channel0
        .configure(channel::config::Config {
            timer: lstimer0,
            duty_pct: 5,
            drive_mode: DriveMode::PushPull,
        })
        .is_err()
    {
        return None;
    }

    Some(channel0)
}

pub(crate) fn setup_radio_433(
    spi2: esp_hal::peripherals::SPI2<'static>,
    sck: esp_hal::peripherals::GPIO18<'static>,
    mosi: esp_hal::peripherals::GPIO23<'static>,
    miso: esp_hal::peripherals::GPIO19<'static>,
    cs: esp_hal::peripherals::GPIO5<'static>,
    gdo0: esp_hal::peripherals::GPIO4<'static>,
    gdo2: esp_hal::peripherals::GPIO15<'static>,
) -> Cc1101Radio {
    Cc1101Radio::new(spi2, sck, mosi, miso, cs, gdo0, gdo2)
}

#[cfg(feature = "pulse_rmt")]
#[allow(clippy::expect_used)]
pub(crate) fn setup_rmt_rx(
    rmt: esp_hal::peripherals::RMT<'static>,
    radio: &mut Cc1101Radio,
) -> RmtChannel<'static, Async, Rx> {
    let data_pin = radio
        .take_data_pin()
        .expect("Failed to take radio data pin");
    let rmt = Rmt::new(rmt, Rate::from_mhz(80))
        .expect("Failed to initialize RMT")
        .into_async();
    let rmt_rx_cfg = RxChannelConfig::default()
        .with_clk_divider(80)
        .with_filter_threshold(50)
        .with_idle_threshold(5000)
        .with_memsize(8);

    rmt.channel0
        .configure_rx(&rmt_rx_cfg)
        .expect("Failed to configure RMT RX channel")
        .with_pin(data_pin)
}

#[allow(clippy::expect_used)]
pub(crate) fn setup_display(
    i2c: esp_hal::peripherals::I2C0<'static>,
    sda: esp_hal::peripherals::GPIO21<'static>,
    scl: esp_hal::peripherals::GPIO22<'static>,
) -> Sh1106Display<I2c<'static, Blocking>> {
    const DISPLAY_I2C_FREQ_KHZ: u32 = 400;
    info!("Setting up I2C display...");

    info!(
        "Display: creating I2C bus ({} kHz) on SDA=GPIO21 SCL=GPIO22",
        DISPLAY_I2C_FREQ_KHZ
    );
    let i2c = I2c::new(
        i2c,
        I2cConfig::default().with_frequency(Rate::from_khz(DISPLAY_I2C_FREQ_KHZ)),
    )
    .expect("Failed to create I2C bus")
    .with_sda(sda)
    .with_scl(scl);

    info!("Display: probing SH1106 over I2C (addr 0x3C)...");
    let mut display = Sh1106Display::new(i2c).expect("Failed to init display");
    info!("Display: SH1106 init OK, drawing startup status...");
    let _ = display.show_status("Starting...");
    info!("Display initialized!");
    display
}

pub(crate) fn setup_ui_input(
    rotary_a: esp_hal::peripherals::GPIO14<'static>,
    rotary_b: esp_hal::peripherals::GPIO13<'static>,
    rotary_sw: esp_hal::peripherals::GPIO27<'static>,
) -> EC11RotaryEncoderInput {
    info!("Setting up Rotary Encoder...");
    let rotary_a = Input::new(rotary_a, InputConfig::default().with_pull(Pull::Up));
    let rotary_b = Input::new(rotary_b, InputConfig::default().with_pull(Pull::Up));
    let rotary_sw = Input::new(rotary_sw, InputConfig::default().with_pull(Pull::Up));
    EC11RotaryEncoderInput::new(rotary_a, rotary_b, rotary_sw)
}
