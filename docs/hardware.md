# Hardware reference

M5Stack Tab5 Kit (ESP32-P4 + ESP32-C6, 5" 720×1280 touch panel, NP-F550 battery), plus a Creative Pebble V3 USB speaker. From M5Stack's [schematic](https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1132/Tab5_Schematics_PDF.pdf), M5Stack's M5Unified/M5GFX sources and measurements on the development unit.

## The development unit

| | |
| --- | --- |
| ESP32-P4 | rev **v1.3** (`espflash board-info`), 360 MHz, 32 MB PSRAM (hex, 200 MHz), 16 MB flash |
| Panel | **ST7123** TDDI (display and touch in one chip), touch firmware 3 |
| USB | USB-C: P4's USB Serial/JTAG (flashing, console), `/dev/cu.usbmodem2101` on the development Mac |
| C6 firmware | factory: M5Stack's esp_hosted slave 1.4.1, whose startup event carries no version and shows as 0.0.0. The firmware replaces it after the first Wi-Fi session: esp_hosted slave 2.13.1 with Wakey's addition ("2.13.1-wakey2") in one app slot, the AirDrop receiver in the other (see [`firmware/c6/`](../firmware/c6/README.md)). Recovery: header J1 "C6_ISP" (3V3, TXD, RXD, RST, IO9, GND), IO9 low during reset for download mode |
| Battery | not connected |

Three panel variants exist across production runs; the firmware detects them from the touch controller: ST712x at 0x55 with firmware 3 → ST7123, 1 → ST7121; GT911 (0x14/0x5D) → ILI9881C. Only the ST712x touch is implemented.

## ESP32-P4 pins

| Function | GPIO | Note |
| --- | --- | --- |
| System I2C SDA / SCL | 31 / 32 | 400 kHz; expanders, RTC, INA226, touch, codec, camera (SCCB), IMU |
| Touch interrupt `TP_INT` | 23 | active low, 5.1 kΩ pull-up (R92); not an LP-GPIO (no deep-sleep wake). Not used (touch is polled) |
| Backlight `LCD_BL` | 22 | enable of the ME2212 boost; LEDC PWM 4 kHz, 14 bit (finer steps at the dark end) |
| MIPI-DSI | dedicated | 2 lanes; PHY supply `VDD_MIPI_DPHY` = on-chip LDO 3 at 2.5 V |
| microSD (SDMMC slot 0) CLK / CMD / D0–D3 | 43 / 44 / 39–42 | IO_MUX pins, 4 bit; pin supply = on-chip LDO 4 at 3.3 V |
| ESP32-C6 (SDMMC slot 1) CLK / CMD / D0–D3 | 12 / 13 / 11, 10, 9, 8 | GPIO matrix, 4 bit, 40 MHz |
| C6 reset `SOC_EXTRF_RST` | 15 | → 1 kΩ → C6 EN (10 kΩ pull-up): low = reset |
| C6 IO2 `RF_C6_IO2` | 14 | UART1 RX at 3 Mbaud: the AirDrop receiver's frames from the C6 |
| USB 2.0 HS OTG | dedicated | → USB-A socket: host port for the speaker |
| USB Serial/JTAG | 24 / 25 | → USB-C socket |
| I2S MCLK / SCLK / LRCK / DOUT / DIN | 30 / 27 / 29 / 26 / 28 | ES8388 codec (I2S0 TX: built-in speaker) and ES7210 mics (unused) |
| UART0 TX / RX | 37 / 38 | also via the CH9102F on USB-C |
| Strapping | 34–38 | `BOOT_GPIO35` |
| RS-485 TX / RX / DIR | 20 / 21 / 34 | unused |
| M5-Bus, 2×5 header | 0–7, 16–19, 45–54 | unused |

LP-GPIOs 0–15 are all taken (bus/header, SDIO to the C6, C6 reset/IO2).

## I2C devices (system bus)

| Device | Address | Use in the firmware |
| --- | --- | --- |
| PI4IOE5V6408 expander A | 0x43 | resets, speaker amp, EXT5V |
| PI4IOE5V6408 expander B | 0x44 | Wi-Fi power, USB-A 5 V, power-off, charging |
| RX8130CE RTC | 0x32 | UTC time; alarm registers not used |
| ST7123 touch | 0x55 | polled every 10 ms (16-bit registers) |
| INA226 | 0x41 | battery current (5 mΩ shunt): more than 30 mA means the clock runs on battery (display sleep) |
| SC202CS camera (SC2356) | 0x36 | chip ID 0xEB52 at 0x3107; MIPI-CSI one lane, 24 MHz from oscillator X2; used as light meter |
| ES8388 | 0x10 | built-in speaker codec (DAC only, values from M5Unified) |
| ES7210, BMI270 | – | unused |

## IO expander pins

Registers: 0x03 direction (1 = out), 0x05 output, 0x07 high-Z, 0x0B pull enable, 0x0D pull select (1 = up), 0x0F input. Start values: M5Unified's `Power_Class.inl`, except that LCD and touch start in reset and Wi-Fi power off ([`board/ioexp.rs`](../firmware/src/board/ioexp.rs)).

| Pin | Signal | Dir | Meaning |
| --- | --- | --- | --- |
| A.P0 | `RF_PTH_L_INT_H_EXT` | out | antenna: 0 = internal |
| A.P1 | `SPK_EN` | out | NS4150B amplifier (internal speaker) |
| A.P2 | `EXT5V_EN` | out | 5 V on M5-Bus/Grove – keep off |
| A.P4 | `LCD_RST` | out → in | LCD reset; released as input with pull-up |
| A.P5 | `TP_RST` | out | touch reset |
| A.P6 | `CAM_RST` | out | camera reset |
| A.P7 | `HP_DET` | in | headphones plugged |
| B.P0 | `WLAN_PWR_EN` | out | 3.3 V LDO for the C6 (U38) |
| B.P3 | `USB5V_EN` | out | 5 V on USB-A (U27 MT9700, ≈ 1.33 A limit) |
| B.P4 | `PWROFF_PULSE` | out | power-off: toggle 10× at 50 ms (M5Unified) |
| B.P5 | `nCHG_QC_EN` | out | fast charge disable, leave default |
| B.P6 | `CHG_STAT` | in | charging (active low) |
| B.P7 | `CHG_EN` | out | allow charging |

## RX8130CE RTC

Registers ([`board/rtc.rs`](../firmware/src/board/rtc.rs), data sheet, M5Unified `RX8130_Class`): 0x10–0x16 time (BCD; weekday as a bit mask, bit 0 = Sunday; year 2000+), 0x17–0x19 alarm (bit 7 = ignore field), 0x1C extension (TE, WADA), 0x1D flags (VLF power lost, AF alarm), 0x1E control 0 (STOP, AIE), 0x1F control 1 (CHGEN supercap charging, INIEN nIRQ output), 0x30 digital offset. Backup: 70 mF supercap, ≈ 7 h. Its `nIRQ` goes to the PMS150 power controller, **not to the P4**: the RTC alarm can switch the board on from power-off, it cannot interrupt the running P4.

## Power

- USB-C path: `USB_5VIN` (FU1 2 A) → U34 MAX40200 (**1 A**) → `SYS_5VBUS` → U23 LPW5209 (≈ 2.4 A) → `SYS_INT5V` → U27 MT9700 (≈ 1.33 A) → USB-A. All 5 V outputs sit behind U34. Budget for the Pebble ≈ 0.5 A (gain switch "L", limited USB volume; not measured, see below).
- Battery path locked out in hardware when external power is present.
- Power-off: `PWROFF_PULSE` → PMS150G (U28) → `MPWR_EN` = 0 → main switch and battery buck off. `VDD_STBY` keeps PMS150, RTC and IMU alive.
- Power-on sources: power button, RTC `nIRQ`, IMU INT1 (all via the PMS150).
- Not switchable: `SOC_3.3V` (SD card, camera LDOs, audio LDO), `VDD_STBY`, and the CH9102F on `USB_5VIN` (1–3 mA, only when powered via USB-C).

| Wake source | Path | Light sleep | Deep sleep | Power-off |
| --- | --- | --- | --- | --- |
| Touch | `TP_INT` → GPIO23 | yes | no (not an LP-GPIO; a wire to GPIO1 would fix that) | no |
| Timer | internal | yes | yes | no |
| RTC alarm | `nIRQ` → PMS150 | no | no | yes |
| IMU tap | INT1 → PMS150 | no | no | yes, if configured |
| Power button | PMS150 | – | – | yes |

### Power states

Two states; the second is a display sleep, not chip light sleep (see below):

| State | Display | Wi-Fi | USB-A 5 V | Wakes on |
| --- | --- | --- | --- | --- |
| On | on, brightness from the room's light | only for a session (clock sync, TIDAL) or AirDrop | only on the settings pages and from 60 s before something rings | – |
| Display sleep | backlight off, panel at 33 Hz | as above | as above | any touch, alarm, daylight |

Display sleep starts after `power.sleep_after_s` (60 s) without a touch on the clock page, when `power.sleep_when` allows it: `"dark"` (default) in a dark room (the light meter's night mode) or on battery; `"always"`; `"never"`. It ends on a touch (which only wakes), when an alarm rings, and when the condition ends (daylight on USB power).

In display sleep: backlight off, nothing drawn, light meter running ([`app/power.rs`](../firmware/src/app/power.rs), `ui::power`); the panel streams at 33 Hz instead of 66 Hz (the display link is rebuilt for it, 0.45 s each way); USB-A is off except on the settings pages and from 60 s before something rings (the Pebble is ready ~1 s after USB-A comes on, well within that lead); the built-in speaker's codec and I2S are off while silent. The panel is not put into sleep mode: the ST7123's touch stops scanning then. Measured at the USB-C input (5 V, Pebble switched on): screen on 237–242 mA; backlight off alone 237 mA; USB-A off −80 mA and 33 Hz −21 mA: ~138 mA; with the codec and I2S off 132 mA asleep (dipping to ~99 mA now and then).

**Not chip light sleep:** a single tap must wake the clock, and from light sleep it cannot: the ST7123 senses touches only while the P4 streams video, which light sleep stops; in its "Smart Wake Up" mode it keeps sensing but reports only gestures (double tap, long press), never a single tap. A double-tap wake from light sleep works (with `gpio_sleep_sel_dis(23)` and the whole display link rebuilt after waking) and would save an estimated 0.1–0.2 W more (details in [pitfalls.md](pitfalls.md#touch-and-i2c)).

### Open power measurements

- Pebble current at gain "L" and the device volume cap (target ≤ 0.5 A); `SYS_USB5V` under load (≥ 4.75 V, no reset).
- The periodic ~20–35 mA dip in display sleep.

## Creative Pebble V3 (USB)

VID 041e / PID 3272, USB 1.1 full speed, UAC 1.0. Control interface 1 with feature unit 9 (mute; volume −60…0 dB in 0.6 dB steps, persists across power cycles). Streaming interface 2, alt 1: PCM 16 bit stereo, **48 kHz only**, isochronous OUT endpoint 0x02, 192 bytes per 1 ms, sample-rate control on the endpoint. Interface 3: HID (volume buttons), ignored. LED: solid green = USB audio, flashing green = Bluetooth pairing (USB set-up failed).
