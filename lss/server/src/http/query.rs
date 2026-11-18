//! Hand-written query-string parsing.
//!
//! `web::Query<T>` answers a bad query with a plain-text 400 that doesn't match the
//! spec, and it has no notion of "first value wins" for repeated keys. Here we
//! decode with `form_urlencoded` (which handles `%xx` and `+` as space) and look
//! up each key ourselves, so the validation rules in `validate.rs` see exactly the
//! decoded strings.

use std::borrow::Cow;

use crate::errors::ApiError;
use crate::validate::{self, ListQuery};

/// Returns the decoded value of the first `name=...` pair, or `None` if absent.
///
/// A key without `=` (`?page`) counts as present with an empty value, which
/// validation then rejects; that matches "parameter is empty".
pub fn first<'a>(query: &'a str, name: &str) -> Option<Cow<'a, str>> {
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// Parses and validates `GET /products?category=&page=&limit=`.
pub fn list_query(query: &str) -> Result<ListQuery, ApiError> {
    let category = first(query, "category");
    let page = first(query, "page");
    let limit = first(query, "limit");
    validate::list_query(category.as_deref(), page.as_deref(), limit.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_first_value() {
        assert_eq!(first("a=1&b=2", "b").as_deref(), Some("2"));
        assert_eq!(first("a=1&a=2", "a").as_deref(), Some("1"));
        assert_eq!(first("a=1", "b"), None);
        assert_eq!(first("", "a"), None);
    }

    #[test]
    fn key_without_value_is_empty() {
        assert_eq!(first("page", "page").as_deref(), Some(""));
        assert_eq!(first("page=", "page").as_deref(), Some(""));
    }

    #[test]
    fn decodes_percent_and_plus() {
        assert_eq!(first("c=a%20b", "c").as_deref(), Some("a b"));
        assert_eq!(first("c=a+b", "c").as_deref(), Some("a b"));
        assert_eq!(first("limit=%2B5", "limit").as_deref(), Some("+5"));
    }

    #[test]
    fn list_query_happy_path() {
        let q = list_query("category=c8d21fa&page=2&limit=10").unwrap();
        assert_eq!((q.category.as_str(), q.page, q.limit), ("c8d21fa", 2, 10));
    }

    #[test]
    fn list_query_defaults_and_unknown_params_ignored() {
        let q = list_query("category=c1&sort=desc").unwrap();
        assert_eq!((q.page, q.limit), (1, 20));
    }

    #[test]
    fn list_query_first_repeat_wins() {
        let q = list_query("category=c1&category=c2&limit=5&limit=500").unwrap();
        assert_eq!((q.category.as_str(), q.limit), ("c1", 5));
        assert!(list_query("category=c1&limit=500&limit=5").is_err());
    }

    #[test]
    fn list_query_rejections() {
        for q in [
            "",
            "page=2",
            "category=",
            "category",
            "category=c&limit=80",
            "category=c&limit=0",
            "category=c&page=0",
            "category=c&page=",
            "category=c&limit=+5", // '+' decodes to a space
            "category=c&limit=%2B5",
            "category=c&page=1.5",
            "category=c&page=-1",
        ] {
            assert_eq!(list_query(q), Err(ApiError::Validation), "{q:?}");
        }
    }
}
