use hmac::{Hmac, Mac};
use http::{HeaderMap, Method};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn sign_request_headers(
    headers: &mut HeaderMap,
    method: &Method,
    url: &str,
    body: &[u8],
    region: &str,
    service: &str,
    now: chrono::DateTime<chrono::Utc>,
    creds: &AwsCredentials,
) -> Result<(), String> {
    let url = url::Url::parse(url).map_err(|e| format!("invalid url: {}", e))?;

    let host = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{}:{}", h, p),
        (Some(h), None) => h.to_string(),
        (None, _) => return Err("missing host".to_string()),
    };

    headers.insert(
        http::header::HOST,
        http::HeaderValue::from_str(&host).map_err(|e| e.to_string())?,
    );

    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();

    headers.insert(
        http::header::HeaderName::from_static("x-amz-date"),
        http::HeaderValue::from_str(&amz_date).map_err(|e| e.to_string())?,
    );

    let payload_hash = hex::encode(Sha256::digest(body));
    headers.insert(
        http::header::HeaderName::from_static("x-amz-content-sha256"),
        http::HeaderValue::from_str(&payload_hash).map_err(|e| e.to_string())?,
    );

    if let Some(token) = &creds.session_token {
        if !token.is_empty() {
            headers.insert(
                http::header::HeaderName::from_static("x-amz-security-token"),
                http::HeaderValue::from_str(token).map_err(|e| e.to_string())?,
            );
        }
    }

    // Canonical request pieces.
    let canonical_uri = if url.path().is_empty() { "/" } else { url.path() };
    let canonical_query = canonical_query_string(&url);

    let (canonical_headers, signed_headers) = canonical_headers(headers);

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        canonical_uri,
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );

    let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let credential_scope = format!("{}/{}/{}/aws4_request", date, region, service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date, credential_scope, canonical_request_hash
    );

    let signing_key = derive_signing_key(creds, &date, region, service);

    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    if creds.access_key_id.is_empty() {
        return Err("missing access key id".to_string());
    }

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        creds.access_key_id, credential_scope, signed_headers, signature
    );

    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_str(&authorization).map_err(|e| e.to_string())?,
    );

    Ok(())
}

fn canonical_query_string(url: &url::Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (aws_encode(&k), aws_encode(&v)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

fn canonical_headers(headers: &HeaderMap) -> (String, String) {
    let mut items: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            let value = value.to_str().ok()?.trim();
            Some((name, collapse_spaces(value)))
        })
        .collect();

    items.sort_by(|a, b| a.0.cmp(&b.0));

    let signed_headers = items
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical = items
        .into_iter()
        .map(|(k, v)| format!("{}:{}\n", k, v))
        .collect::<String>();

    (canonical, signed_headers)
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim().to_string()
}

fn aws_encode(input: &str) -> String {
    url::form_urlencoded::byte_serialize(input.as_bytes()).collect::<String>().replace('+', "%20")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn derive_signing_key(creds: &AwsCredentials, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_secret = format!("AWS4{}", creds.secret_access_key);
    let k_date = hmac_sha256(k_secret.as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn canonical_query_sort_and_encode() {
        let url = url::Url::parse("https://example.com/test?b=2&a=hello%20world").unwrap();
        assert_eq!(canonical_query_string(&url), "a=hello%20world&b=2");
    }

    #[test]
    fn header_canonicalization() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Test", HeaderValue::from_static("  a   b "));
        headers.insert("host", HeaderValue::from_static("example.com"));
        let (canon, signed) = canonical_headers(&headers);
        assert!(canon.contains("host:example.com\n"));
        assert!(canon.contains("x-test:a b\n"));
        assert_eq!(signed, "host;x-test");
    }
}
