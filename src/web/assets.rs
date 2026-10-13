//! The files of the interface, and how they are served; see `docs/web-ui.md` §9.2.
//!
//! The interface is a single-page app that `npm run build` writes to `web/dist/`. The
//! program carries that folder with it (`rust-embed`: in a release build the files are
//! inside the binary, in a debug build they are read from the disk, so rebuilding the
//! interface does not mean rebuilding the program) and serves it at `/`. Nothing else is
//! served: a path that is not a file of the interface is a 404, and the paths that could
//! lead out of the folder or to something hidden are 404 before anything is asked of it.
//!
//! A program built before the interface was (`web/dist/` is empty in a fresh checkout)
//! has no `index.html`, and `/` says how to build one instead.

use std::borrow::Cow;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

/// The page of the interface.
const INDEX: &str = "index.html";

/// What `/` says when there is no interface in the program.
const PLACEHOLDER: &str = "<!doctype html>\n\
<meta charset=\"utf-8\">\n\
<title>Tang</title>\n\
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:36rem;margin:4rem auto;padding:0 1rem}</style>\n\
<h1>Tang</h1>\n\
<p>The server is running, but this program was built without the web interface. Build it \
(<code>npm ci &amp;&amp; npm run build</code> in <code>web/</code>) and, for a release build, \
build the program again; or run <code>npm run dev</code> there, which serves the interface \
and talks to this server.</p>\n";

/// What the placeholder may do: nothing but look like this, and never be framed.
const PLACEHOLDER_POLICY: &str =
    "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// What the interface may do: run its own scripts and styles, load its own files and talk to
/// the server it came from, and never be framed, nor load anything from anywhere else.
const APP_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Where the files come from: the build of the interface, or a stand-in in a test.
pub trait Assets: Send + Sync + 'static {
    /// The file at `path` (relative, no leading slash), if there is one.
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>>;
}

#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist"]
struct Dist;

/// The files of `web/dist`: as they were when the program was built, or in a debug build as
/// they are.
pub struct Embedded;

impl Assets for Embedded {
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        Dist::get(path).map(|file| file.data)
    }
}

/// The answer to a GET of `path` (`/`, `/assets/index-1a2b3c.js`).
pub fn serve(assets: &dyn Assets, path: &str) -> Response {
    let relative = path.trim_start_matches('/');
    if relative.is_empty() || relative == INDEX {
        return match assets.get(INDEX) {
            Some(body) => file(INDEX, body),
            None => placeholder(),
        };
    }
    if !stays_inside(relative) {
        return not_found();
    }
    match assets.get(relative) {
        Some(body) => file(relative, body),
        None => not_found(),
    }
}

/// Whether `path` is one that may be asked of the folder at all: no hidden file or folder
/// (the folder keeps a `.gitkeep`), no `..`, no empty part, nothing a file system reads
/// differently from a URL.
fn stays_inside(path: &str) -> bool {
    !path.contains(['\\', '\0']) && path.split('/').all(|part| !part.is_empty() && !part.starts_with('.'))
}

fn file(path: &str, body: Cow<'static, [u8]>) -> Response {
    let mut response = (StatusCode::OK, [(header::CONTENT_TYPE, content_type(path))], body.into_owned()).into_response();
    let headers = response.headers_mut();
    if path.ends_with(".html") {
        // The page is looked at again every time (it is small, and names the files that
        // follow); and it is the document the policy is for.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(APP_POLICY));
    } else if path.starts_with("assets/") {
        // The build names these files by what is in them, so a name is never reused.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    } else {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    response
}

fn placeholder() -> Response {
    ([(header::CONTENT_SECURITY_POLICY, PLACEHOLDER_POLICY)], Html(PLACEHOLDER)).into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Nothing is served there.\n").into_response()
}

/// What a file of the interface is, by its name.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// A few files to serve, for the tests of the server and of this module.
#[cfg(test)]
pub struct Files(pub std::collections::HashMap<&'static str, &'static [u8]>);

#[cfg(test)]
impl Files {
    /// A built interface, as small as one gets, with a hidden file and a hidden folder that must never come out.
    pub fn interface() -> Files {
        Files(std::collections::HashMap::from([
            ("index.html", b"<!doctype html><title>Tang</title>".as_slice()),
            ("assets/index-abc123.js", b"console.log(1)".as_slice()),
            ("assets/index-abc123.css", b"body{}".as_slice()),
            ("assets/index-abc123.js.map", b"{}".as_slice()),
            ("favicon.svg", b"<svg/>".as_slice()),
            ("robots.txt", b"".as_slice()),
            (".gitkeep", b"".as_slice()),
            (".hidden/secret.txt", b"secret".as_slice()),
        ]))
    }

    /// A folder with nothing in it but the file that keeps it in a checkout.
    pub fn unbuilt() -> Files {
        Files(std::collections::HashMap::from([(".gitkeep", b"".as_slice())]))
    }
}

#[cfg(test)]
impl Assets for Files {
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        self.0.get(path).map(|body| Cow::Borrowed(*body))
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    fn app() -> Files {
        Files::interface()
    }

    async fn body(response: Response) -> String {
        String::from_utf8(to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
    }

    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response.headers().get(name).map(|value| value.to_str().unwrap())
    }

    #[tokio::test]
    async fn the_root_is_the_page_of_the_interface() {
        for path in ["/", "", "/index.html"] {
            let response = serve(&app(), path);
            assert_eq!(response.status(), StatusCode::OK, "{path:?}");
            assert_eq!(header(&response, "content-type"), Some("text/html; charset=utf-8"));
            assert_eq!(body(response).await, "<!doctype html><title>Tang</title>");
        }
    }

    #[test]
    fn the_page_is_looked_at_every_time_and_may_only_use_what_comes_from_here() {
        let response = serve(&app(), "/");
        assert_eq!(header(&response, "cache-control"), Some("no-cache"));
        let policy = header(&response, "content-security-policy").unwrap();
        for wanted in ["default-src 'self'", "script-src 'self'", "frame-ancestors 'none'", "object-src 'none'", "connect-src 'self'"] {
            assert!(policy.contains(wanted), "{wanted} is not in {policy}");
        }
        assert!(!policy.contains("unsafe"), "{policy}");
    }

    #[test]
    fn the_files_the_build_names_by_content_are_kept_for_ever() {
        for path in ["/assets/index-abc123.js", "/assets/index-abc123.css", "/assets/index-abc123.js.map"] {
            let response = serve(&app(), path);
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(header(&response, "cache-control"), Some("public, max-age=31536000, immutable"), "{path}");
        }
        // And the others are asked about again.
        assert_eq!(header(&serve(&app(), "/favicon.svg"), "cache-control"), Some("no-cache"));
    }

    #[test]
    fn a_file_is_what_its_name_says() {
        let cases = [
            ("/assets/index-abc123.js", "text/javascript; charset=utf-8"),
            ("/assets/index-abc123.css", "text/css; charset=utf-8"),
            ("/assets/index-abc123.js.map", "application/json"),
            ("/favicon.svg", "image/svg+xml"),
            ("/robots.txt", "text/plain; charset=utf-8"),
        ];
        for (path, expected) in cases {
            assert_eq!(header(&serve(&app(), path), "content-type"), Some(expected), "{path}");
        }
        for (name, expected) in [
            ("a.mjs", "text/javascript; charset=utf-8"),
            ("a.png", "image/png"),
            ("a.ico", "image/x-icon"),
            ("a.woff2", "font/woff2"),
            ("a.wasm", "application/wasm"),
            ("a.json", "application/json"),
            ("a.html", "text/html; charset=utf-8"),
            ("a.unknown", "application/octet-stream"),
            ("noextension", "application/octet-stream"),
        ] {
            assert_eq!(content_type(name), expected, "{name}");
        }
    }

    #[test]
    fn what_is_not_a_file_of_the_interface_is_not_found() {
        for path in ["/nothing", "/assets/missing.js", "/assets/", "/assets", "/api/session", "//assets//index-abc123.js", "/a/b/c.js"] {
            assert_eq!(serve(&app(), path).status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[test]
    fn hidden_files_and_ways_out_of_the_folder_are_not_found_even_when_the_folder_has_them() {
        for path in [
            "/.gitkeep",
            "/.hidden/secret.txt",
            "/assets/../index.html/../.gitkeep",
            "/../Cargo.toml",
            "/assets/../../Cargo.toml",
            "/..",
            "/./index.html",
            "/assets/./index-abc123.js",
            "/assets\\index-abc123.js",
            "/assets/index-abc123.js\0.png",
        ] {
            assert_eq!(serve(&app(), path).status(), StatusCode::NOT_FOUND, "{path:?}");
        }
    }

    #[tokio::test]
    async fn without_a_built_interface_the_root_says_how_to_build_one() {
        let nothing = Files::unbuilt();
        let response = serve(&nothing, "/");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(header(&response, "content-security-policy").unwrap().contains("default-src 'none'"));
        let page = body(response).await;
        assert!(page.contains("npm run build") && page.contains("<h1>Tang</h1>"), "{page}");
        assert_eq!(serve(&nothing, "/index.html").status(), StatusCode::OK, "the same at its other name");
        assert_eq!(serve(&nothing, "/assets/index-abc123.js").status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_embedded_folder_does_not_give_up_what_is_outside_it() {
        // Whatever the folder holds (it may be empty), nothing from outside it comes out of it.
        for path in ["../Cargo.toml", "../../Cargo.toml", "/etc/passwd", "..\\Cargo.toml", "assets/../../Cargo.toml"] {
            assert!(Embedded.get(path).is_none(), "{path}");
        }
        assert!(Embedded.get("no/such/file.js").is_none());
    }
}
