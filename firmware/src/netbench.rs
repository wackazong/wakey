//! Download tests against a plain web server (feature `download-bench`,
//! before the TIDAL rounds): the link mode, one connection, the same on
//! core 1, then the file in 2 and 4 byte ranges at once, and the TCP
//! handshake time to the server idle and during the single download
//! (`bench:` lines). Nothing is written.
//!
//! Many connections faster than one: the limit is per connection (window,
//! round trip, losses). As fast: the device cannot take more in total.

use crate::http;
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::task::thread::{MallocCap, ThreadSpawnConfiguration};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const HOST: &str = "example.com";
const URL: &str = "https://example.com/video.mp4";

pub fn run() {
    let addr = match (HOST, 443).to_socket_addrs().map(|mut a| a.next()) {
        Ok(Some(a)) => a,
        other => {
            log::warn!("bench: {HOST}: no address ({other:?})");
            return;
        }
    };
    log::info!("bench: {HOST} is {addr}");
    let mut phy: esp_idf_svc::sys::wifi_phy_mode_t = 0;
    let mut bw: esp_idf_svc::sys::wifi_bandwidth_t = 0;
    // SAFETY: out-parameters, valid for the calls.
    let (e1, e2) = unsafe {
        (
            esp_idf_svc::sys::esp_wifi_sta_get_negotiated_phymode(&mut phy),
            esp_idf_svc::sys::esp_wifi_get_bandwidth(esp_idf_svc::sys::wifi_interface_t_WIFI_IF_STA, &mut bw),
        )
    };
    // 4 HT20, 5 HT40, 6 HE20; bandwidth 1 = 20 MHz, 2 = 40 MHz.
    log::info!("bench: link mode {phy} ({e1}), bandwidth {bw} ({e2})");
    let idle: Vec<u32> = (0..5).filter_map(|_| handshake_ms(addr)).collect();
    log::info!("bench: handshake idle: {}", summary(&idle));

    // One connection, with handshakes every 300 ms meanwhile.
    let stop = AtomicBool::new(false);
    let (len, loaded) = std::thread::scope(|s| {
        let probe = spawn_in(s, Core::Core0, "probe", || {
            let mut v = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Some(ms) = handshake_ms(addr) {
                    v.push(ms);
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            v
        });
        let len = timed("1 connection", || {
            http::download(URL, &mut |_| Ok(()), &mut |_, _| {}).map_err(|e| e.to_string())
        });
        stop.store(true, Ordering::Relaxed);
        (len, probe.join().unwrap_or_default())
    });
    log::info!("bench: handshake during the download: {}", summary(&loaded));
    let Some(len) = len else { return };

    // The same on core 1 (the `wifi` thread and the network stack are on
    // core 0).
    std::thread::scope(|s| {
        let h = spawn_in(s, Core::Core1, "core1", || {
            timed("1 connection on core 1", || {
                http::download(URL, &mut |_| Ok(()), &mut |_, _| {}).map_err(|e| e.to_string())
            })
        });
        let _ = h.join();
    });

    for n in [2u64, 4] {
        timed(&format!("{n} connections"), || {
            std::thread::scope(|s| {
                let parts: Vec<_> = (0..n)
                    .map(|i| {
                        let (from, to) = (len * i / n, len * (i + 1) / n - 1);
                        spawn_in(s, Core::Core0, "range", move || {
                            http::download_range(
                                URL,
                                Some(&format!("{from}-{to}")),
                                &mut |_| Ok(()),
                                &mut |_, _| {},
                            )
                            .map_err(|e| e.to_string())
                        })
                    })
                    .collect();
                let mut total = 0;
                for p in parts {
                    total += p.join().map_err(|_| "panicked".to_string())??;
                }
                Ok(total)
            })
        });
    }
}

/// Runs `f`, logs its bytes and speed; the bytes.
fn timed(what: &str, f: impl FnOnce() -> Result<u64, String>) -> Option<u64> {
    let t = Instant::now();
    let result = f();
    let ms = t.elapsed().as_millis().max(1) as u64;
    match result {
        Ok(bytes) => {
            log::info!(
                "bench: {URL}, {what}: {} KiB in {ms} ms, {} KiB/s",
                bytes / 1024,
                bytes * 1000 / 1024 / ms
            );
            Some(bytes)
        }
        Err(e) => {
            log::warn!("bench: {what}: {e}");
            None
        }
    }
}

/// Time of a TCP connect (SYN to SYN-ACK: one round trip), then closed.
fn handshake_ms(addr: SocketAddr) -> Option<u32> {
    let t = Instant::now();
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).ok()?;
    let ms = t.elapsed().as_millis() as u32;
    drop(s);
    Some(ms)
}

fn summary(v: &[u32]) -> String {
    if v.is_empty() {
        return "no samples".into();
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    format!(
        "{} samples, min {} ms, median {} ms, max {} ms",
        s.len(),
        s[0],
        s[s.len() / 2],
        s[s.len() - 1]
    )
}

/// A scoped thread at the `wifi` thread's priority (1) on `core`, its stack
/// in PSRAM (TLS needs ~16 KiB; internal RAM is short).
fn spawn_in<'s, 'e, T: Send + 's>(
    s: &'s std::thread::Scope<'s, 'e>,
    core: Core,
    name: &str,
    f: impl FnOnce() -> T + Send + 's,
) -> std::thread::ScopedJoinHandle<'s, T> {
    let previous = ThreadSpawnConfiguration::get().unwrap_or_default();
    let cfg = ThreadSpawnConfiguration {
        pin_to_core: Some(core),
        priority: 1,
        stack_alloc_caps: MallocCap::Spiram | MallocCap::Cap8bit,
        ..ThreadSpawnConfiguration::default()
    };
    if let Err(e) = cfg.set() {
        log::warn!("bench: thread config: {e}");
    }
    let handle = std::thread::Builder::new()
        .name(name.into())
        .stack_size(16 * 1024)
        .spawn_scoped(s, f)
        .expect("bench thread");
    let _ = previous.set();
    handle
}
