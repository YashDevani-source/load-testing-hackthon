//! JSON response helpers.
//!
//! Every response goes through here so `Content-Type: application/json` can't be
//! forgotten on some path (the spec requires it on every response, errors included).

use actix_web::HttpResponse;
use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_TYPE, HeaderValue};
use actix_web::web::Bytes;

const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");

/// A JSON body built by the caller (product, list, order list...).
pub fn json(status: StatusCode, body: String) -> HttpResponse {
    json_bytes(status, body)
}

/// A JSON body that is already bytes.
pub fn json_bytes(status: StatusCode, body: impl Into<Bytes>) -> HttpResponse {
    HttpResponse::build(status)
        .insert_header((CONTENT_TYPE, APPLICATION_JSON))
        .body(body.into())
}

/// A constant JSON body. `Bytes::from_static` points at the string in the binary,
/// so this neither allocates nor copies.
pub fn json_static(status: StatusCode, body: &'static str) -> HttpResponse {
    json_bytes(status, Bytes::from_static(body.as_bytes()))
}

/// Appends `s` as a JSON string literal (quotes included, special characters
/// escaped).
///
/// Escaping is delegated to `serde_json` rather than done by hand: quotes,
/// backslashes, control characters and unicode are exactly the kind of detail a
/// hand-written escaper gets subtly wrong, and one wrong byte is a wrong answer.
pub fn push_json_str(out: &mut String, s: &str) {
    match serde_json::to_string(s) {
        Ok(quoted) => out.push_str(&quoted),
        // Serialising a &str into memory cannot fail (serde_json only errors on
        // I/O or non-string map keys). This arm exists so the code has no panic
        // path; if it ever ran, the log would say so.
        Err(err) => {
            tracing::error!(error = %err, "failed to escape JSON string");
            out.push_str("\"\"");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_serde() {
        for s in [
            "plain",
            "quote\"",
            "back\\slash",
            "new\nline",
            "tab\t",
            "हिन्दी ✓",
            "\u{0001}",
            "",
        ] {
            let mut out = String::from("x");
            push_json_str(&mut out, s);
            let back: String = serde_json::from_str(&out[1..]).unwrap();
            assert_eq!(back, s);
        }
    }
}
