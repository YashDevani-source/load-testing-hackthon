//! Input validation. Pure functions: bytes or strings in, typed values or
//! `VALIDATION_ERROR` out. No I/O, no allocation beyond the returned values.
//!
//! Why not `#[derive(Deserialize)]` on typed structs? Serde is lenient in ways the
//! spec isn't (and strict in ways it isn't): depending on the type it can accept
//! `10.0` for an integer field, or need `deny_unknown_fields` gymnastics. Checking
//! a `serde_json::Value` by hand makes every accepted and rejected shape explicit
//! and testable, and the cost is irrelevant next to network I/O.

use serde_json::{Map, Value};

use crate::errors::ApiError;

/// Spec: ids and categories are 1-64 characters.
pub const ID_MAX_CHARS: usize = 64;
/// Spec: product names are 1-200 characters.
pub const NAME_MAX_CHARS: usize = 200;
/// Spec: `page` defaults to 1.
pub const DEFAULT_PAGE: u64 = 1;
/// Spec: `limit` defaults to 20.
pub const DEFAULT_LIMIT: u64 = 20;
/// Spec: `limit` is at most 50.
pub const MAX_LIMIT: u64 = 50;

/// A validated `POST /products` body.
///
/// `price` and `stock` are `i64` because that is SQLite's INTEGER; validation
/// guarantees both are in `0..=i64::MAX`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProduct {
    pub id: String,
    pub name: String,
    pub category: String,
    pub price: i64,
    pub stock: i64,
}

/// A validated `POST /orders` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOrder {
    pub user_id: String,
    pub product_id: String,
}

/// Validated `GET /products?category=&page=&limit=` parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    pub category: String,
    /// 1-based page number, >= 1.
    pub page: u64,
    /// Page size, 1..=50.
    pub limit: u64,
}

impl ListQuery {
    /// Number of items to skip. Saturates instead of overflowing: a huge page
    /// number is simply past the end, which the spec answers with `items: []`.
    pub fn offset(&self) -> u64 {
        self.page.saturating_sub(1).saturating_mul(self.limit)
    }
}

/// Parses a request body that must be a JSON object.
///
/// Empty bodies, invalid JSON, trailing garbage and non-object JSON (arrays,
/// strings, numbers, `null`) are all `VALIDATION_ERROR`.
pub fn json_object(body: &[u8]) -> Result<Map<String, Value>, ApiError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(ApiError::Validation),
    }
}

/// Validates a `POST /products` body. Extra fields are ignored.
pub fn new_product(body: &[u8]) -> Result<NewProduct, ApiError> {
    let obj = json_object(body)?;
    Ok(NewProduct {
        id: string_field(&obj, "id", ID_MAX_CHARS)?,
        name: string_field(&obj, "name", NAME_MAX_CHARS)?,
        category: string_field(&obj, "category", ID_MAX_CHARS)?,
        price: whole_number_field(&obj, "price")?,
        stock: whole_number_field(&obj, "stock")?,
    })
}

/// Validates a `POST /orders` body. Extra fields are ignored.
///
/// The spec gives no length limit for `userId`/`productId` here, only "non-empty
/// string". A `productId` longer than 64 characters can't exist, so it will be a
/// 404 later, which is the right answer for it.
pub fn new_order(body: &[u8]) -> Result<NewOrder, ApiError> {
    let obj = json_object(body)?;
    Ok(NewOrder {
        user_id: string_field(&obj, "userId", usize::MAX)?,
        product_id: string_field(&obj, "productId", usize::MAX)?,
    })
}

/// Validates the list query parameters (already percent-decoded; `None` = absent).
pub fn list_query(
    category: Option<&str>,
    page: Option<&str>,
    limit: Option<&str>,
) -> Result<ListQuery, ApiError> {
    let category = match category {
        Some(c) if !c.is_empty() => c.to_owned(),
        _ => return Err(ApiError::Validation),
    };
    let page = match page {
        None => DEFAULT_PAGE,
        Some(p) => match parse_digits(p) {
            Some(n) if n >= 1 => n,
            _ => return Err(ApiError::Validation),
        },
    };
    let limit = match limit {
        None => DEFAULT_LIMIT,
        Some(l) => match parse_digits(l) {
            Some(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(ApiError::Validation),
        },
    };
    Ok(ListQuery {
        category,
        page,
        limit,
    })
}

/// A required string field with 1..=`max_chars` characters.
///
/// Length is counted in Unicode scalar values, not bytes: "1-64 characters"
/// should accept 64 Devanagari letters even though that's 192 bytes.
fn string_field(obj: &Map<String, Value>, key: &str, max_chars: usize) -> Result<String, ApiError> {
    match obj.get(key) {
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= max_chars => Ok(s.clone()),
        _ => Err(ApiError::Validation),
    }
}

/// A required whole number in `0..=i64::MAX`.
///
/// `is_u64()` is true only for JSON integers without a sign, fraction or exponent
/// that fit in a u64, so this one check rejects `"100"`, `10.5`, `1e2`, `-5`,
/// `true` and `null`. The `try_from` then rejects u64 values SQLite can't store.
fn whole_number_field(obj: &Map<String, Value>, key: &str) -> Result<i64, ApiError> {
    obj.get(key)
        .filter(|v| v.is_u64())
        .and_then(Value::as_u64)
        .and_then(|n| i64::try_from(n).ok())
        .ok_or(ApiError::Validation)
}

/// Parses a non-empty run of ASCII digits. Anything else (empty, sign, decimal
/// point, whitespace, letters, overflow) is `None`.
///
/// Done by hand because `str::parse::<u64>` accepts a leading `+`.
fn parse_digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn product_json(overrides: &[(&str, Value)]) -> Vec<u8> {
        let mut v = json!({
            "id": "p3f9a0c2e71bd",
            "name": "Pixel 10 Pro 256GB",
            "category": "c8d21fa",
            "price": 79999,
            "stock": 120
        });
        for (k, val) in overrides {
            v[*k] = val.clone();
        }
        serde_json::to_vec(&v).unwrap()
    }

    fn without(key: &str) -> Vec<u8> {
        let mut v: Value = serde_json::from_slice(&product_json(&[])).unwrap();
        v.as_object_mut().unwrap().remove(key);
        serde_json::to_vec(&v).unwrap()
    }

    // ---- POST /products -------------------------------------------------------

    #[test]
    fn product_valid() {
        let p = new_product(&product_json(&[])).unwrap();
        assert_eq!(
            p,
            NewProduct {
                id: "p3f9a0c2e71bd".into(),
                name: "Pixel 10 Pro 256GB".into(),
                category: "c8d21fa".into(),
                price: 79999,
                stock: 120,
            }
        );
    }

    #[test]
    fn product_zero_price_and_stock_are_valid() {
        let p = new_product(&product_json(&[("price", json!(0)), ("stock", json!(0))])).unwrap();
        assert_eq!((p.price, p.stock), (0, 0));
    }

    #[test]
    fn product_extra_fields_ignored() {
        let body = product_json(&[
            ("_id", json!("x")),
            ("__v", json!(0)),
            ("extra", json!([1])),
        ]);
        assert!(new_product(&body).is_ok());
    }

    #[test]
    fn product_each_missing_field_rejected() {
        for key in ["id", "name", "category", "price", "stock"] {
            assert_eq!(
                new_product(&without(key)),
                Err(ApiError::Validation),
                "missing {key}"
            );
        }
    }

    #[test]
    fn product_string_fields_reject_wrong_types_and_empty() {
        let bad = [
            json!(""),
            json!(null),
            json!(5),
            json!(true),
            json!([]),
            json!({}),
        ];
        for key in ["id", "name", "category"] {
            for val in &bad {
                let body = product_json(&[(key, val.clone())]);
                assert_eq!(new_product(&body), Err(ApiError::Validation), "{key}={val}");
            }
        }
    }

    #[test]
    fn product_string_length_limits() {
        let ok64 = "a".repeat(64);
        let bad65 = "a".repeat(65);
        let ok200 = "n".repeat(200);
        let bad201 = "n".repeat(201);
        assert!(new_product(&product_json(&[("id", json!(ok64))])).is_ok());
        assert!(new_product(&product_json(&[("category", json!(ok64))])).is_ok());
        assert!(new_product(&product_json(&[("name", json!(ok200))])).is_ok());
        assert!(new_product(&product_json(&[("id", json!(bad65))])).is_err());
        assert!(new_product(&product_json(&[("category", json!(bad65))])).is_err());
        assert!(new_product(&product_json(&[("name", json!(bad201))])).is_err());
    }

    #[test]
    fn product_length_counts_characters_not_bytes() {
        // 64 three-byte characters = 192 bytes, still 64 characters.
        let devanagari = "क".repeat(64);
        assert_eq!(devanagari.len(), 192);
        assert!(new_product(&product_json(&[("id", json!(devanagari))])).is_ok());
        let too_long = "क".repeat(65);
        assert!(new_product(&product_json(&[("id", json!(too_long))])).is_err());
    }

    #[test]
    fn product_numbers_reject_every_non_whole_number() {
        let bad = [
            json!("100"),
            json!(10.5),
            json!(1.0),
            json!(-5),
            json!(-1),
            json!(true),
            json!(false),
            json!(null),
            json!([]),
            json!({}),
            json!(""),
        ];
        for key in ["price", "stock"] {
            for val in &bad {
                let body = product_json(&[(key, val.clone())]);
                assert_eq!(new_product(&body), Err(ApiError::Validation), "{key}={val}");
            }
        }
    }

    #[test]
    fn product_numbers_reject_exponent_notation() {
        // Raw text, because json!() would normalise these.
        for raw in ["1e2", "1E2", "1.5e3"] {
            let body =
                format!(r#"{{"id":"p1","name":"n","category":"c","price":{raw},"stock":1}}"#);
            assert_eq!(
                new_product(body.as_bytes()),
                Err(ApiError::Validation),
                "{raw}"
            );
        }
    }

    #[test]
    fn product_numbers_upper_bound_is_i64_max() {
        let max = i64::MAX as u64;
        let p = new_product(&product_json(&[("price", json!(max))])).unwrap();
        assert_eq!(p.price, i64::MAX);
        let over = product_json(&[("stock", json!(max + 1))]);
        assert_eq!(new_product(&over), Err(ApiError::Validation));
        // Larger than u64: serde_json reads it as a float, which is_u64 rejects.
        let huge =
            br#"{"id":"p1","name":"n","category":"c","price":18446744073709551616,"stock":1}"#;
        assert_eq!(new_product(huge), Err(ApiError::Validation));
    }

    #[test]
    fn product_body_must_be_a_json_object() {
        let bodies: [&[u8]; 9] = [
            b"",
            b" ",
            b"null",
            b"[]",
            b"\"str\"",
            b"42",
            b"{",
            b"not json",
            b"{\"id\":\"p1\"} trailing",
        ];
        for body in bodies {
            assert_eq!(
                new_product(body),
                Err(ApiError::Validation),
                "{:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn product_invalid_utf8_rejected() {
        let body = b"{\"id\":\"\xff\",\"name\":\"n\",\"category\":\"c\",\"price\":1,\"stock\":1}";
        assert_eq!(new_product(body), Err(ApiError::Validation));
    }

    // ---- POST /orders ---------------------------------------------------------

    #[test]
    fn order_valid_and_extra_fields_ignored() {
        let o =
            new_order(br#"{"userId":"u7b0e44c19a2f","productId":"p3f9a0c2e71bd","x":1}"#).unwrap();
        assert_eq!(
            o,
            NewOrder {
                user_id: "u7b0e44c19a2f".into(),
                product_id: "p3f9a0c2e71bd".into(),
            }
        );
    }

    #[test]
    fn order_missing_fields_rejected() {
        assert_eq!(new_order(br#"{"userId":"u1"}"#), Err(ApiError::Validation));
        assert_eq!(
            new_order(br#"{"productId":"p1"}"#),
            Err(ApiError::Validation)
        );
        assert_eq!(new_order(br"{}"), Err(ApiError::Validation));
    }

    #[test]
    fn order_wrong_types_and_empty_rejected() {
        let bad = [
            json!(""),
            json!(null),
            json!(5),
            json!(true),
            json!([]),
            json!({}),
        ];
        for key in ["userId", "productId"] {
            for val in &bad {
                let mut body = json!({"userId": "u1", "productId": "p1"});
                body[key] = val.clone();
                let bytes = serde_json::to_vec(&body).unwrap();
                assert_eq!(new_order(&bytes), Err(ApiError::Validation), "{key}={val}");
            }
        }
    }

    #[test]
    fn order_body_must_be_a_json_object() {
        for body in [&b""[..], b"[]", b"null", b"{", b"\"u1\""] {
            assert_eq!(new_order(body), Err(ApiError::Validation));
        }
    }

    // ---- GET /products?category=&page=&limit= ---------------------------------

    #[test]
    fn list_defaults() {
        let q = list_query(Some("c8d21fa"), None, None).unwrap();
        assert_eq!(
            q,
            ListQuery {
                category: "c8d21fa".into(),
                page: 1,
                limit: 20,
            }
        );
        assert_eq!(q.offset(), 0);
    }

    #[test]
    fn list_explicit_values_and_offset() {
        let q = list_query(Some("c"), Some("3"), Some("50")).unwrap();
        assert_eq!((q.page, q.limit, q.offset()), (3, 50, 100));
        let q = list_query(Some("c"), Some("1"), Some("1")).unwrap();
        assert_eq!((q.page, q.limit), (1, 1));
    }

    #[test]
    fn list_leading_zeros_are_still_digits() {
        let q = list_query(Some("c"), Some("007"), Some("010")).unwrap();
        assert_eq!((q.page, q.limit), (7, 10));
    }

    #[test]
    fn list_category_required_non_empty() {
        assert_eq!(list_query(None, None, None), Err(ApiError::Validation));
        assert_eq!(list_query(Some(""), None, None), Err(ApiError::Validation));
    }

    #[test]
    fn list_page_rejections() {
        for p in [
            "",
            "0",
            "-1",
            "+1",
            "1.0",
            "1.5",
            "abc",
            "1a",
            " 1",
            "1 ",
            "1e2",
            "99999999999999999999999",
        ] {
            assert_eq!(
                list_query(Some("c"), Some(p), None),
                Err(ApiError::Validation),
                "page={p:?}"
            );
        }
    }

    #[test]
    fn list_limit_rejections() {
        for l in [
            "",
            "0",
            "51",
            "80",
            "-5",
            "+5",
            "2.5",
            "ten",
            " 5",
            "99999999999999999999999",
        ] {
            assert_eq!(
                list_query(Some("c"), None, Some(l)),
                Err(ApiError::Validation),
                "limit={l:?}"
            );
        }
    }

    #[test]
    fn list_huge_page_offset_saturates() {
        let q = list_query(Some("c"), Some(&u64::MAX.to_string()), Some("50")).unwrap();
        assert_eq!(q.offset(), u64::MAX);
    }
}
