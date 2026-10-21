# rustk-caster

Turns a u-blox ZED-F9P into an RTK base station and pushes its RTCM3 to any
number of NTRIP casters. It replaces RTKBase, including its two-caster limit.

```
F9P --serial/USB--> rustk-caster --+--> caster 1
                                   +--> caster 2
                                   +--> ... caster N
```

- Talks to the receiver directly. On every connect it sends one UBX-CFG-VALSET,
  written to RAM only, so the flash config is untouched. That message sets
  fixed-base mode at your coordinates, 1 Hz measurement rate, and the RTCM
  output messages. The receiver must ACK it.
- Generates 1008/1033 (antenna and receiver descriptors) itself, with the
  station ID taken from the receiver's own 1005, so they always match.
- Validates every RTCM3 frame with CRC24Q. Casters only receive whole frames,
  and stray UBX/NMEA output from the receiver is ignored.
- Each caster has its own connection and exponential backoff. One bad caster
  never stalls the others.
- Supports NTRIP 1.0 `SOURCE` (default, same as str2str) and NTRIP 2.0 chunked `POST`.
- Self-healing. If no RTCM arrives for `idle_timeout_secs` (cable pulled,
  receiver reset wiped its RAM config, ...), it reopens and reconfigures.

## Differences from RTKBase/str2str

The F9P can only generate these RTCM types natively:
1005, 1074/1077 (GPS), 1084/1087 (GLONASS), 1094/1097 (Galileo),
1124/1127 (BeiDou) and 1230. On top of those, rustk-caster generates 1008 and 1033.

Your old list included these, which are **not available**:

- **1004/1012:** legacy GPS/GLONASS observations, superseded by MSM7.
- **1006:** the same as 1005 plus antenna height. 1005 is sent instead.
- **1019/1020/1042/1045/1046:** broadcast ephemeris. Rovers decode their own,
  so this is rarely needed.
- **1107:** QZSS MSM7.

They're logged at startup and skipped.

Nothing is written to disk. The config file is the only state, and the
systemd unit runs with a read-only filesystem. There's no web UI; use
`journalctl -u rustk-caster`.

## Install

Each release ships `.deb` packages for 64-bit (`arm64`) and 32-bit (`armhf`)
Raspberry Pi OS. They're static binaries with no dependencies, and the Pi
doesn't need Rust.

```bash
curl -fsSLo /tmp/rustk-caster.deb "https://github.com/ppicazo/rustk-caster/releases/latest/download/rustk-caster_$(dpkg --print-architecture).deb" && sudo apt install /tmp/rustk-caster.deb
```

The installer prints the next steps:

1. Write the config from your RTKBase settings: receiver port, position,
   antenna, both casters with their passwords, and the message list. Then add
   more `[[caster]]` blocks.
   ```bash
   sudo rustk-caster import-rtkbase ~/rtkbase/settings.conf /etc/rustk-caster/config.toml
   ```
   ```bash
   sudoedit /etc/rustk-caster/config.toml
   ```
2. Stop RTKBase so it releases the serial port:
   ```bash
   sudo systemctl disable --now $(systemctl list-unit-files 'str2str*' 'rtkbase*' --no-legend | awk '{print $1}')
   ```
3. Start it, and follow the logs:
   ```bash
   sudo systemctl enable --now rustk-caster
   ```
   ```bash
   journalctl -u rustk-caster -f
   ```

`/dev/ttyGNSS` is a udev symlink that RTKBase installed, and it keeps working.
`/dev/ttyACM0` (USB) or `/dev/ttyAMA0` (UART) work too.

The service runs as an unprivileged `rustk-caster` user in the `dialout`
group. The config is `root:rustk-caster` with mode 640, because it holds the
caster passwords.

## Upgrade

Run the same install command. apt keeps your config and restarts the service
if it's running.

## Uninstall

```bash
sudo apt remove rustk-caster
```
This stops and disables the service, and keeps `/etc/rustk-caster/config.toml`.

```bash
sudo apt purge rustk-caster
```
This also removes the config and the `rustk-caster` user, so nothing is left behind.

To go back to RTKBase, re-enable its units, for example
`sudo systemctl enable --now str2str_tcp rtkbase_web`. Its settings are
untouched, and so is the receiver's flash config.

## Build from source

Releases are built by [.github/workflows/release.yml](.github/workflows/release.yml)
when a `v*` tag is pushed. To build the packages locally (needs Docker):

```bash
cargo install cross cargo-deb
cross build --release --target aarch64-unknown-linux-musl
cargo deb --no-build --no-strip --target aarch64-unknown-linux-musl
```

Use `arm-unknown-linux-musleabihf` for armhf. The package lands in
`target/<target>/debian/`.

## Logs

A healthy start looks like this:
```
INFO opened receiver device=/dev/ttyGNSS baud=115200
INFO receiver configured as fixed base lat=... lon=... height=...
INFO receiving RTCM from receiver msg=1077
INFO connected and accepted by caster caster=rtk2go mount=MY_MOUNT
```

Common errors:
- `no UBX-ACK`: wrong device or baud rate, or something else (RTKBase) is
  still reading the port.
- `UBX-ACK-NAK`: the firmware is too old for one of the config keys. HPG 1.13+ is fine.

Set `RUST_LOG=debug` to also see any text the casters send back.

## Config

See [config.example.toml](config.example.toml). For each `[[caster]]` block:

| key | required | notes |
|---|---|---|
| `name` | yes | label used in logs |
| `host`, `port`, `mountpoint`, `password` | yes | |
| `protocol` | no | `source` (default) or `post` |
| `username` | no | `post` only, defaults to the mountpoint name |
| `messages` | no | e.g. `[1005, 1077, 1230]`; omit to forward everything |
| `reconnect_secs` | no | initial retry delay, default 5; doubles up to 120s |

## Related

This project pairs with [NearTRIP](https://github.com/smerty/neartrip), a
nearest-base NTRIP proxy. A public instance runs at
[rtk.k6sh.com](https://rtk.k6sh.com).

## License

[MIT](LICENSE)
