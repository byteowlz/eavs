//! Opt-in browser transport policy for the operator listener, outside authentication.
//! CORS is not authorization: non-browser callers still require listener credentials.
use axum::http::{HeaderValue, Method};
use tower_http::cors::CorsLayer;

/// Build a credential-free policy from exact HTTP(S) origins. Reject wildcards,
/// opaque origins, userinfo and URL paths rather than silently widening access.
pub fn layer(origins: &[String]) -> Result<CorsLayer, &'static str> {
    let mut allowed = Vec::new();
    for origin in origins {
        let url = url::Url::parse(origin).map_err(|_| "Invalid server.cors_allowed_origins")?;
        if origin.contains('*')
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.origin().ascii_serialization() != *origin
        {
            return Err(
                "server.cors_allowed_origins must contain exact HTTP(S) origins without paths",
            );
        }
        allowed.push(HeaderValue::from_str(origin).map_err(|_| "Invalid CORS origin header")?);
    }
    Ok(CorsLayer::new()
        .allow_origin(allowed)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::ACCEPT,
            axum::http::HeaderName::from_static("x-eavs-token"),
            axum::http::HeaderName::from_static("x-provider"),
            axum::http::HeaderName::from_static("anthropic-version"),
            axum::http::HeaderName::from_static("anthropic-beta"),
        ])
        .expose_headers([
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderName::from_static("x-request-id"),
        ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, middleware, routing::any, Router};
    use tower::ServiceExt;

    fn app() -> Result<Router, &'static str> {
        Ok(Router::new()
            .route("/v1/models", any(|| async { "models" }))
            .route(
                "/v1/chat/completions",
                any(|| async {
                    axum::response::Sse::new(futures::stream::iter([Ok::<
                        _,
                        std::convert::Infallible,
                    >(
                        axum::response::sse::Event::default().data("synthetic"),
                    )]))
                }),
            )
            .layer(middleware::from_fn_with_state(
                crate::api::ListenerAuth::new("synthetic".to_string()),
                crate::api::require_listener_auth,
            ))
            .layer(layer(&["https://100.100.100.1:5200".to_string()])?))
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Behavioral assertions; fallible fixture setup uses ?"
    )]
    async fn preflight_bypasses_auth_but_actual_requests_do_not(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let origin = "https://100.100.100.1:5200";
        for path in ["/v1/models", "/v1/chat/completions"] {
            let response = app()?
                .oneshot(
                    Request::builder()
                        .method("OPTIONS")
                        .uri(path)
                        .header("origin", origin)
                        .header("access-control-request-method", "POST")
                        .header(
                            "access-control-request-headers",
                            "authorization,content-type,x-eavs-token",
                        )
                        .body(Body::empty())?,
                )
                .await?;
            assert!(response.status().is_success());
            assert_eq!(response.headers()["access-control-allow-origin"], origin);
            assert!(response.headers()["access-control-allow-headers"]
                .to_str()?
                .contains("authorization"));
            assert!(!response
                .headers()
                .contains_key("access-control-allow-credentials"));
            for method in ["GET", "POST"] {
                let response = app()?
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(path)
                            .header("origin", origin)
                            .body(Body::empty())?,
                    )
                    .await?;
                assert_eq!(response.status(), 401);
                assert_eq!(response.headers()["access-control-allow-origin"], origin);
            }
        }
        let response = app()?
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("origin", origin)
                    .header("authorization", "Bearer synthetic")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["access-control-allow-origin"], origin);
        let body = axum::body::to_bytes(response.into_body(), 1024).await?;
        assert!(std::str::from_utf8(&body)?.contains("data: synthetic"));
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Behavioral assertions; fallible fixture setup uses ?"
    )]
    async fn unapproved_origins_have_no_cors_permission() -> Result<(), Box<dyn std::error::Error>>
    {
        for origin in ["https://evil.example", "null", "https://100.100.100.1:5201"] {
            for method in ["OPTIONS", "GET"] {
                let response = app()?
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri("/v1/models")
                            .header("origin", origin)
                            .header("access-control-request-method", "GET")
                            .body(Body::empty())?,
                    )
                    .await?;
                assert!(!response
                    .headers()
                    .contains_key("access-control-allow-origin"));
                if method == "GET" {
                    assert_eq!(response.status(), 401);
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "Behavioral assertions; fallible fixture setup uses ?"
    )]
    async fn disabled_policy_and_auth_regression() -> Result<(), Box<dyn std::error::Error>> {
        // The pre-fix listener stack rejects browser preflight before routing.
        let protected = Router::new()
            .route("/v1/models", any(|| async { "models" }))
            .layer(middleware::from_fn_with_state(
                crate::api::ListenerAuth::new("synthetic".to_string()),
                crate::api::require_listener_auth,
            ));
        let preflight = || {
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/models")
                .header("origin", "https://100.100.100.1:5200")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
        };
        assert_eq!(protected.clone().oneshot(preflight()?).await?.status(), 401);
        let response = protected.layer(layer(&[])?).oneshot(preflight()?).await?;
        assert!(!response
            .headers()
            .contains_key("access-control-allow-origin"));
        for credential in ["Bearer wrong", "Bearer synthetic"] {
            let response = app()?
                .oneshot(
                    Request::builder()
                        .uri("/v1/models")
                        .header("authorization", credential)
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(
                response.status(),
                if credential == "Bearer wrong" {
                    401
                } else {
                    200
                }
            );
            assert!(!response
                .headers()
                .contains_key("access-control-allow-origin"));
        }
        Ok(())
    }

    #[test]
    fn rejects_unsafe_or_noncanonical_origins() {
        for origin in [
            "*",
            "https://*.example.com",
            "null",
            "https://host/",
            "https://host/path",
            "https://user@host",
            "https://host?x=1",
            "file:///tmp",
        ] {
            assert!(layer(&[origin.to_string()]).is_err());
        }
        assert!(layer(&[]).is_ok());
    }
}
