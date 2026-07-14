CREATE TABLE IF NOT EXISTS visitors (
    id BIGSERIAL PRIMARY KEY,
    ip TEXT NOT NULL,
    country TEXT,
    country_code TEXT,
    city TEXT,
    isp TEXT,
    path TEXT NOT NULL,
    visited_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS visitors_visited_at_idx ON visitors (visited_at DESC);
