const PREAMBLE: u8 = 0xD3;
const HEADER_LEN: usize = 3;
const CRC_LEN: usize = 3;
const MAX_BUFFER: usize = 64 * 1024;

#[derive(Debug)]
pub struct Frame {
    pub msg_type: u16,
    pub bytes: Vec<u8>,
}

impl Frame {
    pub fn from_payload(payload: &[u8]) -> Frame {
        assert!(payload.len() >= 2 && payload.len() < 1024);
        let mut bytes = vec![PREAMBLE, (payload.len() >> 8) as u8, payload.len() as u8];
        bytes.extend_from_slice(payload);
        let crc = crc24q(&bytes);
        bytes.extend_from_slice(&crc.to_be_bytes()[1..]);
        let msg_type = ((payload[0] as u16) << 4) | ((payload[1] as u16) >> 4);
        Frame { msg_type, bytes }
    }

    /// DF003 reference station ID, which follows the message type in 1005,
    /// 1006, MSM and 1230 messages (not ephemeris).
    pub fn station_id(&self) -> Option<u16> {
        let p = &self.bytes[HEADER_LEN..self.bytes.len() - CRC_LEN];
        let has_df003 = matches!(self.msg_type, 1001..=1012 | 1033 | 1070..=1137 | 1230);
        (has_df003 && p.len() >= 3).then(|| (((p[1] & 0x0F) as u16) << 8) | p[2] as u16)
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, value: u64, n: usize) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> i) & 1 == 1 {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }

    /// Counter byte followed by the characters (DF029/030, DF032/033, DF227/228, ...).
    fn put_str(&mut self, s: &str) {
        self.put(s.len() as u64, 8);
        for b in s.bytes() {
            self.put(b as u64, 8);
        }
    }
}

/// 1008: antenna descriptor and serial number.
pub fn msg_1008(station: u16, antenna: &str) -> Frame {
    let mut w = BitWriter::default();
    w.put(1008, 12);
    w.put(station as u64, 12);
    w.put_str(antenna);
    w.put(0, 8); // DF031 antenna setup ID
    w.put_str(""); // antenna serial
    Frame::from_payload(&w.bytes)
}

/// 1033: antenna and receiver descriptors.
pub fn msg_1033(station: u16, antenna: &str, receiver: &str, firmware: &str) -> Frame {
    let mut w = BitWriter::default();
    w.put(1033, 12);
    w.put(station as u64, 12);
    w.put_str(antenna);
    w.put(0, 8); // DF031 antenna setup ID
    w.put_str(""); // antenna serial
    w.put_str(receiver);
    w.put_str(firmware);
    w.put_str(""); // receiver serial
    Frame::from_payload(&w.bytes)
}

/// Reassembles a raw byte stream into CRC-validated RTCM3 frames.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
    pub skipped_bytes: u64,
}

impl Framer {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        if self.buf.len() > MAX_BUFFER {
            let excess = self.buf.len() - MAX_BUFFER;
            self.buf.drain(..excess);
            self.skipped_bytes += excess as u64;
        }
    }

    pub fn next_frame(&mut self) -> Option<Frame> {
        loop {
            match self.buf.iter().position(|&b| b == PREAMBLE) {
                Some(0) => {}
                Some(i) => {
                    self.buf.drain(..i);
                    self.skipped_bytes += i as u64;
                }
                None => {
                    self.skipped_bytes += self.buf.len() as u64;
                    self.buf.clear();
                    return None;
                }
            }

            if self.buf.len() < HEADER_LEN {
                return None;
            }

            // Top 6 bits after the preamble are reserved and must be zero.
            if self.buf[1] & 0xFC != 0 {
                self.drop_one();
                continue;
            }

            let payload_len = (((self.buf[1] & 0x03) as usize) << 8) | self.buf[2] as usize;
            let total = HEADER_LEN + payload_len + CRC_LEN;
            if self.buf.len() < total {
                return None;
            }

            let body_end = HEADER_LEN + payload_len;
            let expected = u32::from_be_bytes([0, self.buf[body_end], self.buf[body_end + 1], self.buf[body_end + 2]]);
            if crc24q(&self.buf[..body_end]) != expected {
                self.drop_one();
                continue;
            }

            let bytes: Vec<u8> = self.buf.drain(..total).collect();
            let msg_type = if payload_len >= 2 {
                ((bytes[3] as u16) << 4) | ((bytes[4] as u16) >> 4)
            } else {
                0
            };
            return Some(Frame { msg_type, bytes });
        }
    }

    fn drop_one(&mut self) {
        self.buf.drain(..1);
        self.skipped_bytes += 1;
    }
}

pub fn crc24q(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &b in data {
        crc ^= (b as u32) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x0100_0000 != 0 {
                crc ^= 0x0186_4CFB;
            }
        }
    }
    crc & 0x00FF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    // Message 1005 example from the RTCM 10403 standard.
    const SAMPLE_1005: [u8; 25] = [
        0xD3, 0x00, 0x13, 0x3E, 0xD7, 0xD3, 0x02, 0x02, 0x98, 0x0E, 0xDE, 0xEF, 0x34, 0xB4, 0xBD,
        0x62, 0xAC, 0x09, 0x41, 0x98, 0x6F, 0x33, 0x36, 0x0B, 0x98,
    ];

    #[test]
    fn crc_matches_standard_example() {
        assert_eq!(crc24q(&SAMPLE_1005[..22]), 0x360B98);
    }

    #[test]
    fn parses_frame_and_type() {
        let mut f = Framer::default();
        f.push(&SAMPLE_1005);
        let frame = f.next_frame().unwrap();
        assert_eq!(frame.msg_type, 1005);
        assert_eq!(frame.bytes, SAMPLE_1005);
        assert!(f.next_frame().is_none());
        assert_eq!(f.skipped_bytes, 0);
    }

    #[test]
    fn resyncs_after_garbage_and_split_reads() {
        let mut f = Framer::default();
        let mut stream = vec![0xB5, 0x62, 0xD3, 0x00, 0x05, 0x01];
        stream.extend_from_slice(&SAMPLE_1005);
        stream.extend_from_slice(&SAMPLE_1005);

        let (a, b) = stream.split_at(15);
        f.push(a);
        assert!(f.next_frame().is_none());
        f.push(b);
        assert_eq!(f.next_frame().unwrap().msg_type, 1005);
        assert_eq!(f.next_frame().unwrap().msg_type, 1005);
        assert!(f.next_frame().is_none());
        assert_eq!(f.skipped_bytes, 6);
    }

    #[test]
    fn station_id_from_1005() {
        let mut f = Framer::default();
        f.push(&SAMPLE_1005);
        // The standard's example uses reference station ID 2003.
        assert_eq!(f.next_frame().unwrap().station_id(), Some(2003));
    }

    #[test]
    fn encodes_1008_and_1033() {
        let f = msg_1008(2003, "ADVNULLANTENNA");
        assert_eq!(f.msg_type, 1008);
        assert_eq!(f.station_id(), Some(2003));
        let p = &f.bytes[3..f.bytes.len() - 3];
        assert_eq!(&p[..3], &[0x3F, 0x07, 0xD3]);
        assert_eq!(p[3], 14);
        assert_eq!(&p[4..18], b"ADVNULLANTENNA");
        assert_eq!(&p[18..], &[0, 0]);

        let f = msg_1033(5, "ANT", "U-blox_ZED-F9P", "HPG 1.32");
        assert_eq!(f.station_id(), Some(5));
        let p = &f.bytes[3..f.bytes.len() - 3];
        let mut expected = vec![0x40, 0x90, 0x05, 3];
        expected.extend_from_slice(b"ANT");
        expected.extend_from_slice(&[0, 0, 14]);
        expected.extend_from_slice(b"U-blox_ZED-F9P");
        expected.push(8);
        expected.extend_from_slice(b"HPG 1.32");
        expected.push(0);
        assert_eq!(p, expected.as_slice());

        let mut framer = Framer::default();
        framer.push(&f.bytes);
        assert_eq!(framer.next_frame().unwrap().bytes, f.bytes);
    }

    #[test]
    fn rejects_corrupted_crc() {
        let mut bad = SAMPLE_1005;
        bad[10] ^= 0xFF;
        let mut f = Framer::default();
        f.push(&bad);
        assert!(f.next_frame().is_none());
    }
}
