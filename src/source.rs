use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::config::SourceConfig;
use crate::rtcm::{Frame, Framer};

const NOT_RTCM_THRESHOLD: u64 = 16 * 1024;

/// Reads RTCM3 from a local TCP stream and broadcasts each validated frame.
pub async fn run(cfg: SourceConfig, tx: broadcast::Sender<Arc<Frame>>) {
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let idle = Duration::from_secs(cfg.idle_timeout_secs.max(1));
    let mut buf = vec![0u8; 4096];

    loop {
        match TcpStream::connect(&addr).await {
            Ok(mut stream) => {
                info!(%addr, "connected to RTCM source");
                let mut framer = Framer::default();
                let mut frames: u64 = 0;
                let mut warned_not_rtcm = false;

                loop {
                    let n = match tokio::time::timeout(idle, stream.read(&mut buf)).await {
                        Err(_) => {
                            warn!(%addr, secs = idle.as_secs(), "no data from source, reconnecting");
                            break;
                        }
                        Ok(Ok(0)) => {
                            warn!(%addr, "source closed connection");
                            break;
                        }
                        Ok(Ok(n)) => n,
                        Ok(Err(e)) => {
                            error!(%addr, error = %e, "error reading from source");
                            break;
                        }
                    };

                    framer.push(&buf[..n]);
                    while let Some(frame) = framer.next_frame() {
                        frames += 1;
                        // Err only means no caster is subscribed right now.
                        let _ = tx.send(Arc::new(frame));
                    }

                    if frames == 0 && !warned_not_rtcm && framer.skipped_bytes > NOT_RTCM_THRESHOLD {
                        warned_not_rtcm = true;
                        error!(
                            %addr,
                            skipped = framer.skipped_bytes,
                            "no valid RTCM3 frames from source. Is it UBX? Point [source] at an RTCM3 \
                             stream (e.g. RTKBase's RTCM server on port 5016), not the raw receiver port"
                        );
                    }
                }
            }
            Err(e) => {
                error!(%addr, error = %e, "failed to connect to source");
            }
        }
        tokio::time::sleep(Duration::from_secs(cfg.reconnect_secs)).await;
    }
}
