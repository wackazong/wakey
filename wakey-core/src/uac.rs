//! USB Audio Class 1.0: parsing a configuration descriptor for playable
//! output streams, and choosing one.

const DESC_INTERFACE: u8 = 0x04;
const DESC_ENDPOINT: u8 = 0x05;
const CS_INTERFACE: u8 = 0x24;
const CS_ENDPOINT: u8 = 0x25;
const CLASS_AUDIO: u8 = 0x01;
const SUBCLASS_CONTROL: u8 = 0x01;
const SUBCLASS_STREAMING: u8 = 0x02;
const AC_FEATURE_UNIT: u8 = 0x06;
const AS_FORMAT_TYPE: u8 = 0x02;
const EP_ISOCHRONOUS: u8 = 0x01;

/// An alternate setting of an Audio Streaming interface with an
/// isochronous OUT endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAlt {
    pub interface: u8,
    pub alt: u8,
    pub endpoint: u8,
    pub max_packet: u16,
    pub interval: u8,
    pub channels: u8,
    /// Bytes per sample in the stream (2 = 16 bit).
    pub subframe_bytes: u8,
    pub bits: u8,
    pub rates: Rates,
    /// The endpoint accepts SET_CUR SAMPLING_FREQ.
    pub freq_control: bool,
}

/// The sample rates of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rates {
    Discrete(Vec<u32>),
    /// Any rate from the lower to the upper bound (inclusive).
    Range(u32, u32),
}

impl Rates {
    /// True if `rate` is offered; also when none are listed (some devices
    /// leave the format descriptor's table empty).
    pub fn contains(&self, rate: u32) -> bool {
        match self {
            Rates::Discrete(r) => r.is_empty() || r.contains(&rate),
            Rates::Range(lo, hi) => (*lo..=*hi).contains(&rate),
        }
    }
}

/// What a device offers for playback.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AudioFunction {
    pub control_interface: Option<u8>,
    /// Feature unit IDs (mute, volume).
    pub feature_units: Vec<u8>,
    pub outputs: Vec<StreamAlt>,
}

impl AudioFunction {
    /// The first output with this format whose packets fit
    /// `packet_bytes` (one 1 ms packet).
    pub fn find_output(
        &self,
        channels: u8,
        subframe_bytes: u8,
        rate: u32,
        packet_bytes: usize,
    ) -> Option<&StreamAlt> {
        self.outputs.iter().find(|o| {
            o.channels == channels
                && o.subframe_bytes == subframe_bytes
                && o.rates.contains(rate)
                && usize::from(o.max_packet) >= packet_bytes
        })
    }
}

/// Parses a complete configuration descriptor (as returned by the host
/// stack, `wTotalLength` bytes).
pub fn parse(config: &[u8]) -> AudioFunction {
    let mut f = AudioFunction::default();
    // Subclass of the current audio interface, if any.
    let mut subclass: Option<u8> = None;
    let mut alt: Option<StreamAlt> = None;
    let finish = |alt: &mut Option<StreamAlt>, f: &mut AudioFunction| {
        if let Some(a) = alt.take()
            && a.endpoint != 0
            && a.endpoint & 0x80 == 0
        {
            f.outputs.push(a);
        }
    };
    let mut i = 0;
    while i + 2 <= config.len() {
        let len = usize::from(config[i]);
        if len < 2 || i + len > config.len() {
            break;
        }
        let d = &config[i..i + len];
        match d[1] {
            DESC_INTERFACE if len >= 9 => {
                finish(&mut alt, &mut f);
                let (num, alt_n, class, sub) = (d[2], d[3], d[5], d[6]);
                subclass = (class == CLASS_AUDIO).then_some(sub);
                if subclass == Some(SUBCLASS_CONTROL) {
                    f.control_interface = Some(num);
                }
                if subclass == Some(SUBCLASS_STREAMING) && alt_n > 0 {
                    alt = Some(StreamAlt {
                        interface: num,
                        alt: alt_n,
                        endpoint: 0,
                        max_packet: 0,
                        interval: 0,
                        channels: 0,
                        subframe_bytes: 0,
                        bits: 0,
                        rates: Rates::Discrete(Vec::new()),
                        freq_control: false,
                    });
                }
            }
            CS_INTERFACE if len >= 3 => match subclass {
                Some(SUBCLASS_CONTROL) if d[2] == AC_FEATURE_UNIT && len >= 4 => {
                    f.feature_units.push(d[3]);
                }
                Some(SUBCLASS_STREAMING) if d[2] == AS_FORMAT_TYPE && len >= 8 => {
                    if let Some(a) = alt.as_mut() {
                        a.channels = d[4];
                        a.subframe_bytes = d[5];
                        a.bits = d[6];
                        let n = usize::from(d[7]);
                        let rate = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], 0]);
                        a.rates = if n == 0 && len >= 14 {
                            Rates::Range(rate(8), rate(11))
                        } else {
                            Rates::Discrete(
                                (0..n)
                                    .map(|k| 8 + 3 * k)
                                    .take_while(|o| o + 3 <= len)
                                    .map(rate)
                                    .collect(),
                            )
                        };
                    }
                }
                _ => {}
            },
            DESC_ENDPOINT if len >= 7 => {
                // OUT only: an async device follows its data endpoint with
                // an isochronous feedback IN endpoint.
                if let Some(a) = alt.as_mut()
                    && d[3] & 0x03 == EP_ISOCHRONOUS
                    && d[2] & 0x80 == 0
                {
                    a.endpoint = d[2];
                    a.max_packet = u16::from_le_bytes([d[4], d[5]]) & 0x7FF;
                    a.interval = d[6];
                }
            }
            CS_ENDPOINT if len >= 4 => {
                if let Some(a) = alt.as_mut() {
                    a.freq_control = d[3] & 0x01 != 0;
                }
            }
            _ => {}
        }
        i += len;
    }
    finish(&mut alt, &mut f);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of the Creative Pebble V3's descriptor that matter: audio
    /// control interface 1 with feature unit 9, streaming interface 2
    /// (alt 0 idle, alt 1 48 kHz/16 bit/stereo on EP 0x02, 192 bytes), and
    /// a HID interface with an interrupt IN endpoint.
    fn pebble() -> Vec<u8> {
        let mut v = vec![9, 0x02, 0, 0, 3, 1, 0, 0x80, 150]; // configuration
        v.extend([9, 0x04, 1, 0, 0, 1, 1, 0, 0]); // AC interface
        v.extend([10, 0x24, 1, 0, 1, 40, 0, 1, 2, 0]); // AC header
        v.extend([12, 0x24, 2, 1, 1, 1, 0, 2, 3, 0, 0, 0]); // input terminal
        v.extend([10, 0x24, 6, 9, 1, 1, 1, 2, 2, 0]); // feature unit 9
        v.extend([9, 0x24, 3, 3, 1, 3, 0, 9, 0]); // output terminal
        v.extend([9, 0x04, 2, 0, 0, 1, 2, 0, 0]); // AS alt 0
        v.extend([9, 0x04, 2, 1, 1, 1, 2, 0, 0]); // AS alt 1
        v.extend([7, 0x24, 1, 1, 1, 1, 0]); // AS general
        v.extend([11, 0x24, 2, 1, 2, 2, 16, 1, 0x80, 0xBB, 0x00]); // format: 48000
        v.extend([9, 0x05, 0x02, 0x09, 192, 0, 1, 0, 0]); // iso OUT
        v.extend([7, 0x25, 1, 1, 0, 0, 0]); // CS endpoint: freq control
        v.extend([9, 0x04, 3, 0, 1, 3, 0, 0, 0]); // HID
        v.extend([7, 0x05, 0x82, 0x03, 4, 0, 10]); // interrupt IN
        v
    }

    #[test]
    fn pebble_descriptor() {
        let f = parse(&pebble());
        assert_eq!(f.control_interface, Some(1));
        assert_eq!(f.feature_units, vec![9]);
        assert_eq!(f.outputs.len(), 1);
        let o = f
            .find_output(2, 2, 48_000, 192)
            .expect("48 kHz stereo output");
        assert_eq!(
            (o.interface, o.alt, o.endpoint, o.max_packet),
            (2, 1, 0x02, 192)
        );
        assert!(o.freq_control);
        assert!(f.find_output(2, 2, 44_100, 192).is_none());
        assert!(f.find_output(2, 2, 48_000, 193).is_none());
    }

    #[test]
    fn async_output_with_feedback_endpoint() {
        let mut d = pebble();
        // After the OUT endpoint (async, 0x05) and its CS endpoint: the
        // isochronous feedback IN endpoint 0x81.
        let at = d
            .windows(4)
            .position(|w| w == [9, 0x05, 0x02, 0x09])
            .unwrap();
        d[at + 3] = 0x05;
        let cs = at + 9 + 7;
        d.splice(cs..cs, [9, 0x05, 0x81, 0x11, 3, 0, 1, 3, 0]);
        let f = parse(&d);
        let o = f.find_output(2, 2, 48_000, 192).expect("async output");
        assert_eq!((o.endpoint, o.max_packet), (0x02, 192));
    }

    #[test]
    fn continuous_rate_range() {
        let mut d = pebble();
        // Format with bSamFreqType 0: 8 kHz to 96 kHz.
        let at = d.windows(4).position(|w| w == [11, 0x24, 2, 1]).unwrap();
        d.splice(
            at..at + 11,
            [
                14, 0x24, 2, 1, 2, 2, 16, 0, 0x40, 0x1F, 0x00, 0x00, 0x77, 0x01,
            ],
        );
        let f = parse(&d);
        assert_eq!(f.outputs[0].rates, Rates::Range(8_000, 96_000));
        assert!(f.find_output(2, 2, 48_000, 192).is_some());
        assert!(f.find_output(2, 2, 44_100, 192).is_some());
        assert!(f.find_output(2, 2, 192_000, 192).is_none());
    }

    #[test]
    fn truncated_descriptor_does_not_panic() {
        let d = pebble();
        for n in 0..d.len() {
            parse(&d[..n]);
        }
    }
}
