#![no_std]
#![no_main]

use bt_hci::controller::ExternalController;
use cc1101::{Cc1101, RadioMode};
use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{DriveMode, Input, Output};
use esp_hal::ledc::{
    Ledc, LowSpeed, LSGlobalClkSource,
    channel::{self, Channel, ChannelIFace},
    timer::{self, TimerIFace},
};
use esp_hal::peripherals::Peripherals;
use esp_hal::spi::master::Spi;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_println as _;
use esp_radio::ble::Config as BleConfig;
use esp_radio::ble::controller::BleConnector;
use esp_radio::init;
use static_cell::StaticCell;
use trouble_host::prelude::*;

use esp32_rust_project::radio_433;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    error!("PANIC: {}", defmt::Debug2Format(info));
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

fn system_setup() -> Result<Peripherals, ()> {
    info!("Initializing system...");
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    info!("Initializing heap...");
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    info!("System setup complete!");

    Ok(peripherals)
}

fn ble_setup(
    bt: esp_hal::peripherals::BT<'static>,
) -> Result<Host<'static, MyController, DefaultPacketPool>, ()> {
    // Initialize radio controller
    info!("Initializing Radio...");
    static RADIO_CONTROLLER: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    let radio_controller = RADIO_CONTROLLER.init(init().unwrap());

    info!("Initializing BLE controller...");
    let connector = BleConnector::new(radio_controller, bt, BleConfig::default()).unwrap();
    let controller = ExternalController::new(connector);

    info!("Initializing BLE host...");
    static RESOURCES: StaticCell<HostResources<DefaultPacketPool, CONNS, CHANNELS, ADV_SETS>> =
        StaticCell::new();
    const CONNS: usize = 0;
    const CHANNELS: usize = 1;
    const ADV_SETS: usize = 1;
    let resources = RESOURCES.init(HostResources::new());
    // Create host stack
    static STACK: StaticCell<Stack<'static, MyController, DefaultPacketPool>> = StaticCell::new();
    let stack = trouble_host::new(controller, resources)
        .set_random_address(Address::random([0x01, 0x00, 0xBE, 0xBA, 0xFE, 0xCA]));
    let stack = STACK.init(stack);
    let host = stack.build();
    info!("BLE Setup complete!");

    Ok(host)
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let peripherals = system_setup().unwrap();

    // Initialize async runtime
    esp_rtos::start(TimerGroup::new(peripherals.TIMG0).timer0);

    // Setup hardware PWM for LED dimming
    let mut ledc = Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    static LEDC_TIMER: StaticCell<timer::Timer<'static, LowSpeed>> = StaticCell::new();
    let lstimer0 = LEDC_TIMER.init(ledc.timer::<LowSpeed>(timer::Number::Timer0));
    lstimer0
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty8Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_khz(1),
        })
        .unwrap();

    let mut channel0 = ledc.channel(channel::Number::Channel0, peripherals.GPIO2);
    channel0
        .configure(channel::config::Config {
            timer: lstimer0,
            duty_pct: 5, // 5% duty = dim LED
            drive_mode: DriveMode::PushPull,
        })
        .unwrap();

    // Spawn LED task with hardware PWM channel
    spawner.spawn(led_pwm_task(channel0)).unwrap();

    // Setup BLE - DISABLED for 433MHz debugging
    let host = ble_setup(peripherals.BT).unwrap();

    // Setup 433MHz radio (this may block if CC1101 not connected!)
    info!("Setting up CC1101 radio...");
    let (cc1101, gdo0, _gdo2) = radio_433::setup_cc1101(
        peripherals.SPI2,
        peripherals.GPIO18,
        peripherals.GPIO23,
        peripherals.GPIO19,
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO15,
    );
    info!("CC1101 setup complete!");

    // Spawn tasks
    // BLE tasks disabled for debugging
    spawner.spawn(ble_runner_task(host.runner)).unwrap();
    spawner.spawn(ble_peripheral_task(host.peripheral)).unwrap();
    spawner.spawn(radio_433_rx_task(cc1101, gdo0)).unwrap();

    loop {
        // Main task should be stuck in pending forever
        core::future::pending::<()>().await;
    }
}

type MyController = ExternalController<BleConnector<'static>, 10>;

#[embassy_executor::task]
async fn led_pwm_task(channel: Channel<'static, LowSpeed>) {
    // Breathing LED: fade up and down
    let brightness = 15;
    let duration_ms = 2000;
    loop {
        if channel.start_duty_fade(0, brightness, duration_ms).is_ok() {
            wait_fade_done(&channel, duration_ms).await;
        }

        if channel.start_duty_fade(brightness, 0, duration_ms).is_ok() {
            wait_fade_done(&channel, duration_ms).await;
        }
    }
}

/// Poll until fade completes, yielding to other tasks
async fn wait_fade_done(channel: &Channel<'static, LowSpeed>, duration_ms: u16) {
    while channel.is_duty_fade_running() {
        Timer::after(Duration::from_millis(duration_ms as u64)).await;
    }
}

#[embassy_executor::task]
async fn ble_runner_task(mut runner: Runner<'static, MyController, DefaultPacketPool>) {
    loop {
        if let Err(e) = runner.run().await {
            error!("BLE Runner Error: {:?}", e);
        }
    }
}

#[embassy_executor::task]
async fn ble_peripheral_task(mut peripheral: Peripheral<'static, MyController, DefaultPacketPool>) {
    let mut adv_data = [0u8; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(0x06),
            AdStructure::CompleteLocalName(b"ESP32-Rust-BLE"),
            AdStructure::ManufacturerSpecificData {
                company_identifier: 0xFFFF,
                payload: &[0x02, 0x15, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05],
            },
        ],
        &mut adv_data,
    )
    .unwrap();

    loop {
        info!("Starting BLE Advertising...");
        let advertiser = peripheral
            .advertise(
                &AdvertisementParameters::default(),
                Advertisement::ConnectableScannableUndirected {
                    adv_data: &adv_data[..len],
                    scan_data: &[],
                },
            )
            .await
            .unwrap();

        match advertiser.accept().await {
            Ok(_connection) => {
                info!("BLE Connected!");
            }
            Err(e) => {
                info!("BLE Advertising Error: {:?}", e);
            }
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// Type alias for the CC1101 radio driver
type Cc1101Radio = Cc1101<ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, NoDelay>>;

#[embassy_executor::task]
async fn radio_433_rx_task(mut cc1101: Cc1101Radio, gdo0: Input<'static>) {
    info!("CC1101 RX task started");

    // Check hardware info to verify SPI communication
    match cc1101.get_hw_info() {
        Ok((part, version)) => {
            info!(
                "CC1101 detected: Part=0x{:02X}, Version=0x{:02X}",
                part, version
            );
        }
        Err(e) => {
            error!("CC1101 not responding: {:?}", defmt::Debug2Format(&e));
            return; // Exit task if chip not detected
        }
    }

    // Put radio in receive mode
    cc1101.set_radio_mode(RadioMode::Receive).unwrap();

    let gdo0_initial = gdo0.is_high();
    info!(
        "CC1101 in receive mode, GDO0 initial state: {}",
        gdo0_initial
    );

    // If GDO0 is HIGH at startup, it *might* be clock output, but we configured it
    // to SERIAL_DATA_OUT. If there's noise, it might be high.
    if gdo0_initial {
        info!("Note: GDO0 is high at startup. This is normal if there is RF noise.");
    }

    // Measure RSSI at startup
    info!("Measuring RSSI on 433.92 MHz for 3s...");
    let mut min_rssi: i16 = 0;
    let mut max_rssi: i16 = -128;
    for _ in 0..60 {
        if let Ok(rssi) = cc1101.get_rssi_dbm() {
            if rssi < min_rssi {
                min_rssi = rssi;
            }
            if rssi > max_rssi {
                max_rssi = rssi;
            }
        }
        Timer::after(Duration::from_millis(50)).await;
    }
    info!("RSSI: min={} max={} dBm", min_rssi, max_rssi);

    info!("Starting pulse capture...");

    // Start pulse capture and decoding
    // This is an infinite loop that captures edges and decodes packets
    use esp32_rust_project::pulse_capture::PulseCapture;
    let mut capture = PulseCapture::new(gdo0);
    capture.run().await;
}
