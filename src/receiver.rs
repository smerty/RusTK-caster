use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::config::{ReceiverConfig, NATIVE_MESSAGES};
use crate::rtcm::{self, Frame, Framer};
use crate::ubx;

// u-blox F9 HPG 1.32 configuration keys.
const CFG_RATE_MEAS: u32 = 0x3021_0001;
const CFG_RATE_NAV: u32 = 0x3021_0002;
const CFG_TMODE_MODE: u32 = 0x2003_0001;
const CFG_TMODE_POS_TYPE: u32 = 0x2003_0002;
const CFG_TMODE_LAT: u32 = 0x4003_0009;
const CFG_TMODE_LON: u32 = 0x4003_000A;
const CFG_TMODE_HEIGHT: u32 = 0x4003_000B;
const CFG_TMODE_LAT_HP: u32 = 0x2003_000C;
const CFG_TMODE_LON_HP: u32 = 0x2003_000D;
const CFG_TMODE_HEIGHT_HP: u32 = 0x2003_000E;
const CFG_UART1OUTPROT_RTCM3X: u32 = 0x1074_0004;
const CFG_USBOUTPROT_RTCM3X: u32 = 0x1078_0004;
pub const NO_STATION: u32 = u32::MAX;
const TMODE_FIXED: i64 = 2;
const POS_TYPE_LLH: i64 = 1;

// CFG-MSGOUT-RTCM_3X_TYPExxxx keys are laid out as I2C, UART1, UART2, USB, SPI.
const MSGOUT_UART1_OFFSET: u32 = 1;
const MSGOUT_USB_OFFSET: u32 = 3;

fn msgout_i2c_key(msg: u16) -> u32 {
    match msg {
        1005 => 0x2091_02BD,
        1074 => 0x2091_035D,
        1077 => 0x2091_02CC,
        1084 => 0x2091_0362,
        1087 => 0x2091_02D1,
        1094 => 0x2091_0367,
        1097 => 0x2091_0318,
        1124 => 0x2091_036C,
        1127 => 0x2091_02D6,
        1230 => 0x2091_0303,
        _ => unreachable!("not a native message: {msg}"),
    }
}

/// Splits degrees into the 1e-7 deg main value and 1e-9 deg high-precision remainder.
fn split_hp(value: f64, main_scale: f64, hp_steps: i64) -> (i64, i64) {
    let fine = (value * main_scale * hp_steps as f64).round() as i64;
    (fine / hp_steps, fine % hp_steps)
}

pub fn base_config(cfg: &ReceiverConfig) -> Vec<(u32, i64)> {
    let (lat, lat_hp) = split_hp(cfg.lat, 1e7, 100);
    let (lon, lon_hp) = split_hp(cfg.lon, 1e7, 100);
    // HEIGHT is in cm, HEIGHT_HP in 0.1 mm.
    let (height, height_hp) = split_hp(cfg.height, 100.0, 100);

    let mut items = vec![
        (CFG_RATE_MEAS, 1000),
        (CFG_RATE_NAV, 1),
        (CFG_TMODE_MODE, TMODE_FIXED),
        (CFG_TMODE_POS_TYPE, POS_TYPE_LLH),
        (CFG_TMODE_LAT, lat),
        (CFG_TMODE_LON, lon),
        (CFG_TMODE_HEIGHT, height),
        (CFG_TMODE_LAT_HP, lat_hp),
        (CFG_TMODE_LON_HP, lon_hp),
        (CFG_TMODE_HEIGHT_HP, height_hp),
        (CFG_UART1OUTPROT_RTCM3X, 1),
        (CFG_USBOUTPROT_RTCM3X, 1),
    ];
    // Explicitly set every native message so stale outputs get turned off.
    // Enabled on both UART1 and USB since the F9P may be wired either way.
    for &msg in NATIVE_MESSAGES {
        let rate = cfg.rate(msg) as i64;
        let base = msgout_i2c_key(msg);
        items.push((base + MSGOUT_UART1_OFFSET, rate));
        items.push((base + MSGOUT_USB_OFFSET, rate));
    }
    assert!(items.len() <= ubx::MAX_KEYS_PER_VALSET);
    items
}

/// Blocking loop: open serial port, configure the receiver, broadcast RTCM frames.
/// Never returns; reopens and reconfigures on any failure.
pub fn run(cfg: ReceiverConfig, tx: broadcast::Sender<Arc<Frame>>, station: Arc<AtomicU32>) {
    let config_items = base_config(&cfg);
    loop {
        match serialport::new(&cfg.device, cfg.baud).timeout(Duration::from_millis(500)).open() {
            Ok(mut port) => {
                info!(device = %cfg.device, baud = cfg.baud, "opened receiver");
                match ubx::send_valset(&mut *port, &config_items) {
                    Ok(()) => {
                        info!(lat = cfg.lat, lon = cfg.lon, height = cfg.height, "receiver configured as fixed base");
                        stream(&cfg, &mut *port, &tx, &station);
                    }
                    Err(e) => error!(device = %cfg.device, error = %e, "configuring receiver failed"),
                }
            }
            Err(e) => error!(device = %cfg.device, error = %e, "failed to open receiver"),
        }
        std::thread::sleep(Duration::from_secs(cfg.reconnect_secs));
    }
}

fn stream(cfg: &ReceiverConfig, port: &mut dyn Read, tx: &broadcast::Sender<Arc<Frame>>, station: &AtomicU32) {
    let idle = Duration::from_secs(cfg.idle_timeout_secs.max(1));
    let mut framer = Framer::default();
    let mut buf = [0u8; 4096];
    let mut last_frame = Instant::now();
    let mut first = true;

    loop {
        match port.read(&mut buf) {
            Ok(0) => {
                warn!(device = %cfg.device, "receiver port closed");
                return;
            }
            Ok(n) => framer.push(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::TimedOut => {}
            Err(e) => {
                error!(device = %cfg.device, error = %e, "error reading receiver");
                return;
            }
        }

        while let Some(frame) = framer.next_frame() {
            if first {
                info!(msg = frame.msg_type, "receiving RTCM from receiver");
                first = false;
            }
            if let Some(id) = frame.station_id() {
                station.store(id as u32, Ordering::Relaxed);
            }
            last_frame = Instant::now();
            // Err only means no caster is subscribed right now.
            let _ = tx.send(Arc::new(frame));
        }

        if last_frame.elapsed() > idle {
            // Also covers a receiver reset that wiped the RAM-layer config.
            warn!(secs = idle.as_secs(), "no RTCM from receiver, reopening and reconfiguring");
            return;
        }
    }
}

/// Periodically emits 1008/1033 built from config, tagged with the receiver's station ID.
pub async fn generate(cfg: ReceiverConfig, tx: broadcast::Sender<Arc<Frame>>, station: Arc<AtomicU32>) {
    let mut tasks = tokio::task::JoinSet::new();
    for msg in [1008u16, 1033] {
        let interval = cfg.rate(msg);
        if interval == 0 {
            continue;
        }
        let (cfg, tx, station) = (cfg.clone(), tx.clone(), station.clone());
        tasks.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(interval as u64));
            loop {
                tick.tick().await;
                // Hold off until the receiver's station ID is known so it matches 1005.
                let Ok(id) = u16::try_from(station.load(Ordering::Relaxed)) else {
                    continue;
                };
                let frame = match msg {
                    1008 => rtcm::msg_1008(id, &cfg.antenna),
                    _ => rtcm::msg_1033(id, &cfg.antenna, &cfg.receiver_type, &cfg.firmware),
                };
                let _ = tx.send(Arc::new(frame));
            }
        });
    }
    while tasks.join_next().await.is_some() {}
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_precision_split() {
        assert_eq!(split_hp(45.123456789, 1e7, 100), (451234567, 89));
        assert_eq!(split_hp(-120.987654321, 1e7, 100), (-1209876543, -21));
        // 100.0123 m -> 10001 cm + 23 x 0.1 mm
        assert_eq!(split_hp(100.0123, 100.0, 100), (10001, 23));
    }

    #[test]
    fn known_msgout_keys() {
        // Spot-check against the u-blox interface description.
        assert_eq!(msgout_i2c_key(1005) + MSGOUT_UART1_OFFSET, 0x2091_02BE);
        assert_eq!(msgout_i2c_key(1077) + MSGOUT_UART1_OFFSET, 0x2091_02CD);
        assert_eq!(msgout_i2c_key(1230) + MSGOUT_USB_OFFSET, 0x2091_0306);
    }
}
