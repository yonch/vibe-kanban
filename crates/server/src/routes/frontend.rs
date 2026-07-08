use axum::{
    body::Body,
    http::HeaderValue,
    response::{IntoResponse, Response},
};
use reqwest::{StatusCode, header};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../packages/local-web/dist"]
struct Assets;

const IMMUTABLE_ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const HTML_CACHE_CONTROL: &str = "no-cache";

pub(super) async fn serve_frontend(uri: axum::extract::Path<String>) -> impl IntoResponse {
    let path = uri.trim_start_matches('/');
    serve_file(path).await
}

pub(super) async fn serve_frontend_root() -> impl IntoResponse {
    serve_file("index.html").await
}

async fn serve_file(path: &str) -> impl IntoResponse + use<> {
    let file = Assets::get(path);

    match file {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();

            Response::builder()
                .status(StatusCode::OK)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(mime.as_ref()).unwrap(),
                )
                .header(header::CACHE_CONTROL, cache_control_for_path(path))
                .body(Body::from(content.data.into_owned()))
                .unwrap()
        }
        None => {
            // For SPA routing, serve index.html for unknown routes
            if let Some(index) = Assets::get("index.html") {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, HeaderValue::from_static("text/html"))
                    .header(header::CACHE_CONTROL, HTML_CACHE_CONTROL)
                    .body(Body::from(index.data.into_owned()))
                    .unwrap()
            } else {
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("404 Not Found"))
                    .unwrap()
            }
        }
    }
}

fn cache_control_for_path(path: &str) -> &'static str {
    if path == "index.html" {
        HTML_CACHE_CONTROL
    } else if path.starts_with("assets/") {
        IMMUTABLE_ASSET_CACHE_CONTROL
    } else {
        "public, max-age=3600"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_vite_fingerprinted_assets_immutably() {
        assert_eq!(
            cache_control_for_path("assets/index-Dcn48dHC.js"),
            IMMUTABLE_ASSET_CACHE_CONTROL
        );
    }

    #[test]
    fn keeps_html_revalidating_for_app_updates() {
        assert_eq!(cache_control_for_path("index.html"), HTML_CACHE_CONTROL);
    }

    #[test]
    fn caches_public_assets_briefly() {
        assert_eq!(
            cache_control_for_path("favicon.png"),
            "public, max-age=3600"
        );
    }
}
