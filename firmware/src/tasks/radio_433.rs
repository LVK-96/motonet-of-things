use defmt::{error, info, warn};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
#[cfg(feature = "pulse_rmt")]
use embassy_sync::mutex::Mutex;
use embassy_sync::{channel, watch};
use embassy_time::{Duration, Timer};

#[cfg(feature = "pulse_rmt")]
use esp_hal::Async;
#[cfg(feature = "pulse_rmt")]
use esp_hal::rmt::{Channel as RmtChannel, Rx};

use crate::messages::{RadioReading, RadioSettings};
use crate::pulse_capture::PulseCapture;
#[cfg(feature = "pulse_rmt")]
use crate::pulse_capture::apply_pending_settings;
use crate::radio_433::{Cc1101Radio, Radio433};

type ReadingSender = watch::Sender<'static, CriticalSectionRawMutex, RadioReading, 2>;
type MqttSender = channel::Sender<'static, CriticalSectionRawMutex, RadioReading, 16>;
type SettingsReceiver = watch::Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>;
#[cfg(feature = "pulse_rmt")]
type SharedRadio = &'static Mutex<CriticalSectionRawMutex, Cc1101Radio>;

const RF_SWEEP_ENABLED: bool = true;
const RF_SWEEP_DWELL_SAMPLES: u16 = 24;
const RF_SWEEP_SAMPLE_PERIOD_MS: u64 = 50;

#[derive(Clone, Copy)]
struct RfSweepCandidate {
    name: &'static str,
    freq_hz: u32,
    bandwidth_hz: u32,
}

const RF_SWEEP_CANDIDATES: [RfSweepCandidate; 8] = [
    RfSweepCandidate {
        name: "f0-bw325",
        freq_hz: 433_920_000,
        bandwidth_hz: 325_000,
    },
    RfSweepCandidate {
        name: "f0-bw203",
        freq_hz: 433_920_000,
        bandwidth_hz: 203_000,
    },
    RfSweepCandidate {
        name: "f0-bw162",
        freq_hz: 433_920_000,
        bandwidth_hz: 162_000,
    },
    RfSweepCandidate {
        name: "f0-bw135",
        freq_hz: 433_920_000,
        bandwidth_hz: 135_000,
    },
    RfSweepCandidate {
        name: "fm100-bw203",
        freq_hz: 433_820_000,
        bandwidth_hz: 203_000,
    },
    RfSweepCandidate {
        name: "fp100-bw203",
        freq_hz: 434_020_000,
        bandwidth_hz: 203_000,
    },
    RfSweepCandidate {
        name: "fm50-bw162",
        freq_hz: 433_870_000,
        bandwidth_hz: 162_000,
    },
    RfSweepCandidate {
        name: "fp50-bw162",
        freq_hz: 433_970_000,
        bandwidth_hz: 162_000,
    },
];

#[derive(Clone, Copy, Default)]
struct SignalStats {
    sample_count: u16,
    carrier_sense_samples: u16,
    pqt_samples: u16,
    pkt_gdo0_high_samples: u16,
    pkt_gdo2_high_samples: u16,
    pin_gdo0_high_samples: u16,
    pin_gdo2_high_samples: u16,
    pin_gdo0_toggles: u16,
    min_rssi: i16,
    max_rssi: i16,
}

impl SignalStats {
    fn new() -> Self {
        Self {
            min_rssi: 0,
            max_rssi: -128,
            ..Self::default()
        }
    }

    fn score(&self) -> i32 {
        i32::from(self.pin_gdo0_toggles) * 100
            + i32::from(self.pin_gdo0_high_samples) * 10
            + i32::from(self.pkt_gdo0_high_samples)
    }
}

async fn sample_signal_stats(
    radio: &mut Cc1101Radio,
    sample_count: u16,
    sample_period: Duration,
) -> SignalStats {
    let mut stats = SignalStats::new();
    let mut previous_pin_gdo0: Option<bool> = None;

    for _ in 0..sample_count {
        if let Ok(rssi) = radio.get_rssi_dbm().await {
            if rssi < stats.min_rssi {
                stats.min_rssi = rssi;
            }
            if rssi > stats.max_rssi {
                stats.max_rssi = rssi;
            }
        }

        if let Ok(snapshot) = radio.signal_snapshot() {
            stats.sample_count += 1;
            stats.carrier_sense_samples += u16::from(snapshot.carrier_sense);
            stats.pqt_samples += u16::from(snapshot.preamble_quality_reached);
            stats.pkt_gdo0_high_samples += u16::from(snapshot.pktstatus_gdo0);
            stats.pkt_gdo2_high_samples += u16::from(snapshot.pktstatus_gdo2);
            stats.pin_gdo0_high_samples += u16::from(snapshot.pin_gdo0);
            stats.pin_gdo2_high_samples += u16::from(snapshot.pin_gdo2);
            if let Some(previous) = previous_pin_gdo0
                && previous != snapshot.pin_gdo0
            {
                stats.pin_gdo0_toggles += 1;
            }
            previous_pin_gdo0 = Some(snapshot.pin_gdo0);
        }

        Timer::after(sample_period).await;
    }

    stats
}

async fn run_rf_sweep(radio: &mut Cc1101Radio) {
    if !RF_SWEEP_ENABLED {
        return;
    }

    info!(
        "Running RF sweep ({} candidates)...",
        RF_SWEEP_CANDIDATES.len()
    );

    let mut best: Option<(RfSweepCandidate, SignalStats, i32)> = None;

    for candidate in RF_SWEEP_CANDIDATES {
        if let Err(e) = radio.apply_rf_profile(candidate.freq_hz, candidate.bandwidth_hz) {
            warn!("Sweep apply failed [{}]: {:?}", candidate.name, e);
            continue;
        }

        Timer::after(Duration::from_millis(120)).await;

        let stats = sample_signal_stats(
            radio,
            RF_SWEEP_DWELL_SAMPLES,
            Duration::from_millis(RF_SWEEP_SAMPLE_PERIOD_MS),
        )
        .await;
        let score = stats.score();

        info!(
            "Sweep [{}] f={}Hz bw={}kHz score={} toggles={} pin_gdo0={}/{} pkt_gdo0={}/{} cs={} pqt={} rssi=[{},{}]",
            candidate.name,
            candidate.freq_hz,
            candidate.bandwidth_hz / 1000,
            score,
            stats.pin_gdo0_toggles,
            stats.pin_gdo0_high_samples,
            stats.sample_count,
            stats.pkt_gdo0_high_samples,
            stats.sample_count,
            stats.carrier_sense_samples,
            stats.pqt_samples,
            stats.min_rssi,
            stats.max_rssi
        );

        if best.is_none_or(|(_, _, best_score)| score > best_score) {
            best = Some((candidate, stats, score));
        }
    }

    if let Some((candidate, stats, score)) = best {
        info!(
            "RF sweep selected [{}] f={}Hz bw={}kHz score={} toggles={} pin_gdo0={}/{}",
            candidate.name,
            candidate.freq_hz,
            candidate.bandwidth_hz / 1000,
            score,
            stats.pin_gdo0_toggles,
            stats.pin_gdo0_high_samples,
            stats.sample_count
        );
        if let Err(e) = radio.apply_rf_profile(candidate.freq_hz, candidate.bandwidth_hz) {
            warn!("Failed to apply selected sweep profile: {:?}", e);
        }
    } else {
        warn!("RF sweep did not produce a valid candidate");
    }
}

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

    run_rf_sweep(radio).await;

    let stats = sample_signal_stats(radio, 60, Duration::from_millis(50)).await;
    info!("RSSI: min={} max={} dBm", stats.min_rssi, stats.max_rssi);
    info!(
        "Signal samples={} cs={} pqt={} pkt_gdo0={} pin_gdo0={} toggles={} pkt_gdo2={} pin_gdo2={}",
        stats.sample_count,
        stats.carrier_sense_samples,
        stats.pqt_samples,
        stats.pkt_gdo0_high_samples,
        stats.pin_gdo0_high_samples,
        stats.pin_gdo0_toggles,
        stats.pkt_gdo2_high_samples,
        stats.pin_gdo2_high_samples
    );
    match radio.probe_gdo0_edge_path().await {
        Ok(edge_seen) => info!("GDO0 edge probe (clock mode): edge_seen={}", edge_seen),
        Err(e) => warn!("GDO0 edge probe failed: {:?}", e),
    }
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
    shared_radio: SharedRadio,
    rmt_rx: RmtChannel<'static, Async, Rx>,
    reading_sender: ReadingSender,
    mqtt_sender: MqttSender,
) {
    {
        let mut radio = shared_radio.lock().await;
        if prepare_radio_for_capture(&mut radio).await.is_err() {
            return;
        }
    }

    let mut capture = PulseCapture::new(rmt_rx, shared_radio, reading_sender, mqtt_sender);
    capture.run().await;
}

#[cfg(feature = "pulse_rmt")]
#[embassy_executor::task]
pub async fn radio_433_settings_task(
    shared_radio: SharedRadio,
    mut settings_receiver: SettingsReceiver,
) {
    loop {
        settings_receiver.changed().await;
        let mut radio = shared_radio.lock().await;
        apply_pending_settings(&mut *radio, &mut settings_receiver).await;
    }
}
