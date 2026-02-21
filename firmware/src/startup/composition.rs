use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
#[cfg(feature = "pulse_rmt")]
use embassy_sync::mutex::Mutex;
#[cfg(feature = "pulse_rmt")]
use esp_hal::Async;
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use esp_hal::ledc::{LowSpeed, channel::Channel as LedcChannel};
#[cfg(feature = "pulse_rmt")]
use esp_hal::rmt::{Channel as RmtChannel, Rx};
use esp_hal::timer::timg::TimerGroup;
use esp_radio::init;
use static_cell::StaticCell;

use crate::app_bus;
use crate::display_driver::Sh1106Display;
use crate::network;
use crate::power;
use crate::radio_433::Cc1101Radio;
use crate::startup::hardware;
use crate::tasks::network_supervisor as network_supervisor_task;
use crate::ui_input::EC11RotaryEncoderInput;
use crate::with_retry;

pub(crate) struct StartupContext {
    pub(crate) led_channel: Option<LedcChannel<'static, LowSpeed>>,
    pub(crate) network_stack: embassy_net::Stack<'static>,
    pub(crate) display: Sh1106Display<I2c<'static, Blocking>>,
    pub(crate) ui_input: EC11RotaryEncoderInput,
    #[cfg(feature = "pulse_sw")]
    pub(crate) radio: Cc1101Radio,
    #[cfg(feature = "pulse_rmt")]
    pub(crate) shared_radio: &'static Mutex<CriticalSectionRawMutex, Cc1101Radio>,
    #[cfg(feature = "pulse_rmt")]
    pub(crate) rmt_rx: RmtChannel<'static, Async, Rx>,
}

#[allow(clippy::expect_used)]
pub(crate) async fn compose(spawner: &Spawner) -> StartupContext {
    static RADIO_CONTROLLER: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    #[cfg(feature = "pulse_rmt")]
    static SHARED_RADIO: StaticCell<Mutex<CriticalSectionRawMutex, Cc1101Radio>> =
        StaticCell::new();

    let peripherals = hardware::system_setup();

    power::log_wakeup_cause();
    let initial_power_settings = power::load_settings_or_default();
    power::restore_settings_after_reset(initial_power_settings);
    app_bus::POWER_SETTINGS_WATCH
        .sender()
        .send(initial_power_settings);

    // Initialize async runtime
    esp_rtos::start(TimerGroup::new(peripherals.TIMG0).timer0);

    let led_channel = hardware::setup_led_channel(peripherals.LEDC, peripherals.GPIO2);

    // Initialize radio controller + network stack
    info!("Initializing Radio controller...");
    let radio_controller = with_retry("Radio controller", || {
        init().map(|controller| RADIO_CONTROLLER.init(controller))
    })
    .await;

    let (network_stack, wifi_controller) =
        network::setup_network_stack(radio_controller, peripherals.WIFI, spawner);
    spawner
        .spawn(network_supervisor_task::network_supervisor_task(
            wifi_controller,
            network_stack,
        ))
        .expect("Failed to spawn network supervisor task");

    info!("Setting up CC1101 radio...");
    let mut radio = hardware::setup_radio_433(
        peripherals.SPI2,
        peripherals.GPIO18,
        peripherals.GPIO23,
        peripherals.GPIO19,
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO15,
    );

    with_retry("CC1101 radio", || radio.init()).await;
    info!("CC1101 setup complete!");

    #[cfg(feature = "pulse_rmt")]
    let rmt_rx = hardware::setup_rmt_rx(peripherals.RMT, &mut radio);

    let display = hardware::setup_display(peripherals.I2C0, peripherals.GPIO21, peripherals.GPIO22);
    let ui_input =
        hardware::setup_ui_input(peripherals.GPIO14, peripherals.GPIO13, peripherals.GPIO27);

    if led_channel.is_none() {
        error!("LED hardware setup failed, skipping LED task.");
    }

    StartupContext {
        led_channel,
        network_stack,
        display,
        ui_input,
        #[cfg(feature = "pulse_sw")]
        radio,
        #[cfg(feature = "pulse_rmt")]
        shared_radio: SHARED_RADIO.init(Mutex::new(radio)),
        #[cfg(feature = "pulse_rmt")]
        rmt_rx,
    }
}
