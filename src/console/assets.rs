//! Embedded console frontend assets.
//!
//! The UI is a dependency-free single-page app (vanilla JS + CSS) so the whole
//! console ships inside the binary: `divertlink console` works on Windows,
//! macOS, Linux and in a browser over SSH forwarding.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::api::asset_response;

/// `index.html`
pub const INDEX_HTML: &str = include_str!("../../assets/console/index.html");
/// `app.js`
pub const APP_JS: &str = include_str!("../../assets/console/app.js");
/// `style.css`
pub const STYLE_CSS: &str = include_str!("../../assets/console/style.css");
/// `manifest.webmanifest`
pub const MANIFEST: &str = include_str!("../../assets/console/manifest.webmanifest");
/// `sw.js`
pub const SERVICE_WORKER: &str = include_str!("../../assets/console/sw.js");
/// `icon.svg`
pub const ICON_SVG: &str = include_str!("../../assets/console/icon.svg");

pub async fn index() -> Response {
    asset_response("text/html; charset=utf-8", INDEX_HTML, false)
}

pub async fn app_js() -> Response {
    asset_response("application/javascript; charset=utf-8", APP_JS, false)
}

pub async fn style_css() -> Response {
    asset_response("text/css; charset=utf-8", STYLE_CSS, false)
}

pub async fn manifest() -> Response {
    asset_response("application/manifest+json; charset=utf-8", MANIFEST, true)
}

pub async fn service_worker() -> Response {
    asset_response(
        "application/javascript; charset=utf-8",
        SERVICE_WORKER,
        false,
    )
}

pub async fn icon() -> Response {
    asset_response("image/svg+xml", ICON_SVG, true)
}

pub async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [("content-type", "text/plain; charset=utf-8")],
        "not found\n",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_embedded() {
        assert!(INDEX_HTML.contains("DivertLink"));
        assert!(APP_JS.contains("function"));
        assert!(STYLE_CSS.contains("--"));
        assert!(MANIFEST.contains("name"));
        assert!(SERVICE_WORKER.contains("addEventListener"));
        assert!(ICON_SVG.contains("<svg"));
    }
}
