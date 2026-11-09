# Hardware reference

M5Stack Tab5 Kit (ESP32-P4 + ESP32-C6, 5" 720×1280 touch panel, NP-F550 battery), plus a Creative Pebble V3 USB speaker. From the schematic ([schematics/](schematics), [PDF](https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1132/Tab5_Schematics_PDF.pdf)), M5Stack's M5Unified/M5GFX sources and measurements on the unit here.

## The unit here

| | |
| --- | --- |
| ESP32-P4 | rev **v1.3** (`espflash board-info`), 360 MHz, 32 MB PSRAM (hex, 200 MHz), 16 MB flash |
| Panel | **ST7123** TDDI (display and touch in one chip), touch firmware 3 |
| USB | USB-C: P4's USB Serial/JTAG at `/dev/cu.usbmodem2101` (flashing, console) |
| C6 firmware | M5Stack factory esp_hosted slave, reports protocol version 0.0.0 |
| Battery | not connected |

Three panel variants exist across production runs; the firmware detects them from the touch controller: ST712x at 0x55 with firmware 3 → ST7123, 1 → ST7121; GT911 (0x14/0x5D) → ILI9881C. Only the ST712x touch is implemented.

## ESP32-P4 pins

| Function | GPIO | Note |
| --- | --- | --- |
| System I2C SDA / SCL | 31 / 32 | 400 kHz; expanders, RTC, INA226, touch, codec, IMU |
| Touch interrupt `TP_INT` | 23 | active low, 5.1 kΩ pull-up (R92); not an LP-GPIO (no deep-sleep wake). Not used yet (touch is polled) |
| Backlight `LCD_BL` | 22 | enable of the ME2212 boost; LEDC PWM 44.1 kHz |
| MIPI-DSI | dedicated | 2 lanes; PHY supply `VDD_MIPI_DPHY` = on-chip LDO 3 at 2.5 V |
| microSD (SDMMC slot 0) CLK / CMD / D0–D3 | 43 / 44 / 39–42 | IO_MUX pins, 4 bit; pin supply = on-chip LDO 4 at 3.3 V |
| ESP32-C6 (SDMMC slot 1) CLK / CMD / D0–D3 | 12 / 13 / 11, 10, 9, 8 | GPIO matrix, 4 bit, 40 MHz |
| C6 reset `SOC_EXTRF_RST` | 15 | → 1 kΩ → C6 EN (10 kΩ pull-up): low = reset |
| C6 IO2 `RF_C6_IO2` | 14 | unused |
| USB 2.0 HS OTG | dedicated | → USB-A socket: host port for the speaker |
| USB Serial/JTAG | 24 / 25 | → USB-C socket |
| I2S MCLK / SCLK / LRCK / DOUT / DIN | 30 / 27 / 29 / 26 / 28 | ES8388 codec and ES7210 mics; unused |
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
| RX8130CE RTC | 0x32 | UTC time; alarm registers (not armed yet) |
| ST7123 touch | 0x55 | polled every 10 ms (16-bit registers) |
| INA226 | 0x41 | battery voltage/current (5 mΩ shunt); unused |
| ES8388, ES7210, BMI270 | – | unused |

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

- USB-C path: `USB_5VIN` (FU1 2 A) → U34 MAX40200 (**1 A**) → `SYS_5VBUS` → U23 LPW5209 (≈ 2.4 A) → `SYS_INT5V` → U27 MT9700 (≈ 1.33 A) → USB-A. All 5 V outputs sit behind U34. Budget for the Pebble ≈ 0.5 A (gain switch "L", limited USB volume; not measured yet).
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

## Creative Pebble V3 (USB)

VID 041e / PID 3272, USB 1.1 full speed, UAC 1.0. Control interface 1 with feature unit 9 (mute; volume −60…0 dB in 0.6 dB steps, persists across power cycles). Streaming interface 2, alt 1: PCM 16 bit stereo, **48 kHz only**, isochronous OUT endpoint 0x02, 192 bytes per 1 ms, sample-rate control on the endpoint. Interface 3: HID (volume buttons), ignored. LED: solid green = USB audio, flashing green = Bluetooth pairing (USB set-up failed).

## Schematic pages

| File | Content |
| --- | --- |
| `tab5-sch-1-power.png` | power supply, PMS150, RTC, load switches |
| `tab5-sch-2-esp32p4-c6-ioexpander.png` | P4, C6, flash, expanders |
| `tab5-sch-3-display-touch-sd-imu-camera.png` | display, touch, SD, IMU, camera |
| `tab5-sch-4-audio-codec.png` | ES8388, ES7210, NS4150B |
| `tab5-sch-5-usb-bus-rf-rs485.png` | USB-C, USB-A, M5-Bus, 2×5 header, RF, RS-485 |
