CREATE TABLE reports (
  id TEXT PRIMARY KEY,
  fingerprint TEXT NOT NULL,
  ip_key TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending', 'created', 'rejected', 'uncertain')),
  issue_url TEXT
);
CREATE INDEX reports_created ON reports(created_at);
CREATE INDEX reports_ip_created ON reports(ip_key, created_at);
