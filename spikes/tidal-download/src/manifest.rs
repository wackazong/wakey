use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Url;
use roxmltree::Node;
use serde::Deserialize;

const MAX_SEGMENTS: usize = 20_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playback {
    pub track_id: u64,
    pub asset_presentation: String,
    pub audio_quality: String,
    pub manifest_mime_type: String,
    pub manifest: String,
}

pub enum MediaPlan {
    // BTS URLs are mirrors of the same complete file, not sequential segments.
    Mirrors(Vec<Url>),
    Segments(Vec<Url>),
}

pub fn https_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid media URL"))?;
    ensure!(
        url.scheme() == "https" && url.username().is_empty() && url.password().is_none(),
        "Media must use HTTPS without embedded user credentials"
    );
    Ok(url)
}

impl Playback {
    pub fn plan(&self, track: u64, seconds: f64) -> Result<MediaPlan> {
        ensure!(self.track_id == track, "Wrong track returned");
        ensure!(
            self.asset_presentation == "FULL",
            "Preview/non-full media rejected"
        );
        ensure!(
            matches!(
                self.audio_quality.as_str(),
                "LOW" | "HIGH" | "LOSSLESS" | "HI_RES_LOSSLESS" | "HI_RES"
            ),
            "Unknown returned audio quality"
        );
        let decoded = STANDARD
            .decode(&self.manifest)
            .context("Invalid base64 manifest")?;
        match self.manifest_mime_type.as_str() {
            "application/vnd.tidal.bts" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Bts {
                    encryption_type: String,
                    urls: Vec<String>,
                }
                let bts: Bts = serde_json::from_slice(&decoded).context("Invalid BTS manifest")?;
                ensure!(
                    bts.encryption_type == "NONE",
                    "Encrypted media is not supported; try another quality"
                );
                ensure!(
                    !bts.urls.is_empty() && bts.urls.len() <= 32,
                    "Invalid BTS mirror count"
                );
                Ok(MediaPlan::Mirrors(
                    bts.urls
                        .iter()
                        .map(|s| https_url(s))
                        .collect::<Result<_>>()?,
                ))
            }
            "application/dash+xml" => Ok(MediaPlan::Segments(dash_urls(
                std::str::from_utf8(&decoded)?,
                seconds,
            )?)),
            _ => bail!("Unsupported media manifest; try another quality"),
        }
    }
}

fn children<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Vec<Node<'a, 'input>> {
    node.children().filter(|n| n.has_tag_name(name)).collect()
}
fn integer(node: Node<'_, '_>, key: &str, default: u64) -> Result<u64> {
    node.attribute(key)
        .map(str::parse)
        .transpose()
        .with_context(|| format!("Invalid DASH {key}"))
        .map(|n| n.unwrap_or(default))
}

// Expand DASH templates without guessing the number of segments or ignoring timelines.
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
        let end = rest.find('$').context("Unterminated DASH template")?;
        let token = &rest[..end];
        let (name, width) = if let Some((name, fmt)) = token.split_once('%') {
            let width: usize = fmt
                .strip_prefix('0')
                .and_then(|s| s.strip_suffix('d'))
                .context("Unsupported DASH number format")?
                .parse()?;
            ensure!(width <= 20, "DASH format too wide");
            (name, width)
        } else {
            (token, 0)
        };
        match name {
            "RepresentationID" => out.push_str(id),
            "Bandwidth" => out.push_str(bandwidth),
            "Number" => out.push_str(&format!("{number:0width$}")),
            "Time" => out.push_str(&format!("{time:0width$}")),
            _ => bail!("Unsupported DASH template variable"),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

// MPEG-DASH expresses static presentation length as an ISO-8601 duration.
fn presentation_seconds(value: &str) -> Result<f64> {
    let rest = value
        .strip_prefix("PT")
        .context("Unsupported DASH presentation duration")?;
    let mut number = String::new();
    let mut total = 0.0;
    for c in rest.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let n: f64 = number
            .parse()
            .context("Invalid DASH presentation duration")?;
        total += n * match c {
            'H' => 3600.0,
            'M' => 60.0,
            'S' => 1.0,
            _ => bail!("Unsupported DASH duration unit"),
        };
        number.clear();
    }
    ensure!(
        number.is_empty() && total.is_finite() && total > 0.0 && total <= 86_400.0,
        "Invalid DASH presentation duration"
    );
    Ok(total)
}

pub fn dash_urls(xml: &str, seconds: f64) -> Result<Vec<Url>> {
    ensure!(
        seconds.is_finite() && seconds > 0.0 && seconds <= 86_400.0,
        "Invalid track duration"
    );
    let doc = roxmltree::Document::parse(xml).context("Invalid DASH XML")?;
    let root = doc.root_element();
    ensure!(
        root.has_tag_name("MPD") && root.attribute("type") != Some("dynamic"),
        "Only static DASH audio is supported"
    );
    ensure!(
        !doc.descendants()
            .any(|n| n.has_tag_name("ContentProtection")),
        "Encrypted DASH is not supported; try another quality"
    );
    let periods = children(root, "Period");
    ensure!(periods.len() == 1, "Only single-period DASH is supported");
    let period = periods[0];
    // Prefer exact MPD duration over integer catalog duration when expanding segments.
    let seconds = period
        .attribute("duration")
        .or_else(|| root.attribute("mediaPresentationDuration"))
        .map(presentation_seconds)
        .transpose()?
        .unwrap_or(seconds);
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
        .context("DASH has no audio representation")?;
    let mut base: Option<Url> = None;
    for node in [root, period, adaptation, rep] {
        if let Some(child) = children(node, "BaseURL").first() {
            let value = child.text().context("Empty DASH BaseURL")?.trim();
            base = Some(match base {
                Some(ref b) => b
                    .join(value)
                    .map_err(|_| anyhow::anyhow!("Invalid DASH BaseURL"))?,
                None => https_url(value)?,
            });
        }
    }
    let resolve = |value: &str| -> Result<Url> {
        let url = match &base {
            Some(b) => b
                .join(value)
                .map_err(|_| anyhow::anyhow!("Invalid DASH segment URL"))?,
            None => https_url(value)?,
        };
        https_url(url.as_str())
    };
    let templates: Vec<_> = [rep, adaptation, period]
        .into_iter()
        .flat_map(|n| children(n, "SegmentTemplate"))
        .collect();
    let attribute = |name: &str| templates.iter().find_map(|n| n.attribute(name));
    let template_integer = |name: &str, default: u64| -> Result<u64> {
        attribute(name)
            .map(str::parse)
            .transpose()
            .with_context(|| format!("Invalid DASH {name}"))
            .map(|n| n.unwrap_or(default))
    };
    let list = [rep, adaptation, period]
        .into_iter()
        .find_map(|n| children(n, "SegmentList").first().copied());
    if let Some(list) = list {
        let mut urls = Vec::new();
        if let Some(init) = children(list, "Initialization").first() {
            ensure!(
                init.attribute("range").is_none(),
                "DASH byte ranges are unsupported"
            );
            urls.push(resolve(
                init.attribute("sourceURL")
                    .context("Missing DASH initialization URL")?,
            )?);
        }
        for seg in children(list, "SegmentURL") {
            ensure!(
                seg.attribute("mediaRange").is_none(),
                "DASH byte ranges are unsupported"
            );
            urls.push(resolve(
                seg.attribute("media").context("Missing DASH media URL")?,
            )?);
        }
        ensure!(
            !urls.is_empty() && urls.len() <= MAX_SEGMENTS,
            "Invalid DASH segment count"
        );
        return Ok(urls);
    }
    if templates.is_empty() {
        ensure!(
            !doc.descendants().any(|n| n.has_tag_name("SegmentBase")),
            "DASH SegmentBase is unsupported"
        );
        return Ok(vec![https_url(
            base.context("Missing DASH media URL")?.as_str(),
        )?]);
    }
    let id = rep.attribute("id").unwrap_or("");
    let bandwidth = rep.attribute("bandwidth").unwrap_or("");
    let media = attribute("media").context("Missing DASH media template")?;
    let mut number = template_integer("startNumber", 1)?;
    let timescale = template_integer("timescale", 1)?;
    ensure!(timescale > 0, "Invalid DASH timescale");
    let offset = template_integer("presentationTimeOffset", 0)?;
    let end_time = offset
        .checked_add((seconds * timescale as f64).ceil() as u64)
        .context("DASH duration overflow")?;
    let mut urls = Vec::new();
    if let Some(init) = attribute("initialization") {
        urls.push(resolve(&substitute(init, id, bandwidth, number, 0)?)?);
    }
    let timeline = templates
        .iter()
        .find_map(|n| children(*n, "SegmentTimeline").first().copied());
    let entries = timeline.map(|n| children(n, "S")).unwrap_or_default();
    let mut time = 0;
    let mut append = |time: u64, number: u64| -> Result<()> {
        ensure!(urls.len() < MAX_SEGMENTS, "DASH exceeds segment limit");
        urls.push(resolve(&substitute(media, id, bandwidth, number, time)?)?);
        Ok(())
    };
    if entries.is_empty() {
        let duration = template_integer("duration", 0)?;
        ensure!(duration > 0, "DASH needs a timeline or segment duration");
        let count = (seconds * timescale as f64 / duration as f64).ceil() as u64;
        ensure!(count < MAX_SEGMENTS as u64, "Too many DASH segments");
        for i in 0..count {
            append(
                offset
                    .checked_add(i.checked_mul(duration).context("DASH time overflow")?)
                    .context("DASH time overflow")?,
                number,
            )?;
            number = number.checked_add(1).context("DASH number overflow")?;
        }
    } else {
        for (index, entry) in entries.iter().enumerate() {
            time = integer(*entry, "t", time)?;
            let duration = integer(*entry, "d", 0)?;
            ensure!(duration > 0, "Invalid DASH segment duration");
            let repeat: i64 = entry.attribute("r").unwrap_or("0").parse()?;
            ensure!(repeat >= -1, "Invalid DASH repetition");
            let count = if repeat == -1 {
                let until = entries
                    .get(index + 1)
                    .map(|n| integer(*n, "t", end_time))
                    .transpose()?
                    .unwrap_or(end_time);
                ensure!(until > time, "Invalid DASH timeline boundary");
                (until - time).div_ceil(duration)
            } else {
                repeat as u64 + 1
            };
            ensure!(count < MAX_SEGMENTS as u64, "Too many DASH segments");
            for _ in 0..count {
                append(time, number)?;
                time = time.checked_add(duration).context("DASH time overflow")?;
                number = number.checked_add(1).context("DASH number overflow")?;
            }
        }
    }
    ensure!(!urls.is_empty(), "Empty DASH manifest");
    Ok(urls)
}
