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
use esp32_rust_project::messages::RadioReading;
use esp32_rust_project::network;
use esp32_rust_project::radio_433::{Cc1101Radio, Radio433};
use esp32_rust_project::secrets::{MQTT_BROKER_IP, MQTT_BROKER_PORT, MQTT_CLIENT_ID};
use esp32_rust_project::time_sync::{self, TIME_WATCH};
use esp32_rust_project::ui_input::{EC11RotaryEncoderInput, UiEvent, UiInput};

/// Watch for sharing readings with multiple consumers (MQTT + display)
static READING_WATCH: Watch<CriticalSectionRawMutex, RadioReading, 2> = Watch::new();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    error!("PANIC: {}", defmt::Debug2Format(info));
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

fn system_setup() -> Result<Peripherals, ()> {
    info!("Initializing system...");
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    info!("Initializing heap...");
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    info!("System setup complete!");

    Ok(peripherals)
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
            duty_pct: 5,
            drive_mode: DriveMode::PushPull,
        })
        .unwrap();

    // Spawn LED task
    spawner.spawn(led_pwm_task(channel0)).unwrap();

    // Initialize Radio controller
    info!("Initializing Radio controller...");
    static RADIO_CONTROLLER: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    let radio_controller = RADIO_CONTROLLER.init(init().unwrap());

    // Setup WiFi and network stack
    let stack = network::setup_wifi(radio_controller, peripherals.WIFI, &spawner).await;

    // Setup 433MHz radio
    info!("Setting up CC1101 radio...");
    let radio = Cc1101Radio::new(
        peripherals.SPI2,
        peripherals.GPIO18,
        peripherals.GPIO23,
        peripherals.GPIO19,
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO15,
    )
    .expect("Failed to initialize CC1101");
    info!("CC1101 setup complete!");

    // Setup I2C for display (GPIO21 = SDA, GPIO22 = SCL)
    info!("Setting up I2C display...");
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO21)
    .with_scl(peripherals.GPIO22);

    let mut display = Sh1106Display::new(i2c).expect("Failed to init display");
    let _ = display.show_status("Starting...");
    info!("Display initialized!");

    // Setup Rotary Encoder (GPIO 14, 13)
    info!("Setting up Rotary Encoder...");
    let rotary_a = Input::new(
        peripherals.GPIO14,
        InputConfig::default().with_pull(Pull::Up),
    );
    let rotary_b = Input::new(
        peripherals.GPIO13,
        InputConfig::default().with_pull(Pull::Up),
    );
    let ui_input = EC11RotaryEncoderInput::new(rotary_a, rotary_b);

    // Spawn tasks
    spawner.spawn(radio_433_rx_task(radio)).unwrap();
    spawner
        .spawn(mqtt_task(stack, READING_WATCH.receiver().unwrap()))
        .unwrap();
    spawner
        .spawn(display_task(
            display,
            READING_WATCH.receiver().unwrap(),
            ui_input,
        ))
        .unwrap();
    spawner.spawn(time_sync_task(stack)).unwrap();

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

async fn wait_fade_done(channel: &LedcChannel<'static, LowSpeed>, duration_ms: u16) {
    while channel.is_duty_fade_running() {
        Timer::after(Duration::from_millis(duration_ms as u64)).await;
    }
}

#[embassy_executor::task]
async fn mqtt_task(
    stack: &'static embassy_net::Stack<'static>,
    mut receiver: embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, RadioReading, 2>,
) {
    // Backoff parameters
    const MIN_BACKOFF_SECS: u64 = 1;
    const MAX_BACKOFF_SECS: u64 = 60;
    let mut backoff_secs = MIN_BACKOFF_SECS;

    loop {
        // Wait for network to be up
        stack.wait_config_up().await;

        info!("MQTT: Connecting to broker...");

        // Create socket buffers
        let mut rx_buffer = [0u8; 1024];
        let mut tx_buffer = [0u8; 1024];

        let mut socket = TcpSocket::new(*stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(120))); // 2 minute timeout (longer than keepalive)
        socket.set_nagle_enabled(false); // Disable Nagle for immediate sends

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
            warn!(
                "MQTT: TCP connect failed: {:?}, retry in {}s",
                e, backoff_secs
            );
            Timer::after(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            continue;
        }

        info!("MQTT: TCP connected");

        // Create MQTT client with bump buffer (larger for MQTT v5 packets)
        let mut mqtt_buffer = [0u8; 1024];
        let mut buffer = BumpBuffer::new(&mut mqtt_buffer);

        let mut client: Client<'_, _, _, 4, 2, 2> = Client::new(&mut buffer);

        // Connect to broker with Last Will and Testament
        let lwt = WillOptions {
            will_qos: QoS::AtMostOnce,
            will_retain: true,
            will_topic: MqttString::try_from("sensors/rubicson/status").unwrap(),
            will_payload: MqttBinary::try_from(b"offline" as &[u8]).unwrap(),
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

        match client.connect(socket, &connect_options, client_id).await {
            Ok(connect_info) => {
                info!(
                    "MQTT: Connected to broker! Session present: {}",
                    connect_info.session_present
                );
                backoff_secs = MIN_BACKOFF_SECS; // Reset backoff on successful connect

                // Reset bump buffer after connect so it can be reused for publish
                // Safety: connect is complete, buffer contents no longer needed
                unsafe { client.buffer().reset() };
            }
            Err(e) => {
                warn!(
                    "MQTT: Broker connect failed: {:?}, retry in {}s",
                    defmt::Debug2Format(&e),
                    backoff_secs
                );
                Timer::after(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        }

        // Publish loop with periodic pings
        const PING_INTERVAL_SECS: u64 = 90; // Ping every 90s (keepalive is 120s)
        let mut last_activity = Instant::now();

        // Publish "online" status message
        unsafe { client.buffer().reset() };
        let status_topic = MqttString::try_from("sensors/rubicson/status").unwrap();
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
        } else {
            info!("MQTT: Published online status");
        }

        info!("MQTT: Ready");

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
                        reading.snr_threshold
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
                Either::Second(_) => {
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
async fn time_sync_task(stack: &'static embassy_net::Stack<'static>) {
    time_sync::time_sync_loop(stack).await;
}

/// Display task - updates OLED when new readings arrive
#[embassy_executor::task]
async fn display_task(
    mut display: Sh1106Display<I2c<'static, Blocking>>,
    mut receiver: embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, RadioReading, 2>,
    mut ui: EC11RotaryEncoderInput,
) {
    info!("Display task started");

    // Get a receiver for time updates
    let mut time_receiver = TIME_WATCH.receiver().unwrap();

    // Show waiting message
    if display.show_status("Waiting...").is_err() {
        error!("Display: Failed to show status");
    }

    enum DisplayState {
        Main,
        Radio,
    }
    let mut state = DisplayState::Main;
    let mut last_reading: Option<RadioReading> = None;

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
            Either::Second(_event) => {
                // Toggle state on any event
                state = match state {
                    DisplayState::Main => DisplayState::Radio,
                    DisplayState::Radio => DisplayState::Main,
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
                let snr_threshold = last_reading.as_ref().map(|r| r.snr_threshold).unwrap_or(16); // Default to 16dB if no reading yet
                let _ = display.show_radio_info(rssi, snr_threshold);
            }
        }
    }
}

#[embassy_executor::task]
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
    let data_pin = match radio.take_data_pin() {
        Some(pin) => pin,
        None => {
            error!("Data pin already taken");
            return;
        }
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

    use esp32_rust_project::pulse_capture::PulseCapture;
    let mut capture = PulseCapture::new(data_pin, &mut radio, READING_WATCH.sender());
    capture.run().await;
}
