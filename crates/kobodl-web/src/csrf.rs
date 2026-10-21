//! Protection against cross-site request forgery.
//!
//! The app has no login of its own; a reverse proxy in front of it handles that. Browsers attach the proxy's
//! credentials to requests that other sites make, so every state-changing request has to prove it came from our own
//! pages.

use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub async fn reject_cross_site_requests(request: Request, next: Next) -> Response {
    let safe_method = matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe_method && !is_same_origin(request.headers()) {
        tracing::warn!(method = %request.method(), uri = %request.uri(), "rejected cross-site request");
        return (StatusCode::FORBIDDEN, "Cross-site request rejected.").into_response();
    }
    next.run(request).await
}

fn is_same_origin(headers: &HeaderMap) -> bool {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());

    // All current browsers send Sec-Fetch-Site. "none" means the user typed the URL or used a bookmark.
    if let Some(site) = header("sec-fetch-site") {
        return site == "same-origin" || site == "none";
    }

    // Older browsers: compare the Origin header with the host the request was sent to. Requests with neither header
    // don't come from a browser, so they can't carry the proxy's credentials without the client knowing them.
    let Some(origin) = header(header::ORIGIN.as_str()) else {
        return true;
    };
    let Some(origin_host) = origin.split_once("://").map(|(_, host)| host) else {
        return false;
    };
    let request_host = header("x-forwarded-host").or_else(|| header(header::HOST.as_str()));
    request_host.is_some_and(|host| host.eq_ignore_ascii_case(origin_host))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| (name.parse().unwrap(), HeaderValue::from_static(value)))
            .collect()
    }

    #[test]
    fn checks_sec_fetch_site_first() {
        assert!(is_same_origin(&headers(&[("sec-fetch-site", "same-origin")])));
        assert!(is_same_origin(&headers(&[("sec-fetch-site", "none")])));
        assert!(!is_same_origin(&headers(&[("sec-fetch-site", "cross-site")])));
        assert!(!is_same_origin(&headers(&[("sec-fetch-site", "same-site")])));
        assert!(!is_same_origin(&headers(&[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://books.example.com"),
            ("host", "books.example.com"),
        ])));
    }

    #[test]
    fn falls_back_to_origin() {
        assert!(is_same_origin(&headers(&[])));
        assert!(is_same_origin(&headers(&[
            ("origin", "https://books.example.com"),
            ("host", "books.example.com")
        ])));
        assert!(is_same_origin(&headers(&[
            ("origin", "https://books.example.com"),
            ("host", "127.0.0.1:8080"),
            ("x-forwarded-host", "books.example.com"),
        ])));
        assert!(!is_same_origin(&headers(&[
            ("origin", "https://evil.example"),
            ("host", "books.example.com")
        ])));
        assert!(!is_same_origin(&headers(&[
            ("origin", "null"),
            ("host", "books.example.com")
        ])));
    }
}
