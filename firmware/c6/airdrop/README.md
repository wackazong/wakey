# The C6's AirDrop receiver

The image the clock boots on its ESP32-C6 while the sound page's Files tab is shown: [ESP32Drop](https://github.com/s-iwaki-d/ESP32Drop) (0BSD; an independent re-implementation of AWDL and the AirDrop receiver, written for the ESP32-S3) with ten patches, a streaming receive path that needs no PSRAM, and a link that sends each received file to the P4 as frames over IO2 (`main/link.h`). How the clock switches the C6 and stores the files: [`docs/architecture.md`](../../../docs/architecture.md#airdrop); building and installing the image: [`../README.md`](../README.md#airdrop-the-c6s-other-two-images).

**Unofficial interoperability.** AirDrop and AWDL are Apple's undocumented protocols; ESP32Drop is reverse-engineered and not affiliated with Apple. An iOS update can break it. The receiver is visible only to senders while the iPhone's AirDrop is set to receive from "Everyone" (ESP32Drop cannot do "Contacts Only").

This started as a spike (`spikes/airdrop-c6`, removed 2026-11-14; in the history up to commit 14f4b33). Its findings follow.

## Measured (2026-11-13/14, iPhone → Tab5, in the spike)

- "Wakey" appears in the iPhone's AirDrop list (and in a Mac's Finder AirDrop window).
- `Tintenfischsong.m4a`, 416,029 bytes, arrived complete seven times: twice with CRC32 `a5e16179`, then five times (three sessions, three TX rates) with `9fe9bfd1`, same size (not checked against the phone's copy: a re-saved file or a changed metadata atom would do it; each record's adler32 passed), plus iOS's AppleDouble sidecar `._Tintenfischsong.m4a` (528 bytes, to be skipped).
- A 4,020,371-byte PDF arrived complete three times with the same CRC32: in 3:29 (19.3 kB/s), then with patch 0009 in 1:43 and 2:02 (39.1, 33.1 kB/s); one AWDL session throughout, heap at the lowest 63 KB.
- **14–98 kB/s**, set mostly by the iPhone (below): the 416 KB file in 4–29 s; from tapping "Wakey" to the upload's start ~3 s more (two TLS handshakes of ~1 s, the iPhone's own steps). A 3 MB song: 30 s to 3.5 minutes.
- The tile's icon does not follow `ReceiverModelName`: "M5Stack", HomePod mini, iMac, MacBook Pro and Apple TV identifiers all gave the same tile on the iPhone. A picture appears only for a contact (an Apple-signed record), so there is no icon to set.
- C6 memory: 175 KB heap free with AWDL and the listener running; at the lowest during a transfer 111 KB (23 kB/s), 58 KB (98 kB/s, the 64 KiB window full). Image 949 KiB (app slot 1920 KiB).
- P-256 on the C6 (ECC accelerator): ephemeral key 8 ms, ECDSA signature 18 ms, shared secret 8 ms; the TLS handshake is not CPU-bound.

## Speed

The C6 has 2.4 GHz only, so AWDL runs on channel 6 alone. Every AWDL device has a channel sequence of 16 slots per 1.05 s cycle; the sender reaches us only in the slots its sequence puts on channel 6. That count decides the speed:

| Raw TX rate | Window | ch6 slots per cycle | Speed |
| --- | --- | --- | --- |
| 11b 1 Mbit/s | 5,760 B | 1 | 14 kB/s |
| 11b 1 Mbit/s | 32 KiB | 1 | 31 kB/s |
| 11b 1 Mbit/s | 64 KiB | 1 (3 at the end) | 19 kB/s |
| 11g 24 Mbit/s | 64 KiB | 3 | **98 kB/s** |
| 11g 24 Mbit/s | 64 KiB | 1 | 23 kB/s, 14 kB/s |
| 11g 54 Mbit/s | 64 KiB | 1 | 23 kB/s |
| 11g 24 Mbit/s | 64 KiB | 1, another device master | 19 kB/s (4 MB) |
| 11g 24 + patch 0009 | 64 KiB | 1–2, another device master | **39, 33 kB/s** (4 MB) |
| as above, announcing dualband (ESP32Drop's default) | 64 KiB | 3 (the iPhone's) | 79, 98 kB/s |
| as above, announcing 2.4 GHz only | 64 KiB | 3 | **123, 136 kB/s** |
| 2.4 GHz only + short GI (HT20) | 64 KiB | 3 | 129, 128 kB/s |

- The slots are the iPhone's choice (and the master's, which the C6 follows when it transmits). In one session the iPhone was the AWDL master and the sender; in the 4 MB one another Apple device in range was master, with one or two channel-6 slots. Idle, every device had one; once the iPhone gave three for a whole transfer, for no reason visible on our side.
- The raw TX rate (our TCP ACKs, AWDL's action frames) is now 11g 24 Mbit/s instead of 1 Mbit/s: an ACK then takes ~50 µs instead of ~1 ms of a slot. 24 and 54 Mbit/s measured the same. HT20 (MCS4) wedged the C6's transmitter ("(tx)rts error … 0xab" on every frame until a reboot); an iPhone waiting for the close then reports the transfer as declined.
- The 64 KiB TCP window (`sdkconfig.defaults`) only matters with more than one slot: with three, 98 kB/s is above one 32 KiB window per slot (94 kB/s).
- ESP32Drop sends only in the master's channel-6 slots. When another device is master, the sender's own schedule can put it on channel 6 at other times too, and its frames arrive then while our ACKs wait. Patch 0009: for 8 ms after a unicast frame from a peer, frames queued for that peer (the ACKs) leave at once; everything else still waits for the window. 1,762 and 3,578 frames went out that way in the two 4 MB transfers, twice as fast. ESP32Drop's own A/B of sending outside the window (to a peer not heard) had made things worse; this sends only to a peer just heard.
- ESP32Drop's data_path_state TLV announces dualband support (flag `0x0020` in `0x8f24`). Announcing 2.4 GHz only (`0x8f04`) made the same iPhone schedule (3 of 16 slots on channel 6 in all six runs, alternating A/B/C on 2026-11-14) deliver 123–136 instead of 79–98 kB/s; the schedule it announced did not change, so presumably it uses its channel-6 slots differently for a peer it cannot move to 5 GHz (inference). Short guard interval (`ht_capabilities` `0x11ee`) was received fine but gained nothing measurable, so it is not announced. Patch 0010 makes both settable; `main.cpp` sets `0x8f04`. The idea came from owly's PR #81 (https://github.com/t4t5/owly/pull/81).
- The next lever, per-slot efficiency: an iPhone delivers 140–360 kB/s on channel 6 to a PC's Intel card (https://github.com/t4t5/omdrop-owl/pull/64), which accepts its Block Ack session and aggregated frames; the C6 ACKs frame by frame. Block Ack for an unassociated peer is not in ESP-IDF's driver (Wayline's attempt for the S3/C5 hooks undocumented `libpp` internals: https://github.com/ZyphrZero/Wayline). Not tried.
- Untested: whether the iPhone gives channel 6 more slots when it is not on a 5 GHz Wi-Fi network (it must also serve that network's channel).

## What it took on the C6

ESP32Drop builds for the C6 with ESP-IDF 5.5.5 nearly as is (it uses only `millis()` from Arduino; the C6's ROM has the same miniz). What did not work, in the order found:

1. **No console on IO2.** The bootloader routes the console's pins; the app only sets the baud rate. The C6 keeps esp_hosted's bootloader (default pins, the J1 header). The receiver has no console now: `main/link.c` drives UART1 on IO2 itself and sends the log as frames.
2. **Single core.** ESP32Drop pins its tasks to core 1, which asserts on the C6 (patch 0003).
3. **No PSRAM.** ESP32Drop holds the whole archive (and starts with a 320 KiB receive buffer). The streaming decoder (`components/esp32drop/stream`) holds the 32 KiB inflate window and a 4 KiB read buffer, whatever the file size (patches 0001, 0002).
4. **The sender's frames were not ACKed.** On the soft-AP interface the C6's MAC does not ACK AWDL's unicast data frames (no ToDS/FromDS, the shared AWDL BSSID); the S3's does. 85 % of the frames for us arrived as 802.11 retries and every TLS handshake was reset by the sender. AWDL now runs on the unassociated station interface, where the C6 ACKs them (patch 0005). The receive BSSID check (`hal_rx_disable_bssid_check`) was off already; not the cause.
5. **Transmission stopped after 16 frames.** As a station, the C6's driver transmits raw multicast data frames (the S3's refuses them) and reports their completion; ESP32Drop's TX-done handler returned early for multicast before releasing the frame's in-flight slot. After 16 such frames (IPv6 neighbour discovery) nothing was sent again. Fixed, plus a reaper that releases a slot whose completion has not come after two AWDL cycles (patch 0006; it has not fired since).
6. **Slow, and 3 s at the end.** lwIP's default TCP receive window (5,760 bytes) let the sender send four segments per channel-6 slot: 14 kB/s; 32 KiB: 31 kB/s; now 64 KiB (`sdkconfig.defaults`). iOS sends neither an end record nor the HTTP last chunk and waits for the answer, so ESP32Drop waited 3 s of silence; the upload now ends when the archive's trailer arrives at a record boundary (patch 0007).

## Design

```
iPhone ──AWDL ch 6──▶ C6: ESP32Drop (AWDL, IPv6, mDNS, TLS, HTTP /Discover /Ask /Upload)
                          └─ /Upload body ─▶ ad_stream: DvZip records ─▶ tinfl ─▶ cpio ─▶ sink
                                                                                     │
P4: wakey_core::airdrop ◀── frames, UART 3 Mbaud, IO2 → GPIO14 (main/link.c) ◀───────┘
```

- `components/esp32drop/stream/ad_stream.{h,c}`: plain C, no platform calls. DvZip records (4-byte header, bit 31 = stored) → per-record zlib through the ROM's `tinfl_decompress` with a wrapping 32 KiB window → an incremental cpio (odc, newc) parser → `begin(name, size)` / `data` / `end(complete)` per regular file. Every length from the sender is bounded (records ≤ 1 MiB, names ≤ 255 bytes). A file's `end(true)` means its bytes were delivered, not verified (a record's adler32 comes at its end): files stay provisional until the transfer's verdict, which the P4 waits for before it renames the part files.
- With `AD_RX_STREAM`, `/Upload` goes to `rx_upload_stream()`: the chunked body is read in 4 KiB pieces into the decoder; it ends on an end record, the last chunk, the archive trailer at a record boundary, or a record boundary + 3 s silence; the status is decided before the response (200 only for a whole, intact archive; 400 broken; 507 the sink refused), then `done(ok, why)`. API: `ad_on_stream(sink, done)`, callbacks on ESP32Drop's TLS task, which a full UART buffer blocks (back-pressure through TCP).
- `main/main.cpp`: points the boot slot back at esp_hosted first (if the other slot holds `network_adapter`), starts AWDL and the listener as "Wakey" (raw TX 11g 24 Mbit/s, "2.4 GHz only"), sends each file and a status once a second.

## Patches to ESP32Drop

Applied by `fetch.sh` in order; 0001–0003, 0005–0007 and 0010 keep ESP32Drop's behaviour on the S3 by default.

| Patch | What |
| --- | --- |
| `0001-bulk-heap-configurable` | PSRAM allocations and the 384 KiB reserve as `AD_BULK_CAPS`, `AD_RX_HEADROOM` |
| `0002-streaming-receive` | `AD_RX_STREAM`: `rx_upload_stream()`, `ad_on_stream()` |
| `0003-single-core` | tasks on core 0 when there is only one |
| `0004-spike-diagnostics` | counters for the spike's log (unused by `main.cpp` now): transmitted IPv6 by kind and TCP flags, unicast frames for us and how many as retries, the raw TX rate |
| `0005-station-interface` | `AWDL_WIFI_IF` (default the soft-AP; the C6 build: the station) |
| `0006-release-multicast-tx-slots` | multicast completions release their in-flight slot; the reaper |
| `0007-stream-ends-on-trailer` | the upload ends on the archive trailer |
| `0008-rx-format-and-peers` | more counters (unused by `main.cpp` now): frames for us by PHY format, each peer's channel-6 slots, the master and the sender |
| `0009-ack-to-a-sender-just-heard` | frames for a peer heard in the last 8 ms leave outside the master's window (`AWDL_FOLLOW_US`) |
| `0010-settable-capabilities` | `awdl_set_caps()`: the data_path_state flags and HT capabilities our frames announce |

## Build and test

```sh
./fetch.sh                 # ESP32Drop and miniz at pinned commits into vendor/, patches applied
./test/run.sh              # host test of the streaming decoder (ASan, UBSan)
. ./env.sh                 # the firmware's ESP-IDF (firmware/.embuild, from its first build)
python "$IDF_PATH/tools/idf.py" set-target esp32c6   # once
python "$IDF_PATH/tools/idf.py" build                # build/wakey_airdrop_c6.bin
```

After a change to `sdkconfig.defaults`, delete `sdkconfig` (an existing one wins). Then copy the image to `../wakey_airdrop_c6-<version>.bin` (`PROJECT_VER` in `CMakeLists.txt`), and update `../pack.py`.

## Recovery

The receiver's first action is to make esp_hosted the next boot, so a crash after that point is harmless. An image that crashes before that switch has finished keeps booting itself: seen 2026-11-13, when a bigger `app_main` frame overflowed the main task's 3,584-byte stack inside `esp_ota_set_boot_partition()` (it verifies the esp_hosted image), hundreds of boots in a row. Since then the main task has 8 KiB and `boot_hosted_next()` is not inlined into `app_main`.

The way back without a USB-UART adapter is [`../rescue`](../rescue/main/rescue.c), a P4 firmware (C, ESP-IDF, Espressif's esp-serial-flasher): it powers the C6, resets it (GPIO15) into its ROM loader and talks to that over SDIO, reads the C6's partition table, writes `../esp_hosted_slave-2.13.1-esp32c6.bin` into the slot the bootloader starts with blank otadata (factory, else `ota_0`, MD5-verified), and erases otadata. The P4 cannot drive the C6's boot strap IO9: bridge the pads G9 and GND next to the C6 module (header J1 "C6_ISP": 3V3, TXD, RXD, RST, G9, GND; under the back cover) while the tool resets the C6 (every 3 s, until "connected"). Used on 2026-11-13: connected, 1.2 MB written in 5 s, and the clock's next boot reported "C6 firmware 2.13.1" (the clock then installs its own esp_hosted and the receiver again).

```sh
cd ../rescue && ./fetch.sh && . ../airdrop/env.sh
python "$IDF_PATH/tools/idf.py" set-target esp32p4   # once
python "$IDF_PATH/tools/idf.py" build
cd build && python -m esptool --chip esp32p4 -p <port> write_flash "@flash_args"
```

It uses the clock's `partitions.csv`, so the clock's NVS and the `c6` partition stay. Afterwards flash the clock firmware again.

## Open

- Several files at once, a Mac as sender, the iPhone off its 5 GHz network (more channel-6 slots?).
- Block Ack for the iPhone's aggregated frames (Speed, above).
