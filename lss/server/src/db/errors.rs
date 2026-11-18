//! Recognising the two constraint violations that carry meaning.
//!
//! A duplicate product id and a duplicate (user, product) order are not bugs:
//! they are how the database tells us "409". Matching the *extended* result code
//! distinguishes them from every other failure (CHECK, NOT NULL, I/O), which must
//! stay a 500.

use rusqlite::ffi;

fn extended_code(e: &rusqlite::Error) -> Option<std::ffi::c_int> {
    match e {
        rusqlite::Error::SqliteFailure(err, _) => Some(err.extended_code),
        _ => None,
    }
}

/// `INSERT INTO products` with an id that exists (`products.id` is the PRIMARY KEY).
pub fn is_primary_key_violation(e: &rusqlite::Error) -> bool {
    extended_code(e) == Some(ffi::SQLITE_CONSTRAINT_PRIMARYKEY)
}

/// `INSERT INTO orders` for a (user, product) pair that already has an order.
pub fn is_unique_violation(e: &rusqlite::Error) -> bool {
    extended_code(e) == Some(ffi::SQLITE_CONSTRAINT_UNIQUE)
}
