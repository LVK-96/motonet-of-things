use defmt::{error, info};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::{channel, watch};
use embassy_time::{Duration, Timer};

#[cfg(feature = "pulse_rmt")]
use esp_hal::Async;
#[cfg(feature = "pulse_rmt")]
use esp_hal::rmt::{Channel as RmtChannel, Rx};

use crate::messages::{RadioReading, RadioSettings};
use crate::pulse_capture::PulseCapture;
use crate::radio_433::{Cc1101Radio, Radio433};

type ReadingSender = watch::Sender<'static, CriticalSectionRawMutex, RadioReading, 2>;
type MqttSender = channel::Sender<'static, CriticalSectionRawMutex, RadioReading, 16>;
type SettingsReceiver = watch::Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>;

async fn prepare_radio_for_capture(radio: &mut Cc1101Radio) -> Result<(), ()> {
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
            return Err(());
        }
    }

    if let Err(e) = radio.set_receive_mode().await {
        error!("Failed to set receive mode: {:?}", e);
        return Err(());
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
    Ok(())
}

#[cfg(feature = "pulse_sw")]
#[embassy_executor::task]
pub async fn radio_433_rx_task(
    mut radio: Cc1101Radio,
    reading_sender: ReadingSender,
    mqtt_sender: MqttSender,
    settings_receiver: SettingsReceiver,
) {
    if prepare_radio_for_capture(&mut radio).await.is_err() {
        return;
    }

    // Take ownership of the data pin for pulse capture
    let Some(data_pin) = radio.take_data_pin() else {
        error!("Data pin already taken");
        return;
    };

    let mut capture = PulseCapture::new(
        data_pin,
        &mut radio,
        reading_sender,
        mqtt_sender,
        settings_receiver,
    );
    capture.run().await;
}

#[cfg(feature = "pulse_rmt")]
#[embassy_executor::task]
pub async fn radio_433_rx_task(
    mut radio: Cc1101Radio,
    rmt_rx: RmtChannel<'static, Async, Rx>,
    reading_sender: ReadingSender,
    mqtt_sender: MqttSender,
    settings_receiver: SettingsReceiver,
) {
    if prepare_radio_for_capture(&mut radio).await.is_err() {
        return;
    }

    let mut capture = PulseCapture::new(
        rmt_rx,
        &mut radio,
        reading_sender,
        mqtt_sender,
        settings_receiver,
    );
    capture.run().await;
}
