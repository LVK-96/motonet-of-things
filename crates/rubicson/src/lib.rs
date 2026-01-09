#![cfg_attr(not(test), no_std)]

#[cfg(feature = "defmt")]
use defmt::Format;

#[derive(Format, Clone, Debug)]
pub struct RubicsonReading {
    pub id: u8,
    pub channel: u8,
    pub battery_ok: bool,
    pub temperature_c: f32,
    pub crc_ok: bool,
}

#[derive(Format, Clone, Debug)]
pub enum DecodeError {
    InvalidLength,
    CrcMismatch,
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub enum BreakReset {
    Break,
    Reset,
}

fn decode_gap(gap: u32) -> Result<u8, BreakReset> {
    // Short pulse ~1000us
    let zero_upper_limit = 1500;

    // Long pulse ~2000us
    let one_upper_limit = 3000;

    // Breaks between packets ~3000us
    // 4000us -> reset
    let break_limit = 4000;

    // Decode the received pulse
    if gap > break_limit {
        Err(BreakReset::Reset)
    } else if gap > one_upper_limit {
        // End of packet
        Err(BreakReset::Break)
    } else if gap > zero_upper_limit {
        // Long gap -> 1
        Ok(1)
    } else {
        // Short gap -> 0
        Ok(0)
    }
}

fn add_bit(buf: &mut [u8], index: usize, val: u8) {
    if index / 8 >= buf.len() {
        return;
    }

    if val != 0 {
        buf[index / 8] |= 1 << (7 - (index % 8));
    }
}

pub fn decode_gaps(pulses: &[u32]) -> Result<RubicsonReading, ()> {
    let mut bit_buffer = [0u8; 5 * 12]; // Support up to 12 packets of 5 bytes
    let mut bit_index = 0;
    let mut bit_buffer_row = 0;

    for &gap in pulses.iter() {
        // Calculate start byte for the current row (packet)
        let row_start_byte = bit_buffer_row * 5;

        match decode_gap(gap) {
            Ok(bit) => {
                if let Some(row_slice) = bit_buffer.get_mut(row_start_byte..row_start_byte + 5) {
                    add_bit(row_slice, bit_index, bit);
                }
                bit_index += 1;
            }
            Err(e) => {
                // If the break or reset is at bit index 36 we might have a complete packet
                if bit_index == 36 {
                    if let Some(row_slice) = bit_buffer.get_mut(row_start_byte..row_start_byte + 5)
                    {
                        // The same packet is send 12 time in a row
                        // If we are able to decode one we can return
                        match decode_rubicson(row_slice) {
                            Ok(result) => {
                                #[cfg(feature = "defmt")]
                                defmt::info!("Decoded: {:?}", result);
                                return Ok(result);
                            }
                            Err(decode_error) => {
                                #[cfg(feature = "defmt")]
                                defmt::info!("Decode error: {:?}", decode_error);
                            }
                        }
                    }
                } else {
                    // The break or reset is not at bit index 36
                    // If it is a reset we are done, we were unable to decode the packets
                    match e {
                        BreakReset::Reset => return Err(()),
                        _ => {}
                    }

                    // Otherwise move on to the next packet
                    bit_buffer_row += 1;
                    bit_index = 0;
                }
            }
        }
    }

    Err(()) // Unable to decode the gaps
}

fn decode_rubicson(bits: &[u8]) -> Result<RubicsonReading, DecodeError> {
    let b = bits;

    // The protocol structure is 9 nibbles (4 bits):
    // [id0] [id1] [bat|chan] [temp0] [temp1] [temp2] [0xf] [crc1] [crc2]

    // CRC Calculation
    // CRC8 poly 0x31, init 0x6c.
    // The CRC is calculated over the first 7 nibbles (up to and including the 0xF constant).
    // b[3] & 0xF0 ensures we include the 0xF nibble and 0-pad the rest for the calculation buffer.
    // The received CRC corresponds to the last two nibbles (Nibbles 7 and 8).
    let mut crc_buf = [0u8; 5];
    crc_buf[0] = b[0];
    crc_buf[1] = b[1];
    crc_buf[2] = b[2];
    crc_buf[3] = b[3] & 0xF0;

    // Construct received CRC from the last two nibbles
    // Nibble 7 is lower nibble of b[3]. Nibble 8 is upper nibble of b[4].
    crc_buf[4] = (b[3] & 0x0F) << 4 | (b[4] & 0xF0) >> 4;

    // The calculated CRC over the whole sequence (including the received CRC byte) should be 0
    // if the CRC is correct and standard CRC8 properties apply (or we check payload vs CRC explicitly).
    // rtl_433 checks if crc8(all_5_bytes) == 0.
    if calculate_crc8(&crc_buf, 0x31, 0x6c) != 0 {
        return Err(DecodeError::CrcMismatch);
    }

    let id = b[0];
    let battery_ok = (b[1] & 0x80) != 0;
    let channel = ((b[1] & 0x30) >> 4) + 1;

    // Temperature: 12 bit signed integer, scaled by 10.
    // Data is in Nibbles 3, 4, 5.
    // Nibble 3 is lower nibble of b[1].
    // Nibble 4 is upper nibble of b[2].
    // Nibble 5 is lower nibble of b[2].
    // Combined: [N3] [N4] [N5]
    let temp_raw_high = (b[1] as u16) << 12; // Shift N3 to top
    let temp_raw_low = (b[2] as u16) << 4; // Shift N4 N5 to follow

    // Sign-extend from 12 bits to 16 bits
    let temp_raw = (temp_raw_high | temp_raw_low) as i16;
    let temp_c = (temp_raw >> 4) as f32 * 0.1;

    Ok(RubicsonReading {
        id,
        channel,
        battery_ok,
        temperature_c: temp_c,
        crc_ok: true,
    })
}

fn calculate_crc8(data: &[u8], poly: u8, mut crc: u8) -> u8 {
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if (crc & 0x80) != 0 {
                crc = (crc << 1) ^ poly;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== TEST UTILITIES ====================

    /// Build a valid 5-byte Rubicson payload with CRC
    fn build_payload(id: u8, channel: u8, battery_ok: bool, temp_raw: u16) -> [u8; 5] {
        let mut payload = [0u8; 5];

        // Byte 0: ID
        payload[0] = id;

        // Byte 1: [Battery:1][??:1][Channel:2][TempHigh:4]
        // Channel stored as (ch-1)
        let temp_high_nibble = ((temp_raw >> 8) & 0x0F) as u8;
        payload[1] =
            (if battery_ok { 0x80 } else { 0x00 }) | ((channel - 1) << 4) | temp_high_nibble;

        // Byte 2: TempLow (lower 8 bits)
        payload[2] = (temp_raw & 0xFF) as u8;

        // Byte 3: [Constant 0xF][CRC high nibble]
        payload[3] = 0xF0;

        // Byte 4: [CRC low nibble][0000]
        payload[4] = 0x00;

        // Calculate and embed CRC
        let prefix = [payload[0], payload[1], payload[2], payload[3]];
        let crc_val = calculate_crc8(&prefix, 0x31, 0x6c);
        payload[3] |= (crc_val & 0xF0) >> 4;
        payload[4] |= (crc_val & 0x0F) << 4;

        payload
    }

    /// Convert a 5-byte payload to gap timings (simulates radio output)
    /// Short gap (1000µs) = bit 0, Long gap (2000µs) = bit 1
    fn payload_to_gaps(payload: &[u8; 5]) -> std::vec::Vec<u32> {
        let mut gaps = std::vec::Vec::new();

        let mut bit_count = 0;
        for byte_idx in 0..5 {
            let byte = payload[byte_idx];
            for bit_pos in (0..8).rev() {
                if bit_count >= 36 {
                    break;
                }

                let bit = (byte >> bit_pos) & 1;
                gaps.push(if bit == 1 { 2000u32 } else { 1000u32 });
                bit_count += 1;
            }
        }

        // Add packet separator (triggers decode)
        gaps.push(5000);

        gaps
    }

    // ==================== TESTS ====================

    #[test]
    fn test_calculate_crc8() {
        let msg = [0x12, 0x34];
        let c = calculate_crc8(&msg, 0x31, 0x6c);
        let msg_with_crc = [0x12, 0x34, c];
        assert_eq!(calculate_crc8(&msg_with_crc, 0x31, 0x6c), 0);
    }

    #[test]
    fn test_decode_valid_packet() {
        let mut payload = [0u8; 5];
        payload[0] = 0x0C; // ID
        payload[1] = 0x80; // Bat OK, Chan 1, TempHi 0
        payload[2] = 0xD7; // Temp 0xD7 = 215
        payload[3] = 0xF0; // N6=F
        payload[4] = 0x00;

        let prefix = [payload[0], payload[1], payload[2], payload[3]];
        let crc_val = calculate_crc8(&prefix, 0x31, 0x6c);

        // Pack CRC (N7 N8)
        let n7 = (crc_val & 0xF0) >> 4;
        let n8 = crc_val & 0x0F;

        payload[3] |= n7;
        payload[4] |= n8 << 4;

        let r = decode_rubicson(&payload).expect("Decode failed");
        assert_eq!(r.id, 12);
        assert_eq!(r.battery_ok, true);
        assert_eq!(r.channel, 1);
        assert!((r.temperature_c - 21.5).abs() < 0.01);
    }

    #[test]
    fn test_decode_negative_temp() {
        // -10.5 C
        let mut payload = [0u8; 5];
        payload[0] = 0x0A;
        payload[1] = 0x1F;
        payload[2] = 0x97;
        payload[3] = 0xF0;

        let prefix = [payload[0], payload[1], payload[2], payload[3]];
        let crc_val = calculate_crc8(&prefix, 0x31, 0x6c);

        let n7 = (crc_val & 0xF0) >> 4;
        let n8 = crc_val & 0x0F;

        payload[3] |= n7;
        payload[4] |= n8 << 4;

        let r = decode_rubicson(&payload).expect("Decode failed");
        assert_eq!(r.temperature_c, -10.5);
    }

    #[test]
    fn test_decode_gaps() {
        // ID=12, Channel=1, BatteryOK=true, Temp=21.5C (raw=215)
        let payload = build_payload(0x0C, 1, true, 215);

        // Convert to gaps and repeat for burst
        let mut gaps = payload_to_gaps(&payload);
        let single_packet = gaps.clone();
        gaps.extend_from_slice(&single_packet);
        gaps.extend_from_slice(&single_packet);

        let r = decode_gaps(&gaps).expect("Decode burst failed");
        assert_eq!(r.id, 12);
        assert!((r.temperature_c - 21.5).abs() < 0.1);
    }

    /// End-to-end test: Raw gaps → Bits → Decoded Message
    #[test]
    fn test_end_to_end_decode() {
        // Define expected sensor values
        let expected_id: u8 = 0xAB;
        let expected_channel: u8 = 2;
        let expected_battery_ok = true;
        let expected_temp_raw: u16 = 253; // 25.3°C

        // Build payload and convert to gaps
        let payload = build_payload(
            expected_id,
            expected_channel,
            expected_battery_ok,
            expected_temp_raw,
        );
        let gaps = payload_to_gaps(&payload);

        // Run full decode pipeline
        let result = decode_gaps(&gaps).expect("End-to-end decode failed");

        // Verify decoded values
        assert_eq!(result.id, expected_id, "ID mismatch");
        assert_eq!(result.channel, expected_channel, "Channel mismatch");
        assert_eq!(
            result.battery_ok, expected_battery_ok,
            "Battery status mismatch"
        );
        assert!(
            (result.temperature_c - 25.3).abs() < 0.1,
            "Temperature mismatch: expected 25.3, got {}",
            result.temperature_c
        );
        assert!(result.crc_ok, "CRC validation failed");
    }

    #[test]
    fn test_decode_gap_timing() {
        // Test Short Gap (0) -> Gap <= 1500
        assert_eq!(decode_gap(1000), Ok(0));

        // Test Long Gap (1) -> Gap > 1500 and <= 3000
        assert_eq!(decode_gap(2000), Ok(1));

        // Test Break -> Gap > 3000
        assert_eq!(decode_gap(3500), Err(BreakReset::Break));

        // Test Reset -> Gap > 4000
        assert_eq!(decode_gap(5000), Err(BreakReset::Reset));
    }

    /// End-to-end test for a FAILING packet (corrupted data / CRC mismatch)
    #[test]
    fn test_end_to_end_decode_failure() {
        // Build valid payload then corrupt it
        let mut payload = build_payload(0x42, 2, true, 0xAB);

        // Corrupt the packet (flip a bit) - this will cause CRC mismatch
        payload[1] ^= 0x08;

        // Convert to gaps
        let gaps = payload_to_gaps(&payload);

        // Verify decode fails
        let result = decode_gaps(&gaps);
        assert!(
            result.is_err(),
            "Expected decode to fail due to CRC mismatch"
        );
    }

    /// Test that pure noise (random short gaps) is rejected
    #[test]
    fn test_noise_rejection() {
        // Random noise - gaps that don't form a valid packet
        let gaps: Vec<u32> = vec![
            500, 800, 600, 1200, 900, 700, 1100, 800,  // Various gaps
            5000, // Reset
        ];

        let result = decode_gaps(&gaps);
        assert!(result.is_err(), "Expected noise to be rejected");
    }
}
