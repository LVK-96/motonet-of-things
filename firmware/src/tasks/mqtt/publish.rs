use core::fmt::Write;

use defmt::{debug, trace, warn};
use rust_mqtt::Bytes;
use rust_mqtt::client::options::PublicationOptions;
use rust_mqtt::types::{MqttString, QoS, TopicName};

use crate::messages::RadioReading;
use crate::time_sync::TIME_WATCH;

use super::MqttClient;

pub(super) enum PublishOutcome {
    Published,
    Dropped,
    Reconnect(RadioReading),
}

fn build_topic(reading: RadioReading) -> Option<heapless::String<64>> {
    let mut topic: heapless::String<64> = heapless::String::new();
    if write!(topic, "sensors/rubicson/{}/temperature", reading.inner.id).is_err() {
        warn!("MQTT: Dropping reading due to topic format error");
        return None;
    }

    Some(topic)
}

fn build_payload(reading: RadioReading) -> Option<heapless::String<128>> {
    let current_time = TIME_WATCH.anon_receiver().try_get().flatten();
    let unix_secs = current_time.map_or(0, |time_ref| time_ref.now_unix_secs());
    let batt = if reading.inner.battery_ok {
        "ok"
    } else {
        "low"
    };

    let mut payload: heapless::String<128> = heapless::String::new();
    if write!(
        payload,
        "id={},ch={},temp={:.1},batt={},rssi={},snr={},unix_s={}",
        reading.inner.id,
        reading.inner.channel,
        reading.inner.temperature_c,
        batt,
        reading.rssi,
        reading.detection_threshold,
        u32::try_from(unix_secs).unwrap_or(u32::MAX)
    )
    .is_err()
    {
        warn!("MQTT: Dropping reading due to payload format error");
        return None;
    }

    Some(payload)
}

pub(super) async fn publish_reading(
    client: &mut MqttClient<'_>,
    reading: RadioReading,
) -> PublishOutcome {
    unsafe { client.buffer().reset() };

    let Some(topic) = build_topic(reading) else {
        return PublishOutcome::Dropped;
    };

    let Some(payload) = build_payload(reading) else {
        return PublishOutcome::Dropped;
    };

    trace!(
        "MQTT: Publishing to {} : {}",
        topic.as_str(),
        payload.as_str()
    );

    let topic_name = if let Ok(topic_str) = MqttString::from_slice(topic.as_str()) {
        unsafe { TopicName::new_unchecked(topic_str) }
    } else {
        warn!("MQTT: Dropping reading due to invalid topic");
        return PublishOutcome::Dropped;
    };

    let pub_options = PublicationOptions {
        retain: false,
        topic: topic_name,
        qos: QoS::AtLeastOnce, // QoS 1
    };

    if let Err(e) = client
        .publish(&pub_options, Bytes::from(payload.as_bytes()))
        .await
    {
        warn!(
            "MQTT: Publish failed: {:?}, reconnecting...",
            defmt::Debug2Format(&e)
        );
        return PublishOutcome::Reconnect(reading);
    }

    PublishOutcome::Published
}

pub(super) async fn ping(client: &mut MqttClient<'_>) -> Result<(), ()> {
    debug!("MQTT: Sending periodic ping");
    unsafe { client.buffer().reset() };

    if let Err(e) = client.ping().await {
        warn!(
            "MQTT: Ping failed: {:?}, reconnecting...",
            defmt::Debug2Format(&e)
        );
        return Err(());
    }

    debug!("MQTT: Ping sent successfully");
    Ok(())
}
