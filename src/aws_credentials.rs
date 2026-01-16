use crate::aws_sigv4::AwsCredentials;
use crate::upstream::{Upstream, UpstreamRequest};
use bytes::Bytes;
use futures::StreamExt;
use http::Method;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AwsCredentialsWithExpiration {
    pub creds: AwsCredentials,
    #[allow(dead_code)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub fn default_shared_credentials_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AWS_SHARED_CREDENTIALS_FILE") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }

    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".aws").join("credentials"))
}

pub fn aws_profile() -> String {
    std::env::var("AWS_PROFILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "default".to_string())
}

pub fn load_shared_credentials(profile: &str, path: &Path) -> Option<AwsCredentials> {
    let contents = std::fs::read_to_string(path).ok()?;
    parse_aws_shared_credentials(profile, &contents)
}

fn parse_aws_shared_credentials(profile: &str, contents: &str) -> Option<AwsCredentials> {
    let wanted = profile.trim();
    if wanted.is_empty() {
        return None;
    }

    let mut current_profile: Option<String> = None;
    let mut access_key_id: Option<String> = None;
    let mut secret_access_key: Option<String> = None;
    let mut session_token: Option<String> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let section = section.trim();
            let section = section.strip_prefix("profile ").unwrap_or(section);
            current_profile = Some(section.to_string());
            access_key_id = None;
            secret_access_key = None;
            session_token = None;
            continue;
        }

        if current_profile.as_deref()? != wanted {
            continue;
        }

        let (k, v) = line.split_once('=')?;
        let key = k.trim();
        let val = v.trim().to_string();

        match key {
            "aws_access_key_id" => access_key_id = Some(val),
            "aws_secret_access_key" => secret_access_key = Some(val),
            "aws_session_token" => session_token = Some(val),
            _ => {}
        }
    }

    Some(AwsCredentials {
        access_key_id: access_key_id?,
        secret_access_key: secret_access_key?,
        session_token,
    })
}

pub async fn assume_role_with_web_identity(
    upstream: &dyn Upstream,
    role_arn: &str,
    web_identity_token: &str,
    role_session_name: &str,
) -> Result<AwsCredentialsWithExpiration, String> {
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("Action", "AssumeRoleWithWebIdentity");
        serializer.append_pair("Version", "2011-06-15");
        serializer.append_pair("RoleArn", role_arn);
        serializer.append_pair("RoleSessionName", role_session_name);
        serializer.append_pair("WebIdentityToken", web_identity_token);
        serializer.finish()
    };

    let req = UpstreamRequest {
        method: Method::POST,
        url: "https://sts.amazonaws.com/".to_string(),
        headers: {
            let mut h = http::HeaderMap::new();
            h.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/x-www-form-urlencoded"),
            );
            h
        },
        body: Bytes::from(body),
    };

    let mut resp = upstream
        .send(req)
        .await
        .map_err(|e| format!("STS request failed: {}", e))?;

    let mut collected = Vec::new();
    while let Some(chunk) = resp.body.next().await {
        let chunk = chunk.map_err(|e| format!("STS body read failed: {}", e))?;
        collected.extend_from_slice(&chunk);
        if collected.len() > 1024 * 1024 {
            return Err("STS response exceeded 1MiB limit".to_string());
        }
    }

    let xml = String::from_utf8_lossy(&collected).to_string();

    if !resp.status.is_success() {
        // Try to surface the most relevant message.
        return Err(format!("STS returned {}: {}", resp.status, xml));
    }

    let access_key_id =
        xml_tag_value(&xml, "AccessKeyId").ok_or_else(|| "missing AccessKeyId".to_string())?;
    let secret_access_key = xml_tag_value(&xml, "SecretAccessKey")
        .ok_or_else(|| "missing SecretAccessKey".to_string())?;
    let session_token =
        xml_tag_value(&xml, "SessionToken").ok_or_else(|| "missing SessionToken".to_string())?;
    let expires_at = xml_tag_value(&xml, "Expiration")
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    Ok(AwsCredentialsWithExpiration {
        creds: AwsCredentials {
            access_key_id,
            secret_access_key,
            session_token: Some(session_token),
        },
        expires_at,
    })
}

fn xml_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::UpstreamResponse;
    use futures::stream;
    use futures::StreamExt;
    use http::StatusCode;

    #[test]
    fn parse_shared_credentials_profile() {
        let contents = r#"
            [default]
            aws_access_key_id = AKIADEFAULT
            aws_secret_access_key = SECRETDEFAULT

            [work]
            aws_access_key_id=AKIAWORK
            aws_secret_access_key=SECRETWORK
            aws_session_token=TOKENWORK
        "#;

        let creds = super::parse_aws_shared_credentials("work", contents).unwrap();
        assert_eq!(creds.access_key_id, "AKIAWORK");
        assert_eq!(creds.secret_access_key, "SECRETWORK");
        assert_eq!(creds.session_token.as_deref(), Some("TOKENWORK"));
    }

    #[test]
    fn xml_tag_value_extracts() {
        let xml = "<Root><AccessKeyId>A</AccessKeyId></Root>";
        assert_eq!(
            super::xml_tag_value(xml, "AccessKeyId").as_deref(),
            Some("A")
        );
    }

    #[tokio::test]
    async fn assume_role_with_web_identity_parses_sts_xml() {
        #[derive(Clone)]
        struct TestUpstream;

        impl crate::upstream::Upstream for TestUpstream {
            fn send<'a>(
                &'a self,
                _request: UpstreamRequest,
            ) -> futures::future::BoxFuture<'a, Result<UpstreamResponse, std::io::Error>>
            {
                Box::pin(async move {
                    let xml = r#"
                        <AssumeRoleWithWebIdentityResponse>
                          <AssumeRoleWithWebIdentityResult>
                            <Credentials>
                              <AccessKeyId>AKIA123</AccessKeyId>
                              <SecretAccessKey>SECRET123</SecretAccessKey>
                              <SessionToken>TOKEN123</SessionToken>
                              <Expiration>2025-01-01T00:00:00Z</Expiration>
                            </Credentials>
                          </AssumeRoleWithWebIdentityResult>
                        </AssumeRoleWithWebIdentityResponse>
                    "#;
                    Ok(UpstreamResponse {
                        status: StatusCode::OK,
                        headers: http::HeaderMap::new(),
                        body: stream::iter(vec![Ok(Bytes::from_static(xml.as_bytes()))]).boxed(),
                    })
                })
            }
        }

        let res =
            assume_role_with_web_identity(&TestUpstream, "arn:aws:iam::123:role/r", "jwt", "sess")
                .await
                .unwrap();
        assert_eq!(res.creds.access_key_id, "AKIA123");
        assert_eq!(res.creds.secret_access_key, "SECRET123");
        assert_eq!(res.creds.session_token.as_deref(), Some("TOKEN123"));
        assert!(res.expires_at.is_some());
    }
}
