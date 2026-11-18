//! Response bodies, built as strings.
//!
//! Both read modes (sqlite and memory) and the write handlers render through
//! these functions, so the same data always produces the same bytes. A product
//! is always `json_prefix + stock + "}"`: everything that never changes is
//! serialised once, when the product is created, and stored with it.

use crate::http::respond::push_json_str;

/// `{"id":<id>,"name":<name>,"category":<category>,"price":<price>,"stock":`
///
/// The one function that builds a product's fixed JSON. Used by `POST /products`,
/// the schema-v2 backfill, and nowhere else (everyone else reads the stored copy).
pub fn build_json_prefix(id: &str, name: &str, category: &str, price: i64) -> String {
    let mut out = String::with_capacity(64 + id.len() + name.len() + category.len());
    out.push_str("{\"id\":");
    push_json_str(&mut out, id);
    out.push_str(",\"name\":");
    push_json_str(&mut out, name);
    out.push_str(",\"category\":");
    push_json_str(&mut out, category);
    out.push_str(",\"price\":");
    push_int(&mut out, price);
    out.push_str(",\"stock\":");
    out
}

/// Appends an integer without going through `fmt` (itoa is several times faster).
pub fn push_int(out: &mut String, n: impl itoa::Integer) {
    out.push_str(itoa::Buffer::new().format(n));
}

/// Appends a product to `out`.
pub fn push_product(out: &mut String, json_prefix: &str, stock: i64) {
    out.push_str(json_prefix);
    push_int(out, stock);
    out.push('}');
}

/// A whole product body.
pub fn product(json_prefix: &str, stock: i64) -> String {
    let mut out = String::with_capacity(json_prefix.len() + 21);
    push_product(&mut out, json_prefix, stock);
    out
}

/// The client-facing order id. The database's integer `order_id` is unique and
/// never reused (INTEGER PRIMARY KEY on a table we never delete from); the `o_`
/// prefix just makes it obviously a string, as the spec's examples are.
pub fn push_order_id(out: &mut String, order_id: i64) {
    out.push_str("\"o_");
    push_int(out, order_id);
    out.push('"');
}

/// `{"orderId":"o_<id>","userId":<user>,"productId":<product>}`
pub fn order_created(order_id: i64, user_id: &str, product_id: &str) -> String {
    let mut out = String::with_capacity(48 + user_id.len() + product_id.len());
    out.push_str("{\"orderId\":");
    push_order_id(&mut out, order_id);
    out.push_str(",\"userId\":");
    push_json_str(&mut out, user_id);
    out.push_str(",\"productId\":");
    push_json_str(&mut out, product_id);
    out.push('}');
    out
}

/// Builds `{"items":[...],"page":p,"limit":l,"total":t}` one item at a time.
pub struct ListBody {
    out: String,
    empty: bool,
}

impl ListBody {
    pub fn new(limit: u64) -> Self {
        // ~150 bytes per product is typical; over-reserving is cheaper than
        // growing the buffer several times.
        let cap = 64 + 160 * usize::try_from(limit).unwrap_or(0);
        let mut out = String::with_capacity(cap);
        out.push_str("{\"items\":[");
        Self { out, empty: true }
    }

    pub fn push(&mut self, json_prefix: &str, stock: i64) {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
        push_product(&mut self.out, json_prefix, stock);
    }

    pub fn finish(mut self, page: u64, limit: u64, total: u64) -> String {
        self.out.push_str("],\"page\":");
        push_int(&mut self.out, page);
        self.out.push_str(",\"limit\":");
        push_int(&mut self.out, limit);
        self.out.push_str(",\"total\":");
        push_int(&mut self.out, total);
        self.out.push('}');
        self.out
    }
}

/// Builds `{"userId":<user>,"orders":[{"orderId":..,"productId":..},...]}`.
pub struct OrdersBody {
    out: String,
    empty: bool,
}

impl OrdersBody {
    pub fn new(user_id: &str) -> Self {
        let mut out = String::with_capacity(48 + user_id.len() + 64);
        out.push_str("{\"userId\":");
        push_json_str(&mut out, user_id);
        out.push_str(",\"orders\":[");
        Self { out, empty: true }
    }

    pub fn push(&mut self, order_id: i64, product_id: &str) {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
        self.out.push_str("{\"orderId\":");
        push_order_id(&mut self.out, order_id);
        self.out.push_str(",\"productId\":");
        push_json_str(&mut self.out, product_id);
        self.out.push('}');
    }

    pub fn finish(mut self) -> String {
        self.out.push_str("]}");
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn product_round_trips_awkward_strings() {
        let name = "Quote \" back \\ new\nline हिन्दी ✓ \u{0007}";
        let prefix = build_json_prefix("p1", name, "c\"1", 79999);
        assert_eq!(
            parse(&product(&prefix, 120)),
            json!({"id":"p1","name":name,"category":"c\"1","price":79999,"stock":120})
        );
    }

    #[test]
    fn product_matches_spec_layout() {
        let prefix = build_json_prefix("p3f9a0c2e71bd", "Pixel 10 Pro 256GB", "c8d21fa", 79999);
        assert_eq!(
            product(&prefix, 119),
            r#"{"id":"p3f9a0c2e71bd","name":"Pixel 10 Pro 256GB","category":"c8d21fa","price":79999,"stock":119}"#
        );
    }

    #[test]
    fn order_created_body() {
        assert_eq!(
            order_created(42, "u\"1", "p1"),
            r#"{"orderId":"o_42","userId":"u\"1","productId":"p1"}"#
        );
    }

    #[test]
    fn list_bodies() {
        assert_eq!(
            ListBody::new(20).finish(20, 20, 214),
            r#"{"items":[],"page":20,"limit":20,"total":214}"#
        );
        let mut b = ListBody::new(2);
        b.push(r#"{"id":"a","stock":"#, 1);
        b.push(r#"{"id":"b","stock":"#, 0);
        assert_eq!(
            b.finish(1, 2, 3),
            r#"{"items":[{"id":"a","stock":1},{"id":"b","stock":0}],"page":1,"limit":2,"total":3}"#
        );
    }

    #[test]
    fn orders_bodies() {
        assert_eq!(
            OrdersBody::new("u1").finish(),
            r#"{"userId":"u1","orders":[]}"#
        );
        let mut b = OrdersBody::new("u1");
        b.push(1, "p1");
        b.push(7, "p2");
        assert_eq!(
            b.finish(),
            r#"{"userId":"u1","orders":[{"orderId":"o_1","productId":"p1"},{"orderId":"o_7","productId":"p2"}]}"#
        );
    }
}
