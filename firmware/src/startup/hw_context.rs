use defmt::{error, info};
use embassy_executor::{SpawnError, SpawnToken, Spawner};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
#[cfg(feature = "pulse_rmt")]
use esp_hal::Async;
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::ledc::{LowSpeed, channel::Channel as LedcChannel};
#[cfg(feature = "pulse_rmt")]
use esp_hal::rmt::{Channel as RmtChannel, Rx};
use esp_hal::timer::timg::TimerGroup;
use esp_storage::FlashStorage;
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

#[allow(clippy::expect_used, clippy::trivially_copy_pass_by_ref)]
fn spawn_task<S>(
    spawner: &Spawner,
    token: Result<SpawnToken<S>, SpawnError>,
    message: &'static str,
) {
    spawner.spawn(token.expect(message));
}

fn log_procpu_stack_guard() {
    unsafe extern "C" {
        static _stack_end_cpu0: u32;
        static _stack_start_cpu0: u32;
        static __stack_chk_guard: u32;
    }

    let stack_end = &raw const _stack_end_cpu0 as usize;
    let stack_start = &raw const _stack_start_cpu0 as usize;
    let guard = &raw const __stack_chk_guard as usize;
    let mut dbreaka0: u32;
    unsafe {
        core::arch::asm!("rsr {0}, 144", out(reg) dbreaka0, options(nostack));
    }
    info!(
        "StackGuard: procpu stack=0x{:x}..0x{:x} ({} bytes), guard=0x{:x}, dbreaka0=0x{:x}",
        stack_end,
        stack_start,
        stack_start.saturating_sub(stack_end),
        guard,
        dbreaka0
    );
}

pub(crate) struct HWContext {
    pub(crate) led_channel: Option<LedcChannel<'static, LowSpeed>>,
    pub(crate) network_stack: embassy_net::Stack<'static>,
    pub(crate) display: Sh1106Display<I2c<'static, Blocking>>,
    pub(crate) ui_input: EC11RotaryEncoderInput,
    pub(crate) flash_mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>,
    #[cfg(feature = "pulse_sw")]
    pub(crate) radio: Cc1101Radio,
    #[cfg(feature = "pulse_rmt")]
    pub(crate) shared_radio: &'static Mutex<CriticalSectionRawMutex, Cc1101Radio>,
    #[cfg(feature = "pulse_rmt")]
    pub(crate) rmt_rx: RmtChannel<'static, Async, Rx>,
}

#[allow(clippy::expect_used)]
pub(crate) async fn hw_setup(spawner: &Spawner) -> HWContext {
    #[cfg(feature = "pulse_rmt")]
    static SHARED_RADIO: StaticCell<Mutex<CriticalSectionRawMutex, Cc1101Radio>> =
        StaticCell::new();

    let peripherals = hardware::system_setup();

    // SHA-256 hardware vs software self-test — runs once at boot.
    crate::startup::sha_self_test::run_sha_self_test(
        unsafe { peripherals.SHA.clone_unchecked() },
        unsafe { peripherals.AES.clone_unchecked() },
    );

    // SAFETY: initialised exactly once, before any task accesses them.
    unsafe {
        crate::ota::init_crypto_peripherals(peripherals.AES, peripherals.SHA);
    }

    // Initialize OTA flash storage singleton before any task uses it.
    let flash_mutex = &*app_bus::FLASH.init(Mutex::new(FlashStorage::new(peripherals.FLASH)));

    power::log_wakeup_cause();
    let initial_power_settings = power::load_settings_or_default();
    power::restore_settings_after_reset(initial_power_settings);
    app_bus::POWER_SETTINGS_WATCH
        .sender()
        .send(initial_power_settings);
    let initial_radio_settings = crate::radio_settings::load_settings_or_default();
    app_bus::RADIO_SETTINGS_WATCH
        .sender()
        .send(initial_radio_settings);

    // Initialize async runtime
    let software_interrupts = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(
        TimerGroup::new(peripherals.TIMG0).timer0,
        software_interrupts.software_interrupt0,
    );
    log_procpu_stack_guard();

    let led_channel = hardware::setup_led_channel(peripherals.LEDC, peripherals.GPIO2);

    // Initialize Wi-Fi + network stack
    info!("Initializing WiFi controller...");
    let (network_stack, wifi_controller) = network::setup_network_stack(peripherals.WIFI, spawner);
    spawn_task(
        spawner,
        network_supervisor_task::network_supervisor_task(wifi_controller, network_stack),
        "Failed to spawn network supervisor task",
    );

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

    radio.restore_settings_after_reset(initial_radio_settings);
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

    HWContext {
        led_channel,
        network_stack,
        display,
        ui_input,
        flash_mutex,
        #[cfg(feature = "pulse_sw")]
        radio,
        #[cfg(feature = "pulse_rmt")]
        shared_radio: SHARED_RADIO.init(Mutex::new(radio)),
        #[cfg(feature = "pulse_rmt")]
        rmt_rx,
    }
}
