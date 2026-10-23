use std::collections::HashSet;

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// u-blox receiver on a serial port (normal mode).
    pub receiver: Option<ReceiverConfig>,
    /// Alternative: an existing RTCM3 stream over TCP.
    pub source: Option<SourceConfig>,
    #[serde(rename = "caster")]
    pub casters: Vec<CasterConfig>,
}

/// RTCM types the F9P can generate itself.
pub const NATIVE_MESSAGES: &[u16] = &[1005, 1074, 1077, 1084, 1087, 1094, 1097, 1124, 1127, 1230];
/// RTCM types this program generates from config.
pub const GENERATED_MESSAGES: &[u16] = &[1008, 1033];

#[derive(Debug, Deserialize, Clone)]
pub struct ReceiverConfig {
    pub device: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    /// Fixed base position: WGS84 degrees and ellipsoidal height in meters.
    pub lat: f64,
    pub lon: f64,
    pub height: f64,
    /// RTKBase-style list; "(N)" is the interval in seconds, default 1.
    #[serde(default = "default_messages")]
    pub messages: String,
    #[serde(default)]
    pub antenna: String,
    #[serde(default)]
    pub receiver_type: String,
    #[serde(default)]
    pub firmware: String,
    #[serde(default = "default_reconnect_secs")]
    pub reconnect_secs: u64,
    /// Reopen and reconfigure the receiver if no RTCM arrives for this long.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(skip)]
    pub rates: Vec<(u16, u32)>,
}

impl ReceiverConfig {
    pub fn rate(&self, msg: u16) -> u32 {
        self.rates.iter().find(|(m, _)| *m == msg).map_or(0, |(_, r)| *r)
    }
}

/// Parses "1005(10),1077,1230(10)" into (type, interval) pairs.
/// Returns supported entries and the names of unsupported ones.
type ParsedMessages = (Vec<(u16, u32)>, Vec<u16>);

fn parse_messages(list: &str) -> anyhow::Result<ParsedMessages> {
    let mut rates = Vec::new();
    let mut unsupported = Vec::new();
    for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (num, interval) = match item.split_once('(') {
            Some((n, rest)) => (n, rest.strip_suffix(')').ok_or_else(|| anyhow::anyhow!("bad message entry {item:?}"))?),
            None => (item, "1"),
        };
        let msg: u16 = num.trim().parse().map_err(|_| anyhow::anyhow!("bad message type in {item:?}"))?;
        let interval: u32 = interval.trim().parse().map_err(|_| anyhow::anyhow!("bad interval in {item:?}"))?;
        if interval == 0 {
            anyhow::bail!("interval must be at least 1 in {item:?}");
        }
        if NATIVE_MESSAGES.contains(&msg) {
            if interval > 255 {
                anyhow::bail!("interval for {msg} must be at most 255");
            }
        } else if !GENERATED_MESSAGES.contains(&msg) {
            unsupported.push(msg);
            continue;
        }
        rates.push((msg, interval));
    }
    Ok((rates, unsupported))
}

#[derive(Debug, Deserialize, Clone)]
pub struct SourceConfig {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_reconnect_secs")]
    pub reconnect_secs: u64,
    /// Reconnect if no bytes arrive for this long.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// NTRIP 1.0 "SOURCE" push method (what str2str/rtkbase use).
    #[default]
    Source,
    /// NTRIP 2.0 chunked HTTP POST push method.
    Post,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CasterConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub mountpoint: String,
    pub password: String,
    /// NTRIP 2.0 (`post`) only. Defaults to the mountpoint name.
    pub username: Option<String>,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default = "default_reconnect_secs")]
    pub reconnect_secs: u64,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    /// Only forward these RTCM message types. Omit to forward everything.
    pub messages: Option<HashSet<u16>>,
    /// Wrap the NTRIP session in TLS (NTRIPS).
    #[serde(default)]
    pub tls: bool,
    /// SNI / certificate hostname. Defaults to `host`.
    pub tls_server_name: Option<String>,
    /// Skip certificate verification (lab only).
    #[serde(default)]
    pub tls_insecure: bool,
}

fn default_reconnect_secs() -> u64 {
    5
}

fn default_baud() -> u32 {
    115200
}

fn default_messages() -> String {
    "1005(10),1008(10),1033(10),1077,1087,1097,1127,1230(10)".to_string()
}

fn default_idle_timeout_secs() -> u64 {
    30
}

fn default_user_agent() -> String {
    concat!("NTRIP rustk-caster/", env!("CARGO_PKG_VERSION")).to_string()
}

pub fn load(path: &str) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading config {path}: {e}"))?;
    let mut cfg: Config = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parsing config {path}: {e}"))?;
    if cfg.casters.is_empty() {
        anyhow::bail!("config has no [[caster]] entries");
    }
    match (&mut cfg.receiver, &cfg.source) {
        (Some(_), Some(_)) | (None, None) => {
            anyhow::bail!("config needs exactly one of [receiver] or [source]")
        }
        (Some(r), None) => {
            if !(-90.0..=90.0).contains(&r.lat) || !(-180.0..=180.0).contains(&r.lon) {
                anyhow::bail!("receiver lat/lon out of range");
            }
            for (field, s) in [("antenna", &r.antenna), ("receiver_type", &r.receiver_type), ("firmware", &r.firmware)] {
                if s.len() > 31 || !s.is_ascii() {
                    anyhow::bail!("receiver {field} must be at most 31 ASCII characters");
                }
            }
            let (rates, unsupported) = parse_messages(&r.messages)?;
            if !unsupported.is_empty() {
                tracing::warn!(
                    ?unsupported,
                    "ignoring RTCM types the F9P cannot generate natively; supported: {:?} + generated {:?}",
                    NATIVE_MESSAGES,
                    GENERATED_MESSAGES
                );
            }
            if !rates.iter().any(|(m, _)| NATIVE_MESSAGES.contains(m)) {
                anyhow::bail!("receiver messages list has no observation/position messages");
            }
            r.rates = rates;
        }
        (None, Some(_)) => {}
    }
    for c in &mut cfg.casters {
        c.mountpoint = c.mountpoint.trim_start_matches('/').to_string();
        if c.mountpoint.is_empty() {
            anyhow::bail!("caster {}: mountpoint is empty", c.name);
        }
        if c.reconnect_secs == 0 {
            c.reconnect_secs = 1;
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rtkbase_message_list() {
        let (rates, unsupported) = parse_messages(
            "1004,1005(10),1006,1008(10),1012,1019,1020,1033(10),1042,1045,1046,1077,1087,1097,1107,1127,1230",
        )
        .unwrap();
        assert_eq!(
            rates,
            vec![(1005, 10), (1008, 10), (1033, 10), (1077, 1), (1087, 1), (1097, 1), (1127, 1), (1230, 1)]
        );
        assert_eq!(unsupported, vec![1004, 1006, 1012, 1019, 1020, 1042, 1045, 1046, 1107]);
    }

    #[test]
    fn rejects_bad_entries() {
        assert!(parse_messages("1005(").is_err());
        assert!(parse_messages("abc").is_err());
        assert!(parse_messages("1077(0)").is_err());
        assert!(parse_messages("1077(300)").is_err());
    }
}
