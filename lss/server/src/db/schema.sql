-- Applied at every startup. Every statement is idempotent (IF NOT EXISTS), so
-- re-running it against an existing database changes nothing.

-- Products. Only `stock` ever changes, and only through orders.
CREATE TABLE IF NOT EXISTS products (
    id       TEXT    PRIMARY KEY,
    name     TEXT    NOT NULL,
    category TEXT    NOT NULL,
    price    INTEGER NOT NULL CHECK (price >= 0),
    -- Safety net: even if application code had a bug, the database itself
    -- refuses to store negative stock, so an oversell fails instead of committing.
    stock    INTEGER NOT NULL CHECK (stock >= 0)
);

-- Serves "list a category sorted by price, then id" straight from the index,
-- and lets startup load each category already in order.
CREATE INDEX IF NOT EXISTS products_category_price_id
    ON products (category, price, id);

-- Orders. order_id is SQLite's rowid: unique, monotonic, free to generate.
CREATE TABLE IF NOT EXISTS orders (
    order_id   INTEGER PRIMARY KEY,
    user_id    TEXT    NOT NULL,
    product_id TEXT    NOT NULL,
    -- This constraint is what makes a duplicate purchase impossible: the second
    -- of two simultaneous identical orders fails to insert, whatever the timing.
    -- Its index also serves "all orders of one user" (user_id is the prefix).
    UNIQUE (user_id, product_id)
);
