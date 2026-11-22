//! The embedded web frontend (see `build.rs`) with an SPA fallback to `index.html`, or a
//! fallback page when the binary was built without `web/dist`.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// Whether this binary carries the web frontend.
pub fn embedded() -> bool {
    !ASSETS.is_empty()
}

fn find(path: &str) -> Option<&'static [u8]> {
    ASSETS
        .binary_search_by(|(p, _)| (*p).cmp(path))
        .ok()
        .and_then(|i| ASSETS.get(i))
        .map(|(_, b)| *b)
}

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// What a non-API path resolves to.
#[derive(Debug, PartialEq, Eq)]
enum Pick<'a> {
    /// An embedded file.
    File(&'a str),
    /// A client-side route: the SPA's `index.html` (or the fallback page).
    Index,
    NotFound,
}

/// Whether `path` names a file rather than a client-side route: anything under `assets/`,
/// or a last segment with an extension (`favicon.ico`, `logo.png`).
fn is_file_like(path: &str) -> bool {
    path.starts_with("assets/") || path.rsplit('/').next().is_some_and(|s| s.contains('.'))
}

/// Resolve a path (without the leading `/`). Missing files are 404; only route-like paths
/// fall back to `index.html`, so a stale or mistyped asset URL is never answered with HTML.
fn pick<'a>(path: &'a str, exists: impl Fn(&str) -> bool) -> Pick<'a> {
    if path.is_empty() {
        Pick::Index
    } else if exists(path) {
        Pick::File(path)
    } else if is_file_like(path) {
        Pick::NotFound
    } else {
        Pick::Index
    }
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "not found",
    )
        .into_response()
}

fn file_response(path: &str, body: &'static [u8]) -> Response {
    let cache = if path.starts_with("assets/") {
        // Vite fingerprints everything under assets/.
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut res = body.into_response();
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    res
}

/// Serve `uri` from the embedded frontend.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    match pick(path, |p| find(p).is_some()) {
        Pick::NotFound => not_found(),
        Pick::File(p) => match find(p) {
            Some(b) => file_response(p, b),
            None => not_found(),
        },
        Pick::Index => match find("index.html") {
            Some(b) => file_response("index.html", b),
            None => (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                FALLBACK_PAGE,
            )
                .into_response(),
        },
    }
}

/// Served for every non-API path when the frontend was not embedded.
pub const FALLBACK_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>cartograph</title>
<style>
  body { font: 16px/1.5 system-ui, sans-serif; max-width: 42rem; margin: 4rem auto; padding: 0 1rem; color: #1d1d1f; background: #fafafa; }
  code, pre { font-family: ui-monospace, Consolas, monospace; }
  pre { background: #eee; padding: .75rem 1rem; border-radius: 6px; overflow-x: auto; }
  @media (prefers-color-scheme: dark) { body { color: #e8e8ea; background: #161618; } pre { background: #26262a; } }
</style>
</head>
<body>
<h1>cartograph</h1>
<p>This binary was built without the web frontend, so there is no map to show yet. The API works: try <a href="/api/meta"><code>/api/meta</code></a>.</p>
<p>To embed the frontend, build it and then rebuild cartograph:</p>
<pre>npm --prefix web ci &amp;&amp; npm --prefix web run build
cargo build --release</pre>
<p>If cargo does not pick up the new <code>web/dist</code>, touch <code>crates/cartograph-server/build.rs</code> first.</p>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_served_missing_files_are_404_and_routes_get_the_spa() {
        let have = |p: &str| ["index.html", "assets/app-1.js", "favicon.ico"].contains(&p);
        assert_eq!(pick("", have), Pick::Index);
        assert_eq!(pick("assets/app-1.js", have), Pick::File("assets/app-1.js"));
        assert_eq!(pick("favicon.ico", have), Pick::File("favicon.ico"));
        assert_eq!(pick("assets/app-2.js", have), Pick::NotFound);
        assert_eq!(pick("assets/chunk", have), Pick::NotFound);
        assert_eq!(pick("img/logo.png", have), Pick::NotFound);
        assert_eq!(pick("map", have), Pick::Index);
        assert_eq!(pick("map/dir/src", have), Pick::Index);
    }
}
