#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write;
use defmt::{error, info, warn};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::Ipv4Address;
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Timer};
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
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_println as _;
use esp_radio::init;
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::options::{ConnectOptions, PublicationOptions, WillOptions};
use rust_mqtt::config::{KeepAlive, SessionExpiryInterval};
use rust_mqtt::types::{MqttBinary, MqttString, QoS, TopicName};
use static_cell::StaticCell;

use esp32_rust_project::display::{Display, Sh1106Display};
use esp32_rust_project::messages::{RadioReading, RadioSettings};
use esp32_rust_project::network;
use esp32_rust_project::pulse_capture::PulseCapture;
use esp32_rust_project::radio_433::{Cc1101Radio, Radio433};
use esp32_rust_project::secrets::{MQTT_BROKER_IP, MQTT_BROKER_PORT, MQTT_CLIENT_ID};
use esp32_rust_project::time_sync::{self, TIME_WATCH};
use esp32_rust_project::ui_input::{EC11RotaryEncoderInput, UiEvent, UiInput};
use esp32_rust_project::with_retry;

// Display state for the UI task
#[derive(Clone, Copy)]
enum DisplayState {
    Main,
    Radio,
    Settings { nav_index: u8, editing: bool },
}

/// Watch for sharing readings with multiple consumers (MQTT + display)
static READING_WATCH: Watch<CriticalSectionRawMutex, RadioReading, 2> = Watch::new();

/// Watch for sharing radio settings with the radio task
static RADIO_SETTINGS_WATCH: Watch<CriticalSectionRawMutex, RadioSettings, 2> = Watch::new();

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

    let peripherals = system_setup();

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
        if let Err(e) = spawner.spawn(led_pwm_task(channel0)) {
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

    // Setup WiFi and network stack
    let network_stack = network::setup_wifi(radio_controller, peripherals.WIFI, &spawner).await;

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
    spawner
        .spawn(radio_433_rx_task(r433))
        .expect("Failed to spawn radio task");
    spawner
        .spawn(mqtt_task(
            network_stack,
            READING_WATCH
                .receiver()
                .expect("Failed to get reading receiver"),
        ))
        .expect("Failed to spawn mqtt task");
    spawner
        .spawn(display_task(
            display,
            READING_WATCH
                .receiver()
                .expect("Failed to get reading receiver"),
            ui_input,
        ))
        .expect("Failed to spawn display task");
    spawner
        .spawn(time_sync_task(network_stack))
        .expect("Failed to spawn time sync task");

    loop {
        core::future::pending::<()>().await;
    }
}

#[embassy_executor::task]
async fn led_pwm_task(channel: LedcChannel<'static, LowSpeed>) {
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

#[allow(clippy::trivially_copy_pass_by_ref)]
async fn wait_fade_done(channel: &LedcChannel<'static, LowSpeed>, duration_ms: u16) {
    while channel.is_duty_fade_running() {
        Timer::after(Duration::from_millis(u64::from(duration_ms))).await;
    }
}

/// Sets up the MQTT client, including TCP connection and MQTT broker handshake.
async fn establish_mqtt_session<'a>(
    network_stack: embassy_net::Stack<'static>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    client: &mut Client<'a, TcpSocket<'a>, BumpBuffer<'a>, 4, 2, 2>,
) -> Result<(), ()> {
    // Wait for network to be up
    network_stack.wait_config_up().await;

    info!("MQTT: Connecting to broker...");

    let mut socket = TcpSocket::new(network_stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(120)));
    socket.set_nagle_enabled(false);

    let broker_addr = (
        Ipv4Address::new(
            MQTT_BROKER_IP[0],
            MQTT_BROKER_IP[1],
            MQTT_BROKER_IP[2],
            MQTT_BROKER_IP[3],
        ),
        MQTT_BROKER_PORT,
    );

    if let Err(e) = socket.connect(broker_addr).await {
        warn!("MQTT: TCP connect failed: {:?}", e);
        return Err(());
    }

    info!("MQTT: TCP connected");

    let lwt = WillOptions {
        will_qos: QoS::AtMostOnce,
        will_retain: true,
        will_topic: MqttString::try_from("sensors/rubicson/status").map_err(|_| ())?,
        will_payload: MqttBinary::try_from(b"offline" as &[u8]).map_err(|_| ())?,
        will_delay_interval: 0,
        is_payload_utf8: true,
        message_expiry_interval: None,
        content_type: None,
        response_topic: None,
        correlation_data: None,
    };

    let connect_options = ConnectOptions {
        clean_start: true,
        keep_alive: KeepAlive::Seconds(120),
        session_expiry_interval: SessionExpiryInterval::EndOnDisconnect,
        user_name: None,
        password: None,
        will: Some(lwt),
    };
    let client_id = MqttString::try_from(MQTT_CLIENT_ID).ok();

    if let Err(e) = client.connect(socket, &connect_options, client_id).await {
        warn!("MQTT: Broker connect failed: {:?}", defmt::Debug2Format(&e));
        return Err(());
    }

    info!("MQTT: Connected to broker!");
    unsafe { client.buffer().reset() };

    // Publish "online" status message
    let status_topic = MqttString::try_from("sensors/rubicson/status").map_err(|_| ())?;
    let status_topic_name = unsafe { TopicName::new_unchecked(status_topic) };
    let online_options = PublicationOptions {
        retain: true,
        topic: status_topic_name,
        qos: QoS::AtMostOnce,
    };
    if let Err(e) = client
        .publish(&online_options, Bytes::from(b"online" as &[u8]))
        .await
    {
        warn!(
            "MQTT: Failed to publish online status: {:?}",
            defmt::Debug2Format(&e)
        );
        return Err(());
    }

    info!("MQTT: Published online status");
    Ok(())
}

#[embassy_executor::task]
#[allow(clippy::expect_used, clippy::too_many_lines)]
async fn mqtt_task(
    network_stack: embassy_net::Stack<'static>,
    mut receiver: embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, RadioReading, 2>,
) {
    // Backoff parameters
    const MIN_BACKOFF_SECS: u64 = 1;
    const MAX_BACKOFF_SECS: u64 = 60;
    const PING_INTERVAL_SECS: u64 = 90; // Ping every 90s (keepalive is 120s)
    let mut backoff_secs = MIN_BACKOFF_SECS;

    loop {
        // Create buffers on the stack for this session
        let mut rx_buf = [0u8; 1024];
        let mut tx_buf = [0u8; 1024];
        let mut mqtt_buf = [0u8; 1024];
        let mut mqtt_buffer = BumpBuffer::new(&mut mqtt_buf);
        let mut client = Client::new(&mut mqtt_buffer);

        if establish_mqtt_session(network_stack, &mut rx_buf, &mut tx_buf, &mut client)
            .await
            .is_err()
        {
            Timer::after(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            continue;
        }

        backoff_secs = MIN_BACKOFF_SECS;
        info!("MQTT: Ready");
        let mut last_activity = Instant::now();

        loop {
            // Wait for either a reading or ping timeout
            let timeout = Duration::from_secs(PING_INTERVAL_SECS);

            match select(receiver.changed(), Timer::after(timeout)).await {
                Either::First(reading) => {
                    // Got a reading - publish it
                    let time_since_last = last_activity.elapsed().as_secs();
                    info!("MQTT: Got reading after {}s idle", time_since_last);

                    // Send a ping first if we've been idle for a while
                    if time_since_last > 10 {
                        info!(
                            "MQTT: Sending ping before publish (idle {}s)",
                            time_since_last
                        );
                        unsafe { client.buffer().reset() };
                        if let Err(e) = client.ping().await {
                            warn!(
                                "MQTT: Pre-publish ping failed: {:?}, reconnecting...",
                                defmt::Debug2Format(&e)
                            );
                            break;
                        }
                        // Small delay after ping
                        Timer::after(Duration::from_millis(100)).await;
                    }

                    last_activity = Instant::now();

                    // Reset buffer before publish
                    unsafe { client.buffer().reset() };

                    // Format topic: sensors/rubicson/{id}/temperature
                    let mut topic: heapless::String<64> = heapless::String::new();
                    if write!(topic, "sensors/rubicson/{}/temperature", reading.inner.id).is_err() {
                        continue;
                    }

                    // Format payload: id={id},ch={channel},temp={temp},batt={ok/low},rssi={rssi},snr={snr}
                    let mut payload: heapless::String<96> = heapless::String::new();
                    let batt = if reading.inner.battery_ok {
                        "ok"
                    } else {
                        "low"
                    };
                    if write!(
                        payload,
                        "id={},ch={},temp={:.1},batt={},rssi={},snr={}",
                        reading.inner.id,
                        reading.inner.channel,
                        reading.inner.temperature_c,
                        batt,
                        reading.rssi,
                        reading.detection_threshold
                    )
                    .is_err()
                    {
                        continue;
                    }

                    info!(
                        "MQTT: Publishing to {} : {}",
                        topic.as_str(),
                        payload.as_str()
                    );

                    // Create TopicName from the topic string
                    let topic_name = match MqttString::from_slice(topic.as_str()) {
                        Ok(s) => unsafe { TopicName::new_unchecked(s) },
                        Err(_) => continue,
                    };

                    let pub_options = PublicationOptions {
                        retain: false,
                        topic: topic_name,
                        qos: QoS::AtMostOnce,
                    };

                    if let Err(e) = client
                        .publish(&pub_options, Bytes::from(payload.as_bytes()))
                        .await
                    {
                        warn!(
                            "MQTT: Publish failed: {:?}, reconnecting...",
                            defmt::Debug2Format(&e)
                        );
                        break; // Break inner loop to reconnect
                    }

                    info!("MQTT: Publish successful!");
                }
                Either::Second(()) => {
                    // Ping timeout - send keepalive
                    info!("MQTT: Sending periodic ping");
                    unsafe { client.buffer().reset() };

                    if let Err(e) = client.ping().await {
                        warn!(
                            "MQTT: Ping failed: {:?}, reconnecting...",
                            defmt::Debug2Format(&e)
                        );
                        break;
                    }
                    info!("MQTT: Ping sent successfully");
                    last_activity = Instant::now();
                }
            }
        }

        // Backoff before reconnecting
        Timer::after(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

/// Time sync task - syncs time from NTP server
#[embassy_executor::task]
async fn time_sync_task(stack: embassy_net::Stack<'static>) {
    time_sync::time_sync_loop(stack).await;
}

/// Display task - updates OLED when new readings arrive
#[embassy_executor::task]
#[allow(clippy::too_many_lines, clippy::expect_used)]
async fn display_task(
    mut display: Sh1106Display<I2c<'static, Blocking>>,
    mut receiver: embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, RadioReading, 2>,
    mut ui: EC11RotaryEncoderInput,
) {
    info!("Display task started");

    // Get a receiver for time updates
    let mut time_receiver = TIME_WATCH.receiver().expect("Failed to get time receiver");

    // Show waiting message
    if display.show_status("Waiting...").is_err() {
        error!("Display: Failed to show status");
    }

    let mut state = DisplayState::Main;
    let mut last_reading: Option<RadioReading> = None;

    // Settings values (pending values, not yet applied)
    let mut pending_threshold: u8 = 16; // Default from CC1101 config
    let mut pending_magn_target: u8 = 7; // Default (42 dB)

    // Get sender for radio settings
    let settings_sender = RADIO_SETTINGS_WATCH.sender();

    // Send initial settings
    settings_sender.send(RadioSettings {
        detection_threshold_db: pending_threshold,
        magn_target: pending_magn_target,
    });

    loop {
        match select(
            receiver.changed(),
            ui.next_event(UiEvent::NextScreen, UiEvent::PrevScreen),
        )
        .await
        {
            Either::First(reading) => {
                last_reading = Some(reading);
            }
            Either::Second(event) => {
                // Handle UI events based on current state
                state = match state {
                    DisplayState::Main => match event {
                        UiEvent::NextScreen | UiEvent::PrevScreen => DisplayState::Radio,
                        UiEvent::Select => DisplayState::Settings {
                            nav_index: 0,
                            editing: false,
                        },
                    },
                    DisplayState::Radio => match event {
                        UiEvent::NextScreen | UiEvent::PrevScreen => DisplayState::Main,
                        UiEvent::Select => DisplayState::Settings {
                            nav_index: 0,
                            editing: false,
                        },
                    },
                    DisplayState::Settings { nav_index, editing } => match event {
                        UiEvent::Select => {
                            if editing {
                                // Exit editing mode, go back to navigation
                                DisplayState::Settings {
                                    nav_index,
                                    editing: false,
                                }
                            } else if nav_index == 2 {
                                // Save selected - apply settings and exit
                                settings_sender.send(RadioSettings {
                                    detection_threshold_db: pending_threshold,
                                    magn_target: pending_magn_target,
                                });
                                info!(
                                    "Settings saved: threshold={} dB, magn_target={}",
                                    pending_threshold, pending_magn_target
                                );
                                DisplayState::Radio
                            } else {
                                // Enter editing mode for current item
                                DisplayState::Settings {
                                    nav_index,
                                    editing: true,
                                }
                            }
                        }
                        UiEvent::NextScreen => {
                            if editing {
                                // Adjust value up
                                match nav_index {
                                    0 => {
                                        // Detection threshold: 4, 8, 12, 16 dB
                                        pending_threshold = if pending_threshold >= 16 {
                                            4
                                        } else {
                                            pending_threshold + 4
                                        };
                                    }
                                    1 => {
                                        // Magn target: 0-7
                                        pending_magn_target = if pending_magn_target >= 7 {
                                            0
                                        } else {
                                            pending_magn_target + 1
                                        };
                                    }
                                    _ => {}
                                }
                                DisplayState::Settings { nav_index, editing }
                            } else {
                                // Navigate to next item (wrap around)
                                let next = if nav_index >= 2 { 0 } else { nav_index + 1 };
                                DisplayState::Settings {
                                    nav_index: next,
                                    editing: false,
                                }
                            }
                        }
                        UiEvent::PrevScreen => {
                            if editing {
                                // Adjust value down
                                match nav_index {
                                    0 => {
                                        pending_threshold = if pending_threshold <= 4 {
                                            16
                                        } else {
                                            pending_threshold - 4
                                        };
                                    }
                                    1 => {
                                        pending_magn_target = if pending_magn_target == 0 {
                                            7
                                        } else {
                                            pending_magn_target - 1
                                        };
                                    }
                                    _ => {}
                                }
                                DisplayState::Settings { nav_index, editing }
                            } else {
                                // Navigate to previous item (wrap around)
                                let prev = if nav_index == 0 { 2 } else { nav_index - 1 };
                                DisplayState::Settings {
                                    nav_index: prev,
                                    editing: false,
                                }
                            }
                        }
                    },
                };
            }
        }

        // Always update display after any event
        match state {
            DisplayState::Main => {
                if let Some(reading) = &last_reading {
                    // Get current timestamp if available
                    let mut time_str: heapless::String<16> = heapless::String::new();
                    let timestamp = if let Some(Some(time_ref)) = time_receiver.try_get() {
                        time_ref.format_time(&mut time_str);
                        Some(time_str.as_str())
                    } else {
                        None
                    };

                    info!(
                        "Display: Showing temp {}C from sensor {}",
                        reading.inner.temperature_c, reading.inner.id
                    );

                    if let Err(e) = display.show_temperature(
                        reading.inner.temperature_c,
                        reading.inner.id,
                        reading.inner.channel,
                        reading.inner.battery_ok,
                        timestamp,
                    ) {
                        error!("Display: Failed to update: {:?}", e);
                    }
                } else {
                    let _ = display.show_status("Waiting...");
                }
            }
            DisplayState::Radio => {
                let rssi = last_reading.as_ref().map(|r| r.rssi);
                let det_threshold = last_reading
                    .as_ref()
                    .map_or(pending_threshold, |r| r.detection_threshold);
                let _ = display.show_radio_info(rssi, det_threshold);
            }
            DisplayState::Settings { nav_index, editing } => {
                let _ = display.show_settings_menu(
                    nav_index,
                    editing,
                    pending_threshold,
                    pending_magn_target,
                );
            }
        }
    }
}

#[embassy_executor::task]
#[allow(clippy::expect_used)]
async fn radio_433_rx_task(mut radio: Cc1101Radio) {
    info!("Radio 433 RX task started");

    match radio.get_hw_info().await {
        Ok((part, version)) => {
            info!(
                "Radio detected: Part=0x{:02X}, Version=0x{:02X}",
                part, version
            );
        }
        Err(e) => {
            error!("Radio not responding: {:?}", e);
            return;
        }
    }

    if let Err(e) = radio.set_receive_mode().await {
        error!("Failed to set receive mode: {:?}", e);
        return;
    }

    // Take ownership of the data pin for pulse capture
    let Some(data_pin) = radio.take_data_pin() else {
        error!("Data pin already taken");
        return;
    };

    let data_pin_initial = data_pin.is_high();
    info!(
        "Radio in receive mode, data pin initial state: {}",
        data_pin_initial
    );

    if data_pin_initial {
        info!("Note: Data pin is high at startup. This is normal if there is RF noise.");
    }

    info!("Measuring RSSI on 433.92 MHz for 3s...");
    let mut min_rssi: i16 = 0;
    let mut max_rssi: i16 = -128;
    for _ in 0..60 {
        if let Ok(rssi) = radio.get_rssi_dbm().await {
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

    let settings_receiver = RADIO_SETTINGS_WATCH
        .receiver()
        .expect("Failed to get settings receiver");
    let mut capture = PulseCapture::new(
        data_pin,
        &mut radio,
        READING_WATCH.sender(),
        settings_receiver,
    );
    capture.run().await;
}
