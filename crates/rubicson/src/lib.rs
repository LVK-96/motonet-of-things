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
pub enum BreakResetIgnore {
    Break,
    Reset,
    Ignore,
}

fn decode_gap(gap: u32) -> Result<u8, BreakResetIgnore> {
    // Short pulse ~1000us, the sensor sends a short pulse ~500us right before the break
    let zero_lower_limit = 750;
    let zero_upper_limit = 1500;

    // Long pulse ~2000us
    let one_upper_limit = 2500;

    // Breaks between packets ~4000us
    let break_upper_limit = 4500;

    // Decode the received pulse
    if gap > break_upper_limit {
        Err(BreakResetIgnore::Reset)
    } else if gap > one_upper_limit {
        // End of packet
        Err(BreakResetIgnore::Break)
    } else if gap > zero_upper_limit {
        // Long gap -> 1
        Ok(1)
    } else if gap > zero_lower_limit {
        // Short gap -> 0
        Ok(0)
    } else {
        Err(BreakResetIgnore::Ignore)
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

pub fn decode_gaps(pulses: &[u32]) -> Result<(usize, RubicsonReading), [u8; 5 * 12]> {
    let mut bit_buffer = [0u8; 5 * 12]; // Support up to 12 packets of 5 bytes
    let mut bit_index = 0;
    let mut bit_buffer_row = 0;
    for &gap in pulses.iter() {
        // Calculate start byte for the current row (packet)
        let row_start_byte = bit_buffer_row * 5;
        match decode_gap(gap) {
            Ok(bit) => {
                let row_slice = &mut bit_buffer[row_start_byte..(row_start_byte + 5)];
                add_bit(row_slice, bit_index, bit);
                bit_index += 1;
            }
            Err (e) => {
                match e {
                    BreakResetIgnore::Ignore => {
                        // Ignore the short gap before the break 
                        continue;
                    }
                    BreakResetIgnore::Break => {
                        // We have a complete packet, try to decode
                        let row_slice = &bit_buffer[row_start_byte..(row_start_byte + 5)];
                        if let Ok(reading) = decode_rubicson(row_slice) {
                            // Successfully decoded a packet
                            return Ok((bit_buffer_row, reading));
                        }
                        // If the decode failed, we continue to the next packet
                        bit_buffer_row += 1;
                        bit_index = 0;
                        continue;
                    }
                    BreakResetIgnore::Reset => {
                        // We are done, we were unable to decode the packets
                        return Err(bit_buffer);
                    }
                }
            }
        }
    }

    // Final attempt to decode the last packet if we have 36 bits
    if bit_index == 35 {
        let row_start_byte = bit_buffer_row * 5;
        let row_slice = &bit_buffer[row_start_byte..(row_start_byte + 5)];
        if let Ok(reading) = decode_rubicson(row_slice) {
            // Successfully decoded a packet
            return Ok((bit_buffer_row, reading));
        }
    }

    Err(bit_buffer) // Unable to decode the gaps
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
    use std::fs::File;
    use std::io::{BufRead, BufReader};
    use std::path::Path;
    use std::convert::AsRef;

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

        // Add packet separator (Break range 2500-4500µs triggers decode)
        gaps.push(4000);

        gaps
    }
    
    fn captured_data_to_gaps<P: AsRef<Path>>(gaps_file: P) -> Vec<u32> {
        let f = File::open(gaps_file.as_ref()).expect("Failed to open gaps file");
        let reader = BufReader::new(f);
        let mut gaps: Vec<u32> = Vec::new();
        for line in reader.lines().filter_map(Result::ok) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue; // Skip empty lines and comments
            }
            if let Ok(gap) = line.trim().parse::<u32>() {
                gaps.push(gap);
            }
        }
        gaps
    }

    // ==================== TESTS ====================
    
    #[test]
    fn test_decode_captured_data() {
        let gaps = captured_data_to_gaps("test_data/sample_gaps.txt");
        
        // Decode the whole captured data, like in the real use case
        let (valid_packet_idx, result) = decode_gaps(&gaps).expect("Failed to decode captured gaps");
        // Expected: ID=230, Channel=1, BatteryOK=true, Temp=-9.4C
        assert_eq!(result.id, 230);
        assert_eq!(result.channel, 1);
        assert!(result.battery_ok);
        assert!((result.temperature_c - (-9.4)).abs() < 0.1);
        assert_eq!(result.crc_ok, true);
        // The second packet is the first valid one
        assert_eq!(valid_packet_idx, 1);
        
        // The first packet seems to be corrupt in the captured data
        let result = decode_gaps(&gaps[0..36]);
        assert!(result.is_err(), "Expected first packet to be invalid");
        let bits = result.unwrap_err();
        assert!(matches!(decode_rubicson(&bits), Err(DecodeError::CrcMismatch)));

        // Packets 3 and onwards should be valid as well
        for packet_nro in 2..12 {
            let start_bit = packet_nro * 38; // Each packet 36 gaps + 2 (short + break)
            let end_bit = (start_bit + 38).min(gaps.len() - 1); // Last packet does not have tailing short + break
            let result = decode_gaps(&gaps[start_bit..end_bit]);
            match result {
                Ok((_idx, reading)) => {
                    assert_eq!(reading.id, 230);
                    assert_eq!(reading.channel, 1);
                    assert!(reading.battery_ok);
                    assert!((reading.temperature_c - (-9.4)).abs() < 0.1);
                    assert_eq!(reading.crc_ok, true);
                }
                Err(bits) => {
                    // Try to decode
                    let try_to_decode= decode_rubicson(&bits);
                    match try_to_decode {
                        Ok(reading) => {
                            assert_eq!(reading.id, 230);
                            assert_eq!(reading.channel, 1);
                            assert!(reading.battery_ok);
                            assert!((reading.temperature_c - (-9.4)).abs() < 0.1);
                            assert_eq!(reading.crc_ok, true);
                        }
                        Err(e) => {
                            panic!("Packet {} failed to decode: {:?}", packet_nro + 1, e);
                        }
                    }       
                }
            }           
        }

            

    }

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

        let (_valid_packet_idx, r) = decode_gaps(&gaps).expect("Decode burst failed");
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
        let (_valid_packet_idx, result) = decode_gaps(&gaps).expect("End-to-end decode failed");

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
        // Test Too Short to be valid -> ignore
        assert_eq!(decode_gap(499), Err(BreakResetIgnore::Ignore));

        // Test Short Gap (0) -> Gap <= 1500
        assert_eq!(decode_gap(1000), Ok(0));

        // Test Long Gap (1) -> Gap > 1500 and <= 3000
        assert_eq!(decode_gap(2000), Ok(1));

        // Test Break -> Gap > 3000
        assert_eq!(decode_gap(3500), Err(BreakResetIgnore::Break));

        // Test Reset -> Gap > 4000
        assert_eq!(decode_gap(5000), Err(BreakResetIgnore::Reset));
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
