use bytes::Bytes;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, StreamExt};
use http::{HeaderMap, Method, StatusCode};
use std::time::Duration;

pub struct UpstreamRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

pub struct UpstreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: BoxStream<'static, Result<Bytes, std::io::Error>>,
}

pub trait Upstream: Send + Sync {
    fn send<'a>(
        &'a self,
        request: UpstreamRequest,
    ) -> BoxFuture<'a, Result<UpstreamResponse, std::io::Error>>;

    fn get_bytes<'a>(
        &'a self,
        url: &'a str,
        timeout: Duration,
        max_bytes: usize,
    ) -> BoxFuture<'a, Result<(HeaderMap, Bytes), std::io::Error>> {
        Box::pin(async move {
            let req = UpstreamRequest {
                method: Method::GET,
                url: url.to_string(),
                headers: HeaderMap::new(),
                body: Bytes::new(),
            };

            let mut resp = tokio::time::timeout(timeout, self.send(req))
                .await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"))??;

            if !resp.status.is_success() {
                return Err(std::io::Error::other(format!(
                    "fetch returned {}",
                    resp.status
                )));
            }

            let mut collected = Vec::new();
            while let Some(chunk) = resp.body.next().await {
                let chunk = chunk?;
                if collected.len() + chunk.len() > max_bytes {
                    return Err(std::io::Error::other(format!(
                        "body exceeds {} bytes limit",
                        max_bytes
                    )));
                }
                collected.extend_from_slice(&chunk);
            }

            Ok((resp.headers, Bytes::from(collected)))
        })
    }
}

pub struct ReqwestUpstream {
    client: reqwest::Client,
}

impl ReqwestUpstream {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Upstream for ReqwestUpstream {
    fn send<'a>(
        &'a self,
        request: UpstreamRequest,
    ) -> BoxFuture<'a, Result<UpstreamResponse, std::io::Error>> {
        Box::pin(async move {
            let method =
                reqwest::Method::from_bytes(request.method.as_str().as_bytes()).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
                })?;

            let mut builder = self.client.request(method.clone(), &request.url);
            builder = builder.headers(request.headers.clone());

            // Only add body for methods that support it and when body is non-empty.
            // GET, HEAD, OPTIONS, and TRACE should not have a body per HTTP spec.
            // Some providers (like OpenAI) reject GET requests with a body.
            let should_have_body = !matches!(
                method,
                reqwest::Method::GET
                    | reqwest::Method::HEAD
                    | reqwest::Method::OPTIONS
                    | reqwest::Method::TRACE
            );
            if should_have_body && !request.body.is_empty() {
                builder = builder.body(request.body);
            }

            let resp = builder.send().await.map_err(std::io::Error::other)?;

            let status = resp.status();
            let headers = resp.headers().clone();
            let body = resp
                .bytes_stream()
                .map(|res| res.map_err(std::io::Error::other))
                .boxed();

            Ok(UpstreamResponse {
                status,
                headers,
                body,
            })
        })
    }
}
