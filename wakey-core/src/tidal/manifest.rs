//! Stream manifests:
//!
//! - BTS (`application/vnd.tidal.bts`): JSON with mirror URLs of one
//!   complete file (FLAC for lossless).
//! - DASH (`application/dash+xml`, static, one period): the segments of a
//!   fragmented MP4 (FLAC inside for lossless), in order, initialization
//!   first; see [`super::flac_mp4`].
//!
//! Only unencrypted FLAC is accepted (the clock decodes nothing else). All
//! URLs must be HTTPS without user info.

use super::{Error, Result, ensure, err};
use base64::Engine;
use roxmltree::Node;
use serde::Deserialize;
use url::Url;

const MAX_SEGMENTS: usize = 20_000;

/// What to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPlan {
    /// Mirrors of the same FLAC file: the first that works.
    Mirrors(Vec<String>),
    /// Fragmented MP4 segments, initialization first: all, in order.
    Segments(Vec<String>),
}

fn https_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| err("stream: invalid media URL"))?;
    ensure!(
        url.scheme() == "https" && url.username().is_empty() && url.password().is_none(),
        "stream: media URL not plain HTTPS"
    );
    Ok(url)
}

fn is_flac(codecs: &str) -> bool {
    codecs.to_ascii_lowercase().contains("flac")
}

/// The plan for a manifest (`manifest`: base64, as the API sends it).
pub fn plan(mime: &str, manifest: &str, seconds: f64) -> Result<MediaPlan> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(manifest.trim())
        .map_err(|_| err("stream: invalid manifest"))?;
    match mime {
        "application/vnd.tidal.bts" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Bts {
                #[serde(default)]
                codecs: Option<String>,
                encryption_type: String,
                urls: Vec<String>,
            }
            let bts: Bts =
                serde_json::from_slice(&decoded).map_err(|_| err("stream: invalid manifest"))?;
            ensure!(bts.encryption_type == "NONE", "stream: encrypted");
            ensure!(
                bts.codecs.as_deref().is_none_or(is_flac),
                "stream: not FLAC"
            );
            ensure!(
                !bts.urls.is_empty() && bts.urls.len() <= 32,
                "stream: no mirrors"
            );
            let urls = bts
                .urls
                .iter()
                .map(|u| https_url(u).map(String::from))
                .collect::<Result<_>>()?;
            Ok(MediaPlan::Mirrors(urls))
        }
        "application/dash+xml" => {
            let xml =
                core::str::from_utf8(&decoded).map_err(|_| err("stream: invalid manifest"))?;
            Ok(MediaPlan::Segments(dash_urls(xml, seconds)?))
        }
        _ => Err(err("stream: unsupported manifest")),
    }
}

fn children<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Vec<Node<'a, 'i>> {
    node.children().filter(|n| n.has_tag_name(name)).collect()
}

fn integer(node: Node<'_, '_>, key: &str, default: u64) -> Result<u64> {
    match node.attribute(key) {
        Some(v) => v.parse().map_err(|_| Error(format!("DASH: invalid {key}"))),
        None => Ok(default),
    }
}

/// Expands a DASH template (`$Number%05d$` and the like).
fn substitute(template: &str, id: &str, bandwidth: &str, number: u64, time: u64) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        if let Some(tail) = rest.strip_prefix('$') {
            out.push('$');
            rest = tail;
            continue;
        }
        let end = rest
            .find('$')
            .ok_or_else(|| err("DASH: unterminated template"))?;
        let token = &rest[..end];
        let (name, width) = match token.split_once('%') {
            Some((name, fmt)) => {
                let width: usize = fmt
                    .strip_prefix('0')
                    .and_then(|s| s.strip_suffix('d'))
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| err("DASH: unsupported number format"))?;
                ensure!(width <= 20, "DASH: number format too wide");
                (name, width)
            }
            None => (token, 0),
        };
        match name {
            "RepresentationID" => out.push_str(id),
            "Bandwidth" => out.push_str(bandwidth),
            "Number" => out.push_str(&format!("{number:0width$}")),
            "Time" => out.push_str(&format!("{time:0width$}")),
            _ => return Err(err("DASH: unsupported template variable")),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// An ISO 8601 duration as DASH uses it (`PT3M45.5S`), in seconds.
fn presentation_seconds(value: &str) -> Result<f64> {
    let rest = value
        .strip_prefix("PT")
        .ok_or_else(|| err("DASH: unsupported duration"))?;
    let mut number = String::new();
    let mut total = 0.0;
    for c in rest.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let n: f64 = number.parse().map_err(|_| err("DASH: invalid duration"))?;
        total += n * match c {
            'H' => 3600.0,
            'M' => 60.0,
            'S' => 1.0,
            _ => return Err(err("DASH: unsupported duration unit")),
        };
        number.clear();
    }
    ensure!(
        number.is_empty() && total.is_finite() && total > 0.0 && total <= 86_400.0,
        "DASH: invalid duration"
    );
    Ok(total)
}

/// The segment URLs of a static DASH manifest, initialization first.
fn dash_urls(xml: &str, seconds: f64) -> Result<Vec<String>> {
    ensure!(
        seconds.is_finite() && seconds > 0.0 && seconds <= 86_400.0,
        "DASH: invalid track duration"
    );
    let doc = roxmltree::Document::parse(xml).map_err(|_| err("DASH: invalid XML"))?;
    let root = doc.root_element();
    ensure!(
        root.has_tag_name("MPD") && root.attribute("type") != Some("dynamic"),
        "DASH: only static audio"
    );
    ensure!(
        !doc.descendants()
            .any(|n| n.has_tag_name("ContentProtection")),
        "stream: encrypted"
    );
    let periods = children(root, "Period");
    ensure!(periods.len() == 1, "DASH: only one period");
    let period = periods[0];
    let seconds = match period
        .attribute("duration")
        .or_else(|| root.attribute("mediaPresentationDuration"))
    {
        Some(d) => presentation_seconds(d)?,
        None => seconds,
    };
    let mut choices = Vec::new();
    for adaptation in children(period, "AdaptationSet") {
        for rep in children(adaptation, "Representation") {
            let mime = rep
                .attribute("mimeType")
                .or_else(|| adaptation.attribute("mimeType"))
                .unwrap_or("");
            if mime.starts_with("audio/") || adaptation.attribute("contentType") == Some("audio") {
                choices.push((integer(rep, "bandwidth", 0)?, adaptation, rep));
            }
        }
    }
    let (_, adaptation, rep) = choices
        .into_iter()
        .max_by_key(|c| c.0)
        .ok_or_else(|| err("DASH: no audio"))?;
    let codecs = rep
        .attribute("codecs")
        .or_else(|| adaptation.attribute("codecs"));
    ensure!(codecs.is_none_or(is_flac), "stream: not FLAC");
    let mut base: Option<Url> = None;
    for node in [root, period, adaptation, rep] {
        if let Some(child) = children(node, "BaseURL").first() {
            let value = child
                .text()
                .ok_or_else(|| err("DASH: empty BaseURL"))?
                .trim();
            base = Some(match base {
                Some(ref b) => b.join(value).map_err(|_| err("DASH: invalid BaseURL"))?,
                None => https_url(value)?,
            });
        }
    }
    let resolve = |value: &str| -> Result<String> {
        let url = match &base {
            Some(b) => b
                .join(value)
                .map_err(|_| err("DASH: invalid segment URL"))?,
            None => https_url(value)?,
        };
        https_url(url.as_str()).map(String::from)
    };
    let templates: Vec<_> = [rep, adaptation, period]
        .into_iter()
        .flat_map(|n| children(n, "SegmentTemplate"))
        .collect();
    let attribute = |name: &str| templates.iter().find_map(|n| n.attribute(name));
    let template_integer = |name: &str, default: u64| -> Result<u64> {
        match attribute(name) {
            Some(v) => v
                .parse()
                .map_err(|_| Error(format!("DASH: invalid {name}"))),
            None => Ok(default),
        }
    };
    let list = [rep, adaptation, period]
        .into_iter()
        .find_map(|n| children(n, "SegmentList").first().copied());
    if let Some(list) = list {
        let mut urls = Vec::new();
        if let Some(init) = children(list, "Initialization").first() {
            ensure!(init.attribute("range").is_none(), "DASH: byte ranges");
            let src = init
                .attribute("sourceURL")
                .ok_or_else(|| err("DASH: no initialization URL"))?;
            urls.push(resolve(src)?);
        }
        for seg in children(list, "SegmentURL") {
            ensure!(seg.attribute("mediaRange").is_none(), "DASH: byte ranges");
            urls.push(resolve(
                seg.attribute("media")
                    .ok_or_else(|| err("DASH: no media URL"))?,
            )?);
        }
        ensure!(
            !urls.is_empty() && urls.len() <= MAX_SEGMENTS,
            "DASH: invalid segment count"
        );
        return Ok(urls);
    }
    if templates.is_empty() {
        ensure!(
            !doc.descendants().any(|n| n.has_tag_name("SegmentBase")),
            "DASH: SegmentBase unsupported"
        );
        let base = base.ok_or_else(|| err("DASH: no media URL"))?;
        return Ok(vec![https_url(base.as_str())?.into()]);
    }
    let id = rep.attribute("id").unwrap_or("");
    let bandwidth = rep.attribute("bandwidth").unwrap_or("");
    let media = attribute("media").ok_or_else(|| err("DASH: no media template"))?;
    let mut number = template_integer("startNumber", 1)?;
    let timescale = template_integer("timescale", 1)?;
    ensure!(timescale > 0, "DASH: invalid timescale");
    let offset = template_integer("presentationTimeOffset", 0)?;
    let end_time = offset
        .checked_add((seconds * timescale as f64).ceil() as u64)
        .ok_or_else(|| err("DASH: duration overflow"))?;
    let mut urls = Vec::new();
    if let Some(init) = attribute("initialization") {
        urls.push(resolve(&substitute(init, id, bandwidth, number, 0)?)?);
    }
    let timeline = templates
        .iter()
        .find_map(|n| children(*n, "SegmentTimeline").first().copied());
    let entries = timeline.map(|n| children(n, "S")).unwrap_or_default();
    let mut append = |time: u64, number: u64| -> Result<()> {
        ensure!(urls.len() < MAX_SEGMENTS, "DASH: too many segments");
        urls.push(resolve(&substitute(media, id, bandwidth, number, time)?)?);
        Ok(())
    };
    let overflow = || err("DASH: overflow");
    if entries.is_empty() {
        let duration = template_integer("duration", 0)?;
        ensure!(duration > 0, "DASH: no timeline or segment duration");
        let count = (seconds * timescale as f64 / duration as f64).ceil() as u64;
        ensure!(count < MAX_SEGMENTS as u64, "DASH: too many segments");
        for i in 0..count {
            let t = i.checked_mul(duration).and_then(|t| t.checked_add(offset));
            append(t.ok_or_else(overflow)?, number)?;
            number = number.checked_add(1).ok_or_else(overflow)?;
        }
    } else {
        let mut time = 0;
        for (index, entry) in entries.iter().enumerate() {
            time = integer(*entry, "t", time)?;
            let duration = integer(*entry, "d", 0)?;
            ensure!(duration > 0, "DASH: invalid segment duration");
            let repeat: i64 = entry
                .attribute("r")
                .unwrap_or("0")
                .parse()
                .map_err(|_| err("DASH: invalid repetition"))?;
            ensure!(repeat >= -1, "DASH: invalid repetition");
            let count = if repeat == -1 {
                let until = match entries.get(index + 1) {
                    Some(n) => integer(*n, "t", end_time)?,
                    None => end_time,
                };
                ensure!(until > time, "DASH: invalid timeline");
                (until - time).div_ceil(duration)
            } else {
                repeat as u64 + 1
            };
            ensure!(count < MAX_SEGMENTS as u64, "DASH: too many segments");
            for _ in 0..count {
                append(time, number)?;
                time = time.checked_add(duration).ok_or_else(overflow)?;
                number = number.checked_add(1).ok_or_else(overflow)?;
            }
        }
    }
    ensure!(!urls.is_empty(), "DASH: empty");
    Ok(urls)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn bts_mirrors() {
        let m = b64(
            r#"{"mimeType":"audio/flac","codecs":"flac","encryptionType":"NONE",
            "urls":["https://a.example/x.flac","https://b.example/x.flac"]}"#,
        );
        assert_eq!(
            plan("application/vnd.tidal.bts", &m, 10.0).unwrap(),
            MediaPlan::Mirrors(vec![
                "https://a.example/x.flac".into(),
                "https://b.example/x.flac".into()
            ])
        );
        let aac =
            b64(r#"{"codecs":"mp4a.40.2","encryptionType":"NONE","urls":["https://a.example/x"]}"#);
        assert!(plan("application/vnd.tidal.bts", &aac, 10.0).is_err());
        let enc =
            b64(r#"{"codecs":"flac","encryptionType":"OLD_AES","urls":["https://a.example/x"]}"#);
        assert!(plan("application/vnd.tidal.bts", &enc, 10.0).is_err());
        let http =
            b64(r#"{"codecs":"flac","encryptionType":"NONE","urls":["http://a.example/x"]}"#);
        assert!(plan("application/vnd.tidal.bts", &http, 10.0).is_err());
    }

    #[test]
    fn dash_template_with_timeline() {
        // As TIDAL sends it (shape; URLs made up).
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT9.5S">
 <Period id="0"><AdaptationSet id="0" contentType="audio" mimeType="audio/mp4">
  <Representation id="FLAC,44100,16" codecs="flac" bandwidth="1000000" audioSamplingRate="44100">
   <SegmentTemplate timescale="44100" initialization="https://sp.example/0.mp4?t=1"
     media="https://sp.example/$Number$.mp4?t=1" startNumber="1">
    <SegmentTimeline><S d="176128" r="1"/><S d="66816"/></SegmentTimeline>
   </SegmentTemplate>
  </Representation></AdaptationSet></Period></MPD>"#;
        let urls = dash_urls(xml, 10.0).unwrap();
        assert_eq!(
            urls,
            vec![
                "https://sp.example/0.mp4?t=1",
                "https://sp.example/1.mp4?t=1",
                "https://sp.example/2.mp4?t=1",
                "https://sp.example/3.mp4?t=1",
            ]
        );
        assert_eq!(
            plan("application/dash+xml", &b64(xml), 10.0).unwrap(),
            MediaPlan::Segments(urls)
        );
    }

    #[test]
    fn dash_template_without_timeline_and_formats() {
        let xml = r#"<MPD><Period duration="PT4S"><AdaptationSet mimeType="audio/mp4">
          <BaseURL>https://cdn.example/a/</BaseURL>
          <Representation id="r1" bandwidth="5" codecs="flac">
            <SegmentTemplate duration="2" timescale="1" initialization="init_$RepresentationID$.mp4"
              media="seg_$Number%03d$_$Time$.mp4" startNumber="0"/>
          </Representation></AdaptationSet></Period></MPD>"#;
        assert_eq!(
            dash_urls(xml, 99.0).unwrap(),
            vec![
                "https://cdn.example/a/init_r1.mp4",
                "https://cdn.example/a/seg_000_0.mp4",
                "https://cdn.example/a/seg_001_2.mp4",
            ]
        );
    }

    #[test]
    fn dash_rejections() {
        let protected = r#"<MPD><Period><AdaptationSet mimeType="audio/mp4"><ContentProtection/>
            <Representation bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(dash_urls(protected, 10.0).is_err());
        let aac = r#"<MPD><Period><AdaptationSet mimeType="audio/mp4">
            <Representation bandwidth="1" codecs="mp4a.40.2"><BaseURL>https://x.example/a</BaseURL>
            </Representation></AdaptationSet></Period></MPD>"#;
        assert!(dash_urls(aac, 10.0).is_err());
        let live = r#"<MPD type="dynamic"><Period/></MPD>"#;
        assert!(dash_urls(live, 10.0).is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(presentation_seconds("PT1H2M3.5S").unwrap(), 3723.5);
        assert!(presentation_seconds("P1D").is_err());
    }
}
