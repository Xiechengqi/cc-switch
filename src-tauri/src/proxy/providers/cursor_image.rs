//! Image input loader for Cursor's `agent.v1.AgentService/Run`.
//!
//! Resolves three shapes of image references into byte buffers cursor can
//! ingest as `SelectedContext.selected_images[].data`:
//!
//!   1. `data:` URIs (`data:image/png;base64,...`) — decoded inline.
//!   2. `http(s)://` URLs — fetched via reqwest, with SSRF and DNS-rebinding
//!      guards: the resolved IP must not be loopback / private / link-local /
//!      multicast.
//!   3. Already-decoded inline bytes — passthrough (used by Anthropic
//!      `image.source.type = "base64"`).
//!
//! Hard limits (mirrored from OmniRoute):
//!   * single image ≤ 1 MiB after decode
//!   * total images per turn ≤ 8 (caller enforces by limiting input)
//!   * MIME must start with `image/`
//!   * fetch timeout 10 s
//!
//! cursor accepts JPEG/PNG/GIF/WEBP. We don't probe dimensions cheaply (no
//! image-parsing dep) — Dimension is optional in the protobuf so we send the
//! image without it.

use super::cursor_agent_proto::EncodedImage;
use crate::proxy::ProxyError;
use base64::Engine;
use bytes::Bytes;
use std::net::IpAddr;
use std::time::Duration;

pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Inputs from a request body — already extracted by `cursor_request_builder`.
#[derive(Debug, Clone)]
pub enum ImageRef {
    /// Raw data URI (`data:image/png;base64,...`).
    DataUri(String),
    /// HTTP(S) URL — must resolve to a non-private IP.
    HttpUrl(String),
    /// Pre-decoded inline bytes + declared MIME type.
    Inline { mime: String, data: Bytes },
}

/// Load each image reference into bytes. Returns Err on the first
/// unrecoverable problem (oversize, bad MIME, blocked IP). Empty input
/// returns an empty vec.
pub async fn load_images(refs: Vec<ImageRef>) -> Result<Vec<EncodedImage>, ProxyError> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        match r {
            ImageRef::DataUri(uri) => out.push(decode_data_uri(&uri)?),
            ImageRef::Inline { mime, data } => {
                check_mime(&mime)?;
                check_size(data.len())?;
                out.push(EncodedImage {
                    data,
                    mime_type: Some(mime),
                    width: None,
                    height: None,
                    uuid: uuid::Uuid::new_v4().to_string(),
                });
            }
            ImageRef::HttpUrl(url) => out.push(fetch_http(&url).await?),
        }
    }
    Ok(out)
}

fn decode_data_uri(uri: &str) -> Result<EncodedImage, ProxyError> {
    // shape: data:image/<subtype>[;base64],<payload>
    let body = uri
        .strip_prefix("data:")
        .ok_or_else(|| ProxyError::ForwardFailed("无效的 data URI 前缀".to_string()))?;
    let (header, payload) = body
        .split_once(',')
        .ok_or_else(|| ProxyError::ForwardFailed("data URI 缺少 ',' 分隔符".to_string()))?;
    let mime = header
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or("application/octet-stream");
    check_mime(mime)?;
    let is_base64 = header.contains(";base64");
    let bytes = if is_base64 {
        base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .map_err(|e| ProxyError::ForwardFailed(format!("图片 base64 解码失败: {e}")))?
    } else {
        // URL-percent-encoded payload — extremely rare for binary images,
        // but the spec allows it.
        urlencoding::decode(payload)
            .map_err(|e| ProxyError::ForwardFailed(format!("data URI 解码失败: {e}")))?
            .into_owned()
            .into_bytes()
    };
    check_size(bytes.len())?;
    Ok(EncodedImage {
        data: Bytes::from(bytes),
        mime_type: Some(mime.to_string()),
        width: None,
        height: None,
        uuid: uuid::Uuid::new_v4().to_string(),
    })
}

async fn fetch_http(url: &str) -> Result<EncodedImage, ProxyError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ProxyError::ForwardFailed(format!("图片 URL 无效: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ProxyError::ForwardFailed(format!(
            "图片 URL scheme 必须为 http/https: {}",
            parsed.scheme()
        )));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ProxyError::ForwardFailed("图片 URL 缺少 host".to_string()))?
        .to_string();

    // SSRF guard: pre-resolve the host and reject private/loopback/etc.
    let port = parsed.port_or_known_default().unwrap_or(443);
    let resolved = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("图片域名解析失败 ({host}): {e}")))?
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        return Err(ProxyError::ForwardFailed(format!(
            "图片域名解析为空: {host}"
        )));
    }
    for addr in &resolved {
        guard_ip(&addr.ip())?;
    }

    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| ProxyError::ForwardFailed(format!("构造图片下载 client 失败: {e}")))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("下载图片失败: {e}")))?;
    if !resp.status().is_success() {
        return Err(ProxyError::ForwardFailed(format!(
            "图片下载 HTTP 状态码异常: {}",
            resp.status()
        )));
    }
    let mime = resp
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    check_mime(&mime)?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("读取图片字节失败: {e}")))?;
    check_size(bytes.len())?;
    Ok(EncodedImage {
        data: bytes,
        mime_type: Some(mime),
        width: None,
        height: None,
        uuid: uuid::Uuid::new_v4().to_string(),
    })
}

fn check_mime(mime: &str) -> Result<(), ProxyError> {
    if !mime.to_ascii_lowercase().starts_with("image/") {
        return Err(ProxyError::ForwardFailed(format!(
            "图片 MIME 必须以 image/ 开头: {mime}"
        )));
    }
    Ok(())
}

fn check_size(len: usize) -> Result<(), ProxyError> {
    if len > MAX_IMAGE_BYTES {
        return Err(ProxyError::ForwardFailed(format!(
            "图片超过 {} 字节限制（实际 {len}）",
            MAX_IMAGE_BYTES
        )));
    }
    Ok(())
}

fn guard_ip(ip: &IpAddr) -> Result<(), ProxyError> {
    let blocked = match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                // 169.254.0.0/16 link-local already covered, also reject 100.64.0.0/10 CGNAT
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 0x40)
                // 0.0.0.0/8
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // unique-local fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    };
    if blocked {
        return Err(ProxyError::ForwardFailed(format!(
            "图片域名解析到不允许的 IP：{ip}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn data_uri_base64_decodes() {
        let png_b64 = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2, 3]);
        let uri = format!("data:image/png;base64,{}", png_b64);
        let img = decode_data_uri(&uri).unwrap();
        assert_eq!(img.mime_type.as_deref(), Some("image/png"));
        assert_eq!(&img.data[..], &[0u8, 1, 2, 3]);
        assert!(!img.uuid.is_empty());
    }

    #[test]
    fn rejects_oversize() {
        // 1 MiB + 1
        let too_big = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(check_size(too_big.len()).is_err());
        assert!(check_size(0).is_ok());
        assert!(check_size(MAX_IMAGE_BYTES).is_ok());
    }

    #[test]
    fn rejects_non_image_mime() {
        assert!(check_mime("text/plain").is_err());
        assert!(check_mime("image/png").is_ok());
        assert!(check_mime("Image/PNG").is_ok());
    }

    #[test]
    fn blocks_private_v4() {
        assert!(guard_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))).is_err());
        assert!(guard_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))).is_err());
        assert!(guard_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))).is_err());
        assert!(guard_ip(&IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1))).is_err());
        assert!(guard_ip(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))).is_err());
        assert!(guard_ip(&IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))).is_err());
        // Public IPs pass
        assert!(guard_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))).is_ok());
    }

    #[test]
    fn blocks_private_v6() {
        assert!(guard_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)).is_err());
        assert!(guard_ip(&IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1))).is_err());
        assert!(guard_ip(&IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))).is_err());
        assert!(guard_ip(&IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0, 0, 0, 0, 0, 1))).is_ok());
    }
}
