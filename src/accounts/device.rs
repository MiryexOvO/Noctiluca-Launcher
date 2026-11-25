use anyhow::{Context, Result, bail, ensure};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use url::Url;

const AUTHORITY: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";
const SCOPE: &str = "XboxLive.signin offline_access";
const TIMEOUT: Duration = Duration::from_secs(15);

/// Only these two public fields may be displayed or copied to the clipboard.
pub struct Prompt {
    pub user_code: String,
    pub verification_uri: String,
}

#[derive(Deserialize)]
pub(super) struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
}

#[derive(Deserialize)]
struct Challenge {
    user_code: String,
    verification_uri: String,
    device_code: String,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "登录已取消");
    Ok(())
}

fn verification_uri(value: &str) -> Result<String> {
    let url = Url::parse(value).context("微软返回的验证页地址无效")?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && matches!(
                url.host_str(),
                Some(
                    "www.microsoft.com"
                        | "microsoft.com"
                        | "login.microsoftonline.com"
                        | "login.live.com"
                )
            ),
        "微软返回的验证页地址无效"
    );
    Ok(url.into())
}

fn wait_to_poll(interval: Duration, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    let start = Instant::now();
    let next = start + interval.min(deadline.saturating_duration_since(start));
    loop {
        check_cancel(cancel)?;
        let now = Instant::now();
        ensure!(now < deadline, "登录代码已过期，请重新登录");
        if now >= next {
            return Ok(());
        }
        std::thread::sleep((next - now).min(Duration::from_millis(100)));
    }
}

pub(super) fn authorize(
    client_id: &str,
    cancel: &AtomicBool,
    show: impl FnOnce(Prompt) -> Result<()>,
) -> Result<Tokens> {
    authorize_with(&http_client()?, AUTHORITY, client_id, cancel, show)
}

fn http_client() -> Result<Client> {
    Ok(crate::download::client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()?)
}

pub(super) fn refresh(client_id: &str, previous: &str) -> Result<Tokens> {
    refresh_with(&http_client()?, AUTHORITY, client_id, previous)
}

fn refresh_with(http: &Client, authority: &str, client_id: &str, previous: &str) -> Result<Tokens> {
    let mut tokens: Tokens = http
        .post(format!("{authority}/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", previous),
            ("scope", SCOPE),
        ])
        .send()?
        .error_for_status()?
        .json()?;
    if tokens.refresh_token.is_empty() {
        tokens.refresh_token = previous.to_owned();
    }
    Ok(tokens)
}

fn authorize_with(
    http: &Client,
    authority: &str,
    client_id: &str,
    cancel: &AtomicBool,
    show: impl FnOnce(Prompt) -> Result<()>,
) -> Result<Tokens> {
    check_cancel(cancel)?;
    let response = http
        .post(format!("{authority}/devicecode"))
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .context("无法连接微软登录服务")?;
    ensure!(
        response.status().is_success(),
        "微软未能签发设备代码（HTTP {}），请检查发布应用的公共客户端配置",
        response.status()
    );
    let challenge: Challenge = response.json().context("微软返回的设备代码无效")?;
    ensure!(
        !challenge.device_code.is_empty()
            && !challenge.user_code.trim().is_empty()
            && challenge.expires_in > 0,
        "微软返回了无效的登录代码"
    );
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(challenge.expires_in))
        .context("登录代码有效期无效")?;
    let mut interval = Duration::from_secs(challenge.interval.unwrap_or(5).max(1));
    check_cancel(cancel)?;
    show(Prompt {
        user_code: challenge.user_code,
        verification_uri: verification_uri(&challenge.verification_uri)?,
    })?;
    let token_url = format!("{authority}/token");
    loop {
        wait_to_poll(interval, deadline, cancel)?;
        let response = http
            .post(&token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", challenge.device_code.as_str()),
            ])
            .timeout(TIMEOUT.min(deadline.saturating_duration_since(Instant::now())))
            .send();
        check_cancel(cancel)?;
        let response = match response {
            Err(e) if e.is_timeout() || e.is_connect() || e.is_request() => {
                interval = interval.saturating_mul(2);
                continue;
            }
            other => other.context("无法查询微软授权结果")?,
        };
        let status = response.status();
        if status.is_success() {
            return response.json().context("微软返回的登录会话无效");
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            interval = interval
                .saturating_add(Duration::from_secs(5))
                .max(Duration::from_secs(retry.unwrap_or_default()));
            continue;
        }
        let error: OAuthError = response.json().context("微软授权请求失败")?;
        match error.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval = interval.saturating_add(Duration::from_secs(5)),
            "authorization_declined" | "access_denied" => bail!("微软授权已被拒绝，可重新登录"),
            "expired_token" | "bad_verification_code" => bail!("登录代码已失效，请重新登录"),
            _ => bail!("微软设备授权失败，请重新登录（HTTP {status}）"),
        }
    }
}
