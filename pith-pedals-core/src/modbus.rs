//! Minimal Modbus RTU framing (CRC16, function codes 03H/06H/10H). Carries
//! no device-specific register knowledge — see `servo_jss57p` for that.
//!
//! The unit tests below check the framing against byte-for-byte worked
//! examples from Changzhou Jinsanshi Electromechanical's "JSS57-R" manual.
//! That manual is for a *different* servo than the one this crate actually
//! drives (see `servo_jss57p`'s module doc) — it's cited here only because
//! Modbus RTU CRC16/framing is a generic, device-independent algorithm, and
//! having real worked examples beats inventing test vectors.

#![allow(dead_code)]

#[derive(Debug, PartialEq, Eq)]
pub enum ModbusError {
    ShortFrame,
    BadCrc,
    Exception { function: u8, code: u8 },
    UnexpectedFunction,
    MalformedPayload,
}

/// Standard Modbus RTU CRC-16 (poly 0x8005 reflected == 0xA001, init 0xFFFF).
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

fn push_crc(frame: &mut Vec<u8>) {
    let crc = crc16(frame);
    frame.push((crc & 0xFF) as u8);
    frame.push((crc >> 8) as u8);
}

pub fn encode_read_holding(slave: u8, addr: u16, count: u16) -> Vec<u8> {
    let mut f = vec![
        slave,
        0x03,
        (addr >> 8) as u8,
        addr as u8,
        (count >> 8) as u8,
        count as u8,
    ];
    push_crc(&mut f);
    f
}

pub fn encode_write_single(slave: u8, addr: u16, value: u16) -> Vec<u8> {
    let mut f = vec![
        slave,
        0x06,
        (addr >> 8) as u8,
        addr as u8,
        (value >> 8) as u8,
        value as u8,
    ];
    push_crc(&mut f);
    f
}

pub fn encode_write_multiple(slave: u8, addr: u16, values: &[u16]) -> Vec<u8> {
    let mut f = vec![
        slave,
        0x10,
        (addr >> 8) as u8,
        addr as u8,
        ((values.len() as u16) >> 8) as u8,
        values.len() as u8,
        (values.len() * 2) as u8,
    ];
    for v in values {
        f.push((v >> 8) as u8);
        f.push(*v as u8);
    }
    push_crc(&mut f);
    f
}

/// Validates CRC and checks for a Modbus exception reply, returning the
/// slice between the function-code byte and the CRC (i.e. the payload).
fn check_and_strip(frame: &[u8], expect_function: u8) -> Result<&[u8], ModbusError> {
    if frame.len() < 4 {
        return Err(ModbusError::ShortFrame);
    }
    let (body, crc_bytes) = frame.split_at(frame.len() - 2);
    let expected = crc16(body);
    let got = crc_bytes[0] as u16 | ((crc_bytes[1] as u16) << 8);
    if expected != got {
        return Err(ModbusError::BadCrc);
    }
    let function = body[1];
    if function == expect_function | 0x80 {
        return Err(ModbusError::Exception {
            function,
            code: body[2],
        });
    }
    if function != expect_function {
        return Err(ModbusError::UnexpectedFunction);
    }
    Ok(&body[2..])
}

pub fn decode_read_holding_response(frame: &[u8]) -> Result<Vec<u16>, ModbusError> {
    let payload = check_and_strip(frame, 0x03)?;
    if payload.is_empty() {
        return Err(ModbusError::MalformedPayload);
    }
    let byte_count = payload[0] as usize;
    let data = &payload[1..];
    if data.len() != byte_count || !byte_count.is_multiple_of(2) {
        return Err(ModbusError::MalformedPayload);
    }
    Ok(data
        .chunks_exact(2)
        .map(|c| ((c[0] as u16) << 8) | c[1] as u16)
        .collect())
}

pub fn decode_write_single_ack(frame: &[u8], addr: u16, value: u16) -> Result<(), ModbusError> {
    let payload = check_and_strip(frame, 0x06)?;
    if payload.len() != 4 {
        return Err(ModbusError::MalformedPayload);
    }
    let got_addr = ((payload[0] as u16) << 8) | payload[1] as u16;
    let got_value = ((payload[2] as u16) << 8) | payload[3] as u16;
    if got_addr != addr || got_value != value {
        return Err(ModbusError::MalformedPayload);
    }
    Ok(())
}

pub fn decode_write_multiple_ack(frame: &[u8], addr: u16, count: u16) -> Result<(), ModbusError> {
    let payload = check_and_strip(frame, 0x10)?;
    if payload.len() != 4 {
        return Err(ModbusError::MalformedPayload);
    }
    let got_addr = ((payload[0] as u16) << 8) | payload[1] as u16;
    let got_count = ((payload[2] as u16) << 8) | payload[3] as u16;
    if got_addr != addr || got_count != count {
        return Err(ModbusError::MalformedPayload);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_matches_manual_read_example() {
        // "01 03 00 0A 00 02 E4 09" — read 2 registers at 0x000A.
        assert_eq!(crc16(&[0x01, 0x03, 0x00, 0x0A, 0x00, 0x02]), 0x09E4);
    }

    #[test]
    fn encode_read_holding_matches_manual_example() {
        assert_eq!(
            encode_read_holding(0x01, 0x000A, 0x0002),
            vec![0x01, 0x03, 0x00, 0x0A, 0x00, 0x02, 0xE4, 0x09]
        );
    }

    #[test]
    fn decode_read_holding_response_matches_manual_example() {
        // "01 03 04 00 01 0C 80 AF 53"
        let frame = [0x01, 0x03, 0x04, 0x00, 0x01, 0x0C, 0x80, 0xAF, 0x53];
        assert_eq!(
            decode_read_holding_response(&frame).unwrap(),
            vec![0x0001, 0x0C80]
        );
    }

    #[test]
    fn encode_write_single_matches_manual_example() {
        // "01 06 00 22 00 64 28 2B" — write 100 (0x0064) to reg 0x0022.
        assert_eq!(
            encode_write_single(0x01, 0x0022, 0x0064),
            vec![0x01, 0x06, 0x00, 0x22, 0x00, 0x64, 0x28, 0x2B]
        );
    }

    #[test]
    fn decode_write_single_ack_matches_manual_example() {
        let frame = [0x01, 0x06, 0x00, 0x22, 0x00, 0x64, 0x28, 0x2B];
        assert!(decode_write_single_ack(&frame, 0x0022, 0x0064).is_ok());
    }

    #[test]
    fn encode_write_multiple_matches_manual_example() {
        // "01 10 00 34 00 02 04 00 00 0C 80 F5 E8" — "set total number of
        // pulses to 3200" (0x0C80) across registers 0x0034/0x0035. (A
        // different worked example in the same manual, §5.2, has a CRC that
        // doesn't reproduce under the standard Modbus CRC16 no matter the
        // byte ordering tried — almost certainly an OCR/transcription typo
        // in that one example, not an algorithm error: this vector and three
        // other independent worked examples in the manual all check out.)
        assert_eq!(
            encode_write_multiple(0x01, 0x0034, &[0x0000, 0x0C80]),
            vec![0x01, 0x10, 0x00, 0x34, 0x00, 0x02, 0x04, 0x00, 0x00, 0x0C, 0x80, 0xF5, 0xE8]
        );
    }

    #[test]
    fn decode_write_multiple_ack_matches_manual_example() {
        // "01 10 00 23 00 02 B0 02"
        let frame = [0x01, 0x10, 0x00, 0x23, 0x00, 0x02, 0xB0, 0x02];
        assert!(decode_write_multiple_ack(&frame, 0x0023, 0x0002).is_ok());
    }

    #[test]
    fn exception_response_is_detected() {
        // ADDR=01 CMD=83H (03H|80H) exception=02H (illegal address) + CRC.
        let body = [0x01u8, 0x83, 0x02];
        let crc = crc16(&body);
        let mut frame = body.to_vec();
        frame.push((crc & 0xFF) as u8);
        frame.push((crc >> 8) as u8);
        assert_eq!(
            decode_read_holding_response(&frame),
            Err(ModbusError::Exception {
                function: 0x83,
                code: 0x02
            })
        );
    }

    #[test]
    fn bad_crc_is_rejected() {
        let mut frame = encode_read_holding(1, 0, 1);
        *frame.last_mut().unwrap() ^= 0xFF;
        assert_eq!(
            decode_read_holding_response(&frame),
            Err(ModbusError::BadCrc)
        );
    }
}
