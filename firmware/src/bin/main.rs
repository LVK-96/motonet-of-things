#![no_std]
#![no_main]

extern crate alloc;

use defmt::{error, info};

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
#[cfg(feature = "pulse_rmt")]
use embassy_sync::mutex::Mutex;

use esp_hal::clock::CpuClock;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::ledc::{
    LSGlobalClkSource, Ledc, LowSpeed,
    channel::{self, ChannelIFace},
    timer::{self, TimerIFace},
};
use esp_hal::peripherals::Peripherals;
#[cfg(feature = "pulse_rmt")]
use esp_hal::rmt::{Rmt, RxChannelConfig, RxChannelCreator};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;

use esp_println as _;
use esp_radio::init;
use static_cell::StaticCell;

use esp32_rust_project::app_bus;
use esp32_rust_project::display::{Display, Sh1106Display};
use esp32_rust_project::network;
use esp32_rust_project::power;
use esp32_rust_project::radio_433::Cc1101Radio;
#[cfg(feature = "pulse_rmt")]
use esp32_rust_project::radio_433::Radio433;
use esp32_rust_project::tasks::{
    display as display_task, led_pwm as led_pwm_task, mqtt as mqtt_task,
    network_supervisor as network_supervisor_task, radio_433 as radio_433_task,
    time_sync as time_sync_task,
};
use esp32_rust_project::ui_input::EC11RotaryEncoderInput;
use esp32_rust_project::with_retry;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    error!("PANIC: {}", defmt::Debug2Format(info));
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

fn system_setup() -> Peripherals {
    info!("Initializing system...");
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    info!("Initializing heap...");
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    info!("System setup complete!");

    peripherals
}

#[esp_rtos::main]
#[allow(clippy::expect_used)]
async fn main(spawner: Spawner) -> ! {
    // Statics allocated in main to ensure lifetime
    static LEDC_TIMER: StaticCell<timer::Timer<'static, LowSpeed>> = StaticCell::new();
    static RADIO_CONTROLLER: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    #[cfg(feature = "pulse_rmt")]
    static SHARED_RADIO: StaticCell<Mutex<CriticalSectionRawMutex, Cc1101Radio>> =
        StaticCell::new();

    let peripherals = system_setup();
    power::log_wakeup_cause();
    let initial_power_settings = power::load_settings_or_default();
    power::restore_settings_after_reset(initial_power_settings);
    app_bus::POWER_SETTINGS_WATCH
        .sender()
        .send(initial_power_settings);

    // Initialize async runtime
    esp_rtos::start(TimerGroup::new(peripherals.TIMG0).timer0);

    // Setup hardware PWM for LED dimming
    let mut ledc = Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    let lstimer0 = LEDC_TIMER.init(ledc.timer::<LowSpeed>(timer::Number::Timer0));
    let mut channel0 = ledc.channel(channel::Number::Channel0, peripherals.GPIO2);

    let timer_ok = lstimer0
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty8Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_khz(1),
        })
        .is_ok();

    let channel_ok = if timer_ok {
        channel0
            .configure(channel::config::Config {
                timer: lstimer0,
                duty_pct: 5,
                drive_mode: DriveMode::PushPull,
            })
            .is_ok()
    } else {
        false
    };

    if channel_ok {
        info!("LED hardware configured! Spawning task...");
        if let Err(e) = spawner.spawn(led_pwm_task::led_pwm_task(channel0)) {
            error!("Failed to spawn LED task: {}", defmt::Debug2Format(&e));
        }
    } else {
        error!("LED hardware setup failed, skipping LED task.");
    }

    // Initialize Radio controller
    info!("Initializing Radio controller...");
    let radio_controller = with_retry("Radio controller", || {
        init().map(|controller| RADIO_CONTROLLER.init(controller))
    })
    .await;

    // Setup network stack without blocking startup on Wi-Fi association.
    let (network_stack, wifi_controller) =
        network::setup_network_stack(radio_controller, peripherals.WIFI, &spawner);
    spawner
        .spawn(network_supervisor_task::network_supervisor_task(
            wifi_controller,
            network_stack,
        ))
        .expect("Failed to spawn network supervisor task");

    // Setup 433MHz radio
    info!("Setting up CC1101 radio...");
    let mut r433 = Cc1101Radio::new(
        peripherals.SPI2,
        peripherals.GPIO18,
        peripherals.GPIO23,
        peripherals.GPIO19,
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO15,
    );

    with_retry("CC1101 radio", || r433.init()).await;
    info!("CC1101 setup complete!");

    #[cfg(feature = "pulse_rmt")]
    let rmt_rx = {
        let data_pin = r433.take_data_pin().expect("Failed to take radio data pin");
        let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80))
            .expect("Failed to initialize RMT")
            .into_async();
        let rmt_rx_cfg = RxChannelConfig::default()
            .with_clk_divider(80)
            .with_filter_threshold(50)
            .with_idle_threshold(5000)
            .with_memsize(8);
        rmt.channel0
            .configure_rx(data_pin, rmt_rx_cfg)
            .expect("Failed to configure RMT RX channel")
    };

    // Setup I2C for display (GPIO21 = SDA, GPIO22 = SCL)
    info!("Setting up I2C display...");
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("Failed to create I2C bus")
    .with_sda(peripherals.GPIO21)
    .with_scl(peripherals.GPIO22);

    let mut display = Sh1106Display::new(i2c).expect("Failed to init display");
    let _ = display.show_status("Starting...");
    info!("Display initialized!");

    // Setup Rotary Encoder (GPIO 14, 13) and push button (GPIO 27)
    info!("Setting up Rotary Encoder...");
    let rotary_a = Input::new(
        peripherals.GPIO14,
        InputConfig::default().with_pull(Pull::Up),
    );
    let rotary_b = Input::new(
        peripherals.GPIO13,
        InputConfig::default().with_pull(Pull::Up),
    );
    let rotary_sw = Input::new(
        peripherals.GPIO27,
        InputConfig::default().with_pull(Pull::Up),
    );
    let ui_input = EC11RotaryEncoderInput::new(rotary_a, rotary_b, rotary_sw);

    // Spawn tasks
    #[cfg(feature = "pulse_sw")]
    {
        let settings_receiver = app_bus::RADIO_SETTINGS_WATCH
            .receiver()
            .expect("Failed to get settings receiver");
        spawner
            .spawn(radio_433_task::radio_433_rx_task(
                r433,
                app_bus::READING_WATCH.sender(),
                app_bus::RADIO_TELEMETRY_CHANNEL.sender(),
                app_bus::RADIO_SETTINGS_WATCH.sender(),
                settings_receiver,
            ))
            .expect("Failed to spawn radio task");
    }
    #[cfg(feature = "pulse_rmt")]
    {
        let shared_radio = SHARED_RADIO.init(Mutex::new(r433));
        let settings_receiver = app_bus::RADIO_SETTINGS_WATCH
            .receiver()
            .expect("Failed to get settings receiver");
        spawner
            .spawn(radio_433_task::radio_433_settings_task(
                shared_radio,
                settings_receiver,
            ))
            .expect("Failed to spawn radio settings task");
        spawner
            .spawn(radio_433_task::radio_433_rx_task(
                shared_radio,
                rmt_rx,
                app_bus::READING_WATCH.sender(),
                app_bus::RADIO_TELEMETRY_CHANNEL.sender(),
                app_bus::RADIO_SETTINGS_WATCH.sender(),
            ))
            .expect("Failed to spawn radio task");
    }
    spawner
        .spawn(radio_433_task::radio_433_event_router_task(
            app_bus::RADIO_TELEMETRY_CHANNEL.receiver(),
            app_bus::MQTT_COMMAND_CHANNEL.sender(),
        ))
        .expect("Failed to spawn radio event router task");
    spawner
        .spawn(app_bus::app_command_dispatch_task())
        .expect("Failed to spawn app command dispatch task");
    spawner
        .spawn(mqtt_task::mqtt_task(
            network_stack,
            app_bus::MQTT_COMMAND_CHANNEL.receiver(),
        ))
        .expect("Failed to spawn mqtt task");
    spawner
        .spawn(display_task::ui_input_task(
            ui_input,
            app_bus::APP_EVENT_CHANNEL.sender(),
        ))
        .expect("Failed to spawn ui input task");
    spawner
        .spawn(display_task::display_task(
            display,
            app_bus::READING_WATCH
                .receiver()
                .expect("Failed to get reading receiver"),
            app_bus::APP_EVENT_CHANNEL.receiver(),
            app_bus::RADIO_SETTINGS_WATCH
                .receiver()
                .expect("Failed to get settings receiver for display"),
            app_bus::POWER_SETTINGS_WATCH
                .receiver()
                .expect("Failed to get power settings receiver for display"),
            app_bus::APP_COMMAND_CHANNEL.sender(),
        ))
        .expect("Failed to spawn display task");
    spawner
        .spawn(time_sync_task::time_sync_task(network_stack))
        .expect("Failed to spawn time sync task");

    loop {
        core::future::pending::<()>().await;
    }
}
