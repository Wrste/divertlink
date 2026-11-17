//! Pooled HTTP client used for local model targets and static upstream routes.

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

use crate::error::{Error, Result};

/// The concrete pooled HTTP client type shared by the agent and the gateway.
pub type HttpClient = Client<HttpsConnector<HttpConnector>, Body>;

/// Build a pooled HTTP/1.1 client with an optional extra CA bundle.
///
/// HTTP and HTTPS URLs are both accepted (`https_or_http`); system/`webpki` roots
/// are used for HTTPS unless `extra_ca` adds a private CA.
pub fn build_http_client(extra_ca: Option<&Path>, connect_timeout: Duration) -> Result<HttpClient> {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(connect_timeout));
    connector.set_nodelay(true);
    connector.enforce_http(false);

    let tls_config = crate::tls::build_https_client_config(extra_ca)?;
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config((*tls_config).clone())
        .https_or_http()
        .enable_http1()
        .wrap_connector(connector);

    Ok(Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(32)
        .build(https))
}

/// Send one request to an upstream and return the streaming response.
pub async fn send_request(
    client: &HttpClient,
    request: axum::http::Request<Body>,
) -> Result<axum::http::Response<hyper::body::Incoming>> {
    client
        .request(request)
        .await
        .map_err(|e| Error::Internal(format!("upstream request failed: {e}")))
}

/// Send one request with a header timeout.
pub async fn send_request_with_timeout(
    client: &HttpClient,
    request: axum::http::Request<Body>,
    timeout: Duration,
) -> Result<axum::http::Response<hyper::body::Incoming>> {
    tokio::time::timeout(timeout, send_request(client, request))
        .await
        .map_err(|_| Error::Timeout("upstream response header timeout".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builds_client_for_http_and_https() {
        let _client = build_http_client(None, Duration::from_secs(5)).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let report = crate::tls::generate_certs(&crate::tls::GenCertsOptions {
            dir: tmp.path().to_path_buf(),
            domain: "localhost".into(),
            client_cert: false,
            self_signed: true,
            force: false,
        })
        .unwrap();
        let _with_ca = build_http_client(Some(&report.ca_cert), Duration::from_secs(5)).unwrap();
    }
}
