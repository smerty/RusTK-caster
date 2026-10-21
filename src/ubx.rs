use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

const CLASS_CFG: u8 = 0x06;
const ID_VALSET: u8 = 0x8A;
const LAYER_RAM: u8 = 0x01;
const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const ATTEMPTS: usize = 3;
pub const MAX_KEYS_PER_VALSET: usize = 64;

pub fn frame(class: u8, id: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u16;
    let mut f = vec![0xB5, 0x62, class, id];
    f.extend_from_slice(&len.to_le_bytes());
    f.extend_from_slice(payload);
    let (mut a, mut b) = (0u8, 0u8);
    for &x in &f[2..] {
        a = a.wrapping_add(x);
        b = b.wrapping_add(a);
    }
    f.extend_from_slice(&[a, b]);
    f
}

/// Builds a UBX-CFG-VALSET writing `items` to the RAM layer only, so the
/// receiver's saved (flash/BBR) configuration is left untouched.
pub fn valset_ram(items: &[(u32, i64)]) -> Vec<u8> {
    let mut p = vec![0x00, LAYER_RAM, 0x00, 0x00];
    for &(key, val) in items {
        let size = match (key >> 28) & 0x7 {
            1 | 2 => 1,
            3 => 2,
            4 => 4,
            5 => 8,
            s => panic!("config key {key:#010x} has invalid size code {s}"),
        };
        p.extend_from_slice(&key.to_le_bytes());
        p.extend_from_slice(&val.to_le_bytes()[..size]);
    }
    frame(CLASS_CFG, ID_VALSET, &p)
}

/// Sends a VALSET and waits for the matching UBX-ACK-ACK, retrying on silence.
pub fn send_valset<P: Read + Write + ?Sized>(port: &mut P, items: &[(u32, i64)]) -> anyhow::Result<()> {
    let msg = valset_ram(items);
    let ack = frame(0x05, 0x01, &[CLASS_CFG, ID_VALSET]);
    let nak = frame(0x05, 0x00, &[CLASS_CFG, ID_VALSET]);
    let mut buf = [0u8; 1024];

    for _ in 0..ATTEMPTS {
        port.write_all(&msg)?;
        port.flush()?;

        let mut seen = Vec::new();
        let deadline = Instant::now() + ACK_TIMEOUT;
        while Instant::now() < deadline {
            match port.read(&mut buf) {
                Ok(n) => seen.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => return Err(e.into()),
            }
            if contains(&seen, &ack) {
                return Ok(());
            }
            if contains(&seen, &nak) {
                anyhow::bail!("receiver rejected configuration (UBX-ACK-NAK); unsupported key or firmware?");
            }
        }
    }
    anyhow::bail!("no UBX-ACK from receiver after {ATTEMPTS} attempts; check device path and baud rate")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_checksum() {
        // UBX-ACK-ACK for CFG-PRT, a well-known byte sequence.
        assert_eq!(frame(0x05, 0x01, &[0x06, 0x00]), [0xB5, 0x62, 0x05, 0x01, 0x02, 0x00, 0x06, 0x00, 0x0E, 0x37]);
    }

    #[test]
    fn valset_encodes_sizes() {
        let msg = valset_ram(&[(0x20030001, 2), (0x40030009, -1), (0x30210001, 1000)]);
        let payload = &msg[6..msg.len() - 2];
        assert_eq!(&payload[..4], &[0, 1, 0, 0]);
        assert_eq!(&payload[4..9], &[0x01, 0x00, 0x03, 0x20, 2]);
        assert_eq!(&payload[9..17], &[0x09, 0x00, 0x03, 0x40, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(&payload[17..23], &[0x01, 0x00, 0x21, 0x30, 0xE8, 0x03]);
    }
}
