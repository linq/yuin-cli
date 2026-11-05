use anyhow::{Result, ensure};
use reqwest::{Client, Response};
use std::time::Duration;

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("Mozilla/5.0 Yuin-CLI/0.1")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}

pub async fn bytes(mut response: Response, max: usize) -> Result<Vec<u8>> {
    ensure!(
        response.status().is_success(),
        "HTTP request failed ({})",
        response.status().as_u16()
    );
    ensure!(
        response.content_length().is_none_or(|n| n <= max as u64),
        "HTTP response is too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= max,
            "HTTP response is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub fn url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "Expected an HTTP(S) URL without embedded credentials"
    );
    Ok(url)
}
