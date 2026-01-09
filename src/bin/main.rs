#![no_std]
#![no_main]

use bt_hci::controller::ExternalController;
use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::Peripherals;
use esp_hal::timer::timg::TimerGroup;
use esp_println as _;
use esp_radio::ble::Config as BleConfig;
use esp_radio::ble::controller::BleConnector;
use esp_radio::init;
use static_cell::StaticCell;
use trouble_host::prelude::*;

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

    // Setup BLE
    let host = ble_setup(peripherals.BT).unwrap();

    // Spawn tasks
    spawner.spawn(ble_runner_task(host.runner)).unwrap();
    spawner.spawn(ble_peripheral_task(host.peripheral)).unwrap();
    spawner
        .spawn(led_task(Output::new(
            peripherals.GPIO2,
            Level::Low,
            OutputConfig::default(),
        )))
        .unwrap();

    loop {
        // Main task should be stuck in pending forever
        core::future::pending::<()>().await;
    }
}

type MyController = ExternalController<BleConnector<'static>, 10>;

#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) {
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(1000)).await;
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
