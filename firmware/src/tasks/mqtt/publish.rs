use core::fmt::Write;

use defmt::{debug, trace, warn};
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::options::{PublicationOptions, TopicReference};
use rust_mqtt::types::{MqttString, QoS, TopicName};

use crate::messages::RadioReading;
use crate::time_sync::TIME_WATCH;

pub(super) enum PublishOutcome {
    Published,
    Dropped,
    Reconnect(RadioReading),
}

fn build_topic(reading: RadioReading) -> Option<heapless::String<64>> {
    let mut topic: heapless::String<64> = heapless::String::new();
    if write!(
        topic,
        "home/sensors/rubicson/{}/temperature",
        reading.inner.id
    )
    .is_err()
    {
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
        "{{\"id\":{},\"ch\":{},\"temp\":{:.1},\"batt\":\"{}\",\"rssi\":{},\"unix_s\":{}}}",
        reading.inner.id,
        reading.inner.channel,
        reading.inner.temperature_c,
        batt,
        reading.rssi,
        u32::try_from(unix_secs).unwrap_or(u32::MAX)
    )
    .is_err()
    {
        warn!("MQTT: Dropping reading due to payload format error");
        return None;
    }

    Some(payload)
}

pub(super) async fn publish_reading<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2, 0>,
    reading: RadioReading,
) -> PublishOutcome
where
    N: AsyncRead + AsyncWrite,
{
    unsafe { client.buffer_mut().reset() };

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

    let topic_name = if let Ok(topic_str) = MqttString::from_str(topic.as_str()) {
        TopicName::new_unchecked(topic_str)
    } else {
        warn!("MQTT: Dropping reading due to invalid topic");
        return PublishOutcome::Dropped;
    };

    let pub_options =
        PublicationOptions::new(TopicReference::Name(topic_name)).qos(QoS::AtLeastOnce);

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

pub(super) async fn ping<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2, 0>,
) -> Result<(), ()>
where
    N: AsyncRead + AsyncWrite,
{
    debug!("MQTT: Sending periodic ping");
    unsafe { client.buffer_mut().reset() };

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
