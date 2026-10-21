use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;

use crate::config::{GENERATED_MESSAGES, NATIVE_MESSAGES};

const PLACEHOLDER: &str = "CHANGE_ME";

/// `rustk-caster import-rtkbase <settings.conf> <output> [--force]`
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let force = args.iter().any(|a| a == "--force");
    let paths: Vec<&String> = args.iter().filter(|a| *a != "--force").collect();
    let [settings_path, out_path] = paths[..] else {
        anyhow::bail!("usage: rustk-caster import-rtkbase <rtkbase settings.conf> <output config.toml> [--force]");
    };

    let text = std::fs::read_to_string(settings_path)
        .map_err(|e| anyhow::anyhow!("reading {settings_path}: {e}"))?;
    let toml = convert(&parse_ini(&text))?;

    if let Ok(existing) = std::fs::read_to_string(out_path) {
        if !existing.contains(PLACEHOLDER) && !force {
            anyhow::bail!("{out_path} already looks configured; pass --force to overwrite it");
        }
    }
    // Truncate in place so an existing file keeps its owner and mode;
    // a new file is created private since it holds passwords.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(out_path)
        .and_then(|mut f| f.write_all(toml.as_bytes()))
        .map_err(|e| anyhow::anyhow!("writing {out_path}: {e}"))?;
    println!("wrote {out_path}");
    Ok(())
}

type Ini = HashMap<String, HashMap<String, String>>;

fn parse_ini(text: &str) -> Ini {
    let mut sections: Ini = HashMap::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().to_string();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = v
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .or_else(|| v.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                .unwrap_or(v);
            sections.entry(current.clone()).or_default().insert(k.trim().to_string(), v.to_string());
        }
    }
    sections
}

fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn convert(ini: &Ini) -> anyhow::Result<String> {
    let empty = HashMap::new();
    let main = ini.get("main").unwrap_or(&empty);
    let get = |k: &str| main.get(k).map(String::as_str).unwrap_or("");

    let pos: Vec<f64> = get("position").split_whitespace().filter_map(|s| s.parse().ok()).collect();
    let [lat, lon, height] = pos[..] else {
        anyhow::bail!("[main] position must be \"lat lon height\", got {:?}", get("position"));
    };
    let com_port = get("com_port");
    if com_port.is_empty() {
        anyhow::bail!("[main] com_port is missing");
    }
    let device = if com_port.starts_with('/') { com_port.to_string() } else { format!("/dev/{com_port}") };
    let baud: u32 = get("com_port_settings").split(':').next().and_then(|b| b.parse().ok()).unwrap_or(115200);

    let mut casters = Vec::new();
    for (section, suffix) in [("ntrip_A", "a"), ("ntrip_B", "b")] {
        let Some(s) = ini.get(section) else { continue };
        let f = |k: &str| s.get(&format!("{k}_{suffix}")).cloned().unwrap_or_default();
        if f("svr_addr").is_empty() || f("mnt_name").is_empty() {
            continue;
        }
        casters.push((f("svr_addr"), f("svr_port"), f("mnt_name"), f("svr_pwd"), f("rtcm_msg")));
    }
    if casters.is_empty() {
        anyhow::bail!("no NTRIP servers found in [ntrip_A]/[ntrip_B]");
    }

    let (messages, dropped) = filter_messages(&casters[0].4);

    let mut out = String::new();
    writeln!(out, "# Imported from RTKBase settings.conf.\n")?;
    writeln!(out, "[receiver]")?;
    writeln!(out, "device = {}", q(&device))?;
    writeln!(out, "baud = {baud}")?;
    writeln!(out, "lat = {lat}")?;
    writeln!(out, "lon = {lon}")?;
    writeln!(out, "height = {height}")?;
    if !dropped.is_empty() {
        writeln!(out, "# Not supported natively by the F9P, dropped: {}", dropped.join(","))?;
    }
    writeln!(out, "messages = {}", q(&messages))?;
    writeln!(out, "antenna = {}", q(get("antenna_info")))?;
    writeln!(out, "receiver_type = {}", q(get("receiver")))?;
    writeln!(out, "firmware = {}", q(get("receiver_firmware")))?;

    for (host, port, mount, pwd, _) in &casters {
        let port: u16 = port.parse().unwrap_or(2101);
        let pwd = if pwd.is_empty() { PLACEHOLDER } else { pwd };
        writeln!(out, "\n[[caster]]")?;
        writeln!(out, "name = {}", q(host))?;
        writeln!(out, "host = {}", q(host))?;
        writeln!(out, "port = {port}")?;
        writeln!(out, "mountpoint = {}", q(mount))?;
        writeln!(out, "password = {}", q(pwd))?;
    }
    writeln!(out, "\n# Add more destinations by copying a [[caster]] block.")?;
    Ok(out)
}

/// Keeps entries the F9P or this program can produce, preserving "(N)" intervals.
fn filter_messages(list: &str) -> (String, Vec<String>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let num = item.split('(').next().unwrap_or("").trim();
        match num.parse::<u16>() {
            Ok(m) if NATIVE_MESSAGES.contains(&m) || GENERATED_MESSAGES.contains(&m) => kept.push(item.to_string()),
            _ => dropped.push(item.to_string()),
        }
    }
    if kept.is_empty() {
        return ("1005(10),1008(10),1033(10),1077,1087,1097,1127,1230(10)".to_string(), dropped);
    }
    (kept.join(","), dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    const SAMPLE: &str = r#"
[main]
position='45.123456789 -120.987654321 100.25'
com_port='ttyGNSS'
com_port_settings='115200:8:n:1'
receiver='U-blox_ZED-F9P'
receiver_firmware='1.32'
antenna_info='ADVNULLANTENNA'

[local_storage]
archive_name=$(date -d "-1 days" +"%Y-%m-%d_%S").zip

[ntrip_A]
svr_addr_a='caster-a.example.com'
svr_port_a='2101'
svr_pwd_a='pa"ss\word'
mnt_name_a='MOUNT_A'
rtcm_msg_a='1004,1005(10),1006,1008(10),1012,1019,1020,1033(10),1042,1045,1046,1077,1087,1097,1107,1127,1230'

[ntrip_B]
svr_addr_b='caster-b.example.com'
svr_port_b='2101'
svr_pwd_b='secret2'
mnt_name_b='MOUNT_B'
rtcm_msg_b='1004'
"#;

    #[test]
    fn converts_rtkbase_settings_to_loadable_config() {
        let toml = convert(&parse_ini(SAMPLE)).unwrap();
        let dir = std::env::temp_dir().join(format!("rustk-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, &toml).unwrap();
        let cfg = config::load(path.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let r = cfg.receiver.unwrap();
        assert_eq!(r.device, "/dev/ttyGNSS");
        assert_eq!(r.baud, 115200);
        assert_eq!((r.lat, r.lon, r.height), (45.123456789, -120.987654321, 100.25));
        assert_eq!(r.messages, "1005(10),1008(10),1033(10),1077,1087,1097,1127,1230");
        assert_eq!(r.antenna, "ADVNULLANTENNA");
        assert_eq!(cfg.casters.len(), 2);
        assert_eq!(cfg.casters[0].mountpoint, "MOUNT_A");
        assert_eq!(cfg.casters[0].password, "pa\"ss\\word");
        assert_eq!(cfg.casters[1].host, "caster-b.example.com");
        assert!(toml.contains("dropped: 1004,1006,1012,1019,1020,1042,1045,1046,1107"));
    }
}
