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
    InvalidConst,
    CrcMismatch,
}

pub fn decode_rubicson(bits: &[u8], bit_count: usize) -> Result<RubicsonReading, DecodeError> {
    // Expected 36 bits (4.5 bytes). We need at least 5 bytes buffer.
    if bit_count < 36 {
        return Err(DecodeError::InvalidLength);
    }

    let b = bits;

    // The protocol structure usually is 9 nibbles:
    // [id0] [id1] [bat|chan] [temp0] [temp1] [temp2] [0xf] [crc1] [crc2]
    // b[3] corresponds to Nibbles 6 and 7. The upper nibble of b[3] is Nibble 6, which must be 0xF.
    if (b[3] & 0xf0) != 0xf0 {
        return Err(DecodeError::InvalidConst);
    }

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

    #[test]
    fn test_calculate_crc8() {
        let msg = [0x12, 0x34];
        let c = calculate_crc8(&msg, 0x31, 0x6c);
        let mut msg_with_crc = [0x12, 0x34, c];
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

        let r = decode_rubicson(&payload, 36).expect("Decode failed");
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

        let r = decode_rubicson(&payload, 36).expect("Decode failed");
        assert_eq!(r.temperature_c, -10.5);
    }

    #[test]
    fn test_invalid_length() {
        let payload = [0u8; 4];
        assert!(matches!(
            decode_rubicson(&payload, 30),
            Err(DecodeError::InvalidLength)
        ));
    }
}
