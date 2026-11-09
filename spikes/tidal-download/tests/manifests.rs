use base64::{Engine, engine::general_purpose::STANDARD};
use tidal_download::{
    auth::validate_redirect,
    manifest::{MediaPlan, Playback, dash_urls},
};

fn playback(value: serde_json::Value) -> Playback {
    Playback {
        track_id: 1,
        asset_presentation: "FULL".into(),
        audio_quality: "LOSSLESS".into(),
        manifest_mime_type: "application/vnd.tidal.bts".into(),
        manifest: STANDARD.encode(value.to_string()),
    }
}
#[test]
fn bts_mirrors_and_access_checks() {
    let value = serde_json::json!({"encryptionType":"NONE", "urls":["https://example.com/a", "https://example.com/b"]});
    let mut p = playback(value.clone());
    assert!(matches!(p.plan(1, 10.0).unwrap(), MediaPlan::Mirrors(urls) if urls.len() == 2));
    assert!(p.plan(2, 10.0).is_err());
    p.asset_presentation = "PREVIEW".into();
    assert!(p.plan(1, 10.0).is_err());
    let mut encrypted = value.clone();
    encrypted["encryptionType"] = "OLD_AES".into();
    assert!(playback(encrypted).plan(1, 10.0).is_err());
    let mut insecure = value;
    insecure["urls"][0] = "http://example.com/a".into();
    assert!(playback(insecure).plan(1, 10.0).is_err());
}
fn mpd(body: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT5.5S"><BaseURL>https://example.com/audio/</BaseURL><Period><AdaptationSet mimeType="audio/mp4"><Representation id="a" bandwidth="1000">{body}</Representation></AdaptationSet></Period></MPD>"#
    )
}
#[test]
fn expands_timeline_time_number_format_and_xml_entities() {
    let urls = dash_urls(&mpd(r#"<SegmentTemplate initialization="init.mp4?x=1&amp;y=2" media="seg-$RepresentationID$-$Number%03d$-$Time$.m4s" startNumber="7" timescale="10"><SegmentTimeline><S t="10" d="20" r="1"/><S d="15"/></SegmentTimeline></SegmentTemplate>"#), 6.0).unwrap();
    assert_eq!(urls.len(), 4);
    assert_eq!(
        urls[0].as_str(),
        "https://example.com/audio/init.mp4?x=1&y=2"
    );
    assert!(urls[1].as_str().ends_with("seg-a-007-10.m4s"));
    assert!(urls[3].as_str().ends_with("seg-a-009-50.m4s"));
}
#[test]
fn uses_exact_presentation_length_and_handles_negative_repeat() {
    let urls = dash_urls(
        &mpd(r#"<SegmentTemplate media="$Number$.m4s" duration="55" timescale="10"/>"#),
        6.0,
    )
    .unwrap();
    assert_eq!(urls.len(), 1); // Rounded catalog duration must not invent a second segment.
    let urls = dash_urls(&mpd(r#"<SegmentTemplate media="$Time$.m4s" timescale="10"><SegmentTimeline><S d="20" r="-1"/></SegmentTimeline></SegmentTemplate>"#), 6.0).unwrap();
    assert_eq!(urls.len(), 3);
    assert!(urls[2].as_str().ends_with("40.m4s"));
}
#[test]
fn rejects_protected_dynamic_and_unbounded_manifests() {
    for body in [
        r#"<ContentProtection schemeIdUri="drm"/>"#,
        r#"<SegmentTemplate media="$Number$" duration="0"/>"#,
        r#"<SegmentTemplate media="$Number$"><SegmentTimeline><S d="1" r="999999999"/></SegmentTimeline></SegmentTemplate>"#,
        r#"<SegmentTemplate media="file:///etc/passwd" duration="2"/>"#,
    ] {
        assert!(dash_urls(&mpd(body), 6.0).is_err());
    }
    assert!(dash_urls(&mpd("").replace("static", "dynamic"), 6.0).is_err());
}
#[test]
fn callback_must_match_the_login_attempt() {
    let login = "https://login.tidal.com/authorize?state=abc";
    let target = "https://tidal.com/android/login/auth";
    assert!(validate_redirect(login, &format!("{target}?state=abc&code=one"), target).is_ok());
    for redirect in [
        format!("{target}?state=wrong&code=one"),
        format!("{target}?code=one"),
        format!("{target}?state=abc&code=one&code=two"),
        "https://evil.test/?state=abc&code=one".into(),
    ] {
        assert!(validate_redirect(login, &redirect, target).is_err());
    }
}

#[test]
fn representation_inherits_template_attributes_from_adaptation() {
    let xml = r#"<MPD mediaPresentationDuration="PT5S"><Period><AdaptationSet mimeType="audio/mp4"><BaseURL>https://example.com/</BaseURL><SegmentTemplate timescale="1000" duration="1000" initialization="init-$RepresentationID$.mp4"/><Representation id="1"><SegmentTemplate media="$Number$.m4s" startNumber="3"/></Representation></AdaptationSet></Period></MPD>"#;
    let urls = dash_urls(xml, 5.0).unwrap();
    assert_eq!(urls.len(), 6);
    assert!(urls[0].as_str().ends_with("init-1.mp4"));
    assert!(urls[1].as_str().ends_with("3.m4s"));
    assert!(urls[5].as_str().ends_with("7.m4s"));
}
