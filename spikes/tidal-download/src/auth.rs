use anyhow::{Context, Result, bail, ensure};
use reqwest::Url;
use std::{fs, future::Future, io::Write, path::Path, time::Duration};
use tidlers::{TidalClient, TidalError, auth::TidalAuth};

// Do not expose upstream error bodies: they can include credentials and URLs.
pub async fn api_call<T>(
    stage: &str,
    future: impl Future<Output = Result<T, TidalError>>,
) -> Result<T> {
    match tokio::time::timeout(Duration::from_secs(45), future).await {
        Err(_) => bail!("{stage}: timed out after 45 seconds; retry when the network is available"),
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            let detail = match error {
                TidalError::RequestClient(tidlers::requests::RequestClientError::StatusCode {
                    status,
                    ..
                }) => format!("HTTP {status}"),
                TidalError::NotAuthenticated => "not authenticated".into(),
                TidalError::JsonParse(_) => "unexpected TIDAL response format".into(),
                _ => "TIDAL request failed".into(),
            };
            bail!(
                "{stage}: {detail}. Retry login if the session was revoked; upstream client compatibility may have changed"
            )
        }
    }
}

pub fn lock_session(path: &Path) -> Result<fs::File> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))?;
    file.try_lock()
        .context("Another command is using this session; wait for it to finish")?;
    Ok(file)
}

pub fn save(client: &TidalClient, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(client.get_json().as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path)
        .context("Cannot save refreshed session")?;
    Ok(())
}

pub async fn load(path: &Path) -> Result<TidalClient> {
    let raw =
        fs::read_to_string(path).context("No saved session; run `cargo run -- login` first")?;
    let mut client =
        TidalClient::from_json(&raw).context("Invalid session file; run login again")?;
    refresh(&mut client, false).await?;
    save(&client, path)?;
    ensure!(
        client.session.auth.access_token.is_some(),
        "Session has no access token; run login"
    );
    Ok(client)
}

// Keep token rotation as well as expiry updates. The upstream refresh response
// model currently omits an optional rotated refresh_token.
pub async fn refresh(client: &mut TidalClient, force: bool) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let auth = &client.session.auth;
    let expires = auth
        .last_refresh_time
        .unwrap_or(0)
        .saturating_add(auth.refresh_expiry.unwrap_or(0));
    if !force && expires > now.saturating_add(60) {
        return Ok(());
    }
    let refresh_token = auth
        .refresh_token
        .as_deref()
        .context("No refresh token; run login again")?;
    let (id, secret) = if auth.pkce_login {
        (&auth.pkce_config.client_id, &auth.pkce_config.client_secret)
    } else {
        (&auth.client_id, &auth.client_secret)
    };
    let http = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(45))
        .build()?;
    let mut response = http
        .post("https://auth.tidal.com/v1/oauth2/token")
        .basic_auth(id, Some(secret))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Session refresh network failure; retry or run login"))?;
    ensure!(
        response.status().is_success(),
        "Session refresh: HTTP {}; run login again if revoked",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("Incomplete refresh response"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= 65536,
            "Refresh response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    apply_refresh(
        client,
        &serde_json::from_slice(&bytes).context("Invalid refresh response")?,
        now,
    )
}

fn apply_refresh(client: &mut TidalClient, value: &serde_json::Value, now: u64) -> Result<()> {
    let token = value["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Refresh response has no access token")?;
    let expiry = value["expires_in"]
        .as_u64()
        .filter(|n| *n > 0)
        .context("Refresh response has no expiry")?;
    let auth = &mut client.session.auth;
    auth.access_token = Some(token.to_owned());
    auth.refresh_expiry = Some(expiry);
    auth.last_refresh_time = Some(now);
    if let Some(token) = value["refresh_token"].as_str().filter(|s| !s.is_empty()) {
        auth.refresh_token = Some(token.to_owned());
    }
    Ok(())
}

pub fn validate_redirect(login_url: &str, redirect: &str, expected: &str) -> Result<()> {
    let login = Url::parse(login_url)?;
    let redirect = Url::parse(redirect).context("Paste the complete browser redirect URL")?;
    let target = Url::parse(expected)?;
    ensure!(
        redirect.origin() == target.origin()
            && redirect.path() == target.path()
            && redirect.username().is_empty()
            && redirect.password().is_none(),
        "Unexpected redirect address"
    );
    let state = login
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .context("Missing login state")?;
    let states: Vec<_> = redirect
        .query_pairs()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .collect();
    ensure!(
        states == [state],
        "Login state mismatch; paste the redirect from this login attempt"
    );
    ensure!(
        !redirect.query_pairs().any(|(k, _)| k == "error"),
        "TIDAL denied login; try again"
    );
    let codes: Vec<_> = redirect
        .query_pairs()
        .filter(|(k, _)| k == "code")
        .collect();
    ensure!(
        codes.len() == 1 && !codes[0].1.is_empty(),
        "Redirect has no single authorization code"
    );
    Ok(())
}

pub async fn login(path: &Path, device: bool) -> Result<()> {
    let auth = if device {
        TidalAuth::with_oauth()
    } else {
        TidalAuth::with_pkce()
    };
    let mut client = TidalClient::new(&auth);
    if device {
        let link = api_call("Device login", client.get_oauth_link()).await?;
        println!(
            "Open {} in your host browser and approve the login.",
            link.verification_uri_complete
        );
        tokio::time::timeout(
            Duration::from_secs(link.expires_in.min(900)),
            client.wait_for_oauth(&link.device_code, link.expires_in, link.interval, None),
        )
        .await
        .context("Device login expired; run login again")?
        .map_err(|_| anyhow::anyhow!("Device login failed; try the default PKCE login"))?;
    } else {
        let mut url = Url::parse(
            &client
                .initiate_pkce_login()
                .map_err(|_| anyhow::anyhow!("Cannot initialize PKCE"))?,
        )?;
        // Upstream supplies PKCE but does not check state; bind the pasted callback here.
        url.query_pairs_mut()
            .append_pair("state", &client.session.auth.pkce_config.code_challenge);
        println!(
            "Open this URL in your host browser:\n{url}\n\nSign in to TIDAL. After redirection, copy the FULL address from the browser,\neven if the destination page is blank or shows an error. Keep this command running."
        );
        let redirect =
            rpassword::prompt_password("Paste the full redirect URL (hidden), then Enter: ")?;
        validate_redirect(
            url.as_str(),
            redirect.trim(),
            &client.session.auth.pkce_config.redirect_uri,
        )?;
        api_call(
            "Finish PKCE login",
            client.finish_pkce_login(redirect.trim()),
        )
        .await?;
    }
    save(&client, path)?;
    println!(
        "Login saved to {}. Next: cargo run -- download <TIDAL-track-URL-or-ID>",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_preserves_or_rotates_the_refresh_token() {
        let mut client = TidalClient::new(&TidalAuth::with_pkce());
        client.session.auth.refresh_token = Some("old-test-token".into());
        apply_refresh(
            &mut client,
            &serde_json::json!({"access_token":"a", "expires_in":3600}),
            100,
        )
        .unwrap();
        assert_eq!(
            client.session.auth.refresh_token.as_deref(),
            Some("old-test-token")
        );
        apply_refresh(&mut client, &serde_json::json!({"access_token":"b", "expires_in":3600, "refresh_token":"rotated-test-token"}), 200).unwrap();
        assert_eq!(
            client.session.auth.refresh_token.as_deref(),
            Some("rotated-test-token")
        );
        assert_eq!(client.session.auth.access_token.as_deref(), Some("b"));
        assert_eq!(client.session.auth.last_refresh_time, Some(200));
    }
}
