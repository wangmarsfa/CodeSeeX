use codeseex_core::WebSearchProxyMode;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr};

use super::extract::{bytes_have_binary_markers, decode_text_bytes};
use super::MAX_BYTES;

pub(super) fn user_agent() -> &'static str {
    "Mozilla/5.0 (compatible; CodeSeeX/1.0; +https://localhost)"
}

pub(super) fn web_client(proxy_mode: WebSearchProxyMode) -> reqwest::Client {
    apply_proxy_mode(reqwest::Client::builder(), proxy_mode)
        .http1_only()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("build web client")
}

pub(super) fn no_redirect_client(proxy_mode: WebSearchProxyMode) -> reqwest::Client {
    apply_proxy_mode(reqwest::Client::builder(), proxy_mode)
        .http1_only()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("build no-redirect web client")
}

fn apply_proxy_mode(
    builder: reqwest::ClientBuilder,
    proxy_mode: WebSearchProxyMode,
) -> reqwest::ClientBuilder {
    match proxy_mode {
        WebSearchProxyMode::System => {
            if let Some(proxy_url) = system_proxy_url() {
                if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
                    return builder.proxy(proxy);
                }
            }
            builder
        }
        WebSearchProxyMode::None => builder.no_proxy(),
    }
}

fn system_proxy_url() -> Option<String> {
    env_proxy_url().or_else(windows_internet_settings_proxy_url)
}

fn env_proxy_url() -> Option<String> {
    ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find_map(|value| normalize_proxy_server(&value))
}

#[cfg(windows)]
fn windows_internet_settings_proxy_url() -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
            KEY_READ,
        )
        .ok()?;
    let enabled = settings
        .get_value::<u32, _>("ProxyEnable")
        .ok()
        .or_else(|| {
            settings
                .get_value::<u64, _>("ProxyEnable")
                .ok()
                .and_then(|value| u32::try_from(value).ok())
        })?;
    if enabled == 0 {
        return None;
    }
    let server = settings.get_value::<String, _>("ProxyServer").ok()?;
    normalize_proxy_server(&server)
}

#[cfg(not(windows))]
fn windows_internet_settings_proxy_url() -> Option<String> {
    None
}

fn normalize_proxy_server(value: &str) -> Option<String> {
    let selected = select_proxy_server(value)?;
    let selected = selected.trim();
    if selected.is_empty() {
        return None;
    }
    if selected.contains("://") {
        return Some(selected.to_owned());
    }
    Some(format!("http://{selected}"))
}

fn select_proxy_server(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('=') {
        return Some(trimmed);
    }
    let entries = trimmed
        .split(';')
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim()))
        .collect::<Vec<_>>();
    for wanted in ["https", "http"] {
        if let Some((_, value)) = entries.iter().find(|(key, _)| key == wanted) {
            return Some(*value);
        }
    }
    entries.first().map(|(_, value)| *value)
}

pub(super) async fn fetch_text(client: &reqwest::Client, url: reqwest::Url, accept: &str) -> Value {
    let response = match client
        .get(url.clone())
        .header(reqwest::header::USER_AGENT, user_agent())
        .header(reqwest::header::ACCEPT, accept)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({
                "ok": false,
                "error": "request_failed",
                "url": url.as_str(),
                "message": request_error_message(&error)
            });
        }
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let (bytes, byte_truncated) = read_limited_response_bytes(response).await;
    if bytes_have_binary_markers(&bytes) {
        return json!({
            "ok": false,
            "error": "binary_response",
            "url": url.as_str(),
            "status": status,
            "content_type": content_type,
            "bytes": bytes.len()
        });
    }
    let (text, encoding, had_decode_errors) = decode_text_bytes(&bytes, &content_type);
    json!({
        "ok": (200..400).contains(&status),
        "url": url.as_str(),
        "status": status,
        "content_type": content_type,
        "encoding": encoding,
        "decode_errors": had_decode_errors,
        "text": text,
        "bytes": bytes.len(),
        "truncated": byte_truncated
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_plain_system_proxy_server() {
        assert_eq!(
            normalize_proxy_server("127.0.0.1:7890").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn normalizes_protocol_mapped_system_proxy_server() {
        assert_eq!(
            normalize_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7891").as_deref(),
            Some("http://127.0.0.1:7891")
        );
    }
}

pub(super) fn request_error_message(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

pub(super) async fn read_limited_response_bytes(response: reqwest::Response) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = usize::try_from(MAX_BYTES)
            .unwrap_or(usize::MAX)
            .saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    (bytes, truncated)
}
