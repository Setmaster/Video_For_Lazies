export function getReport(db, id) { return db.prepare('SELECT id, fingerprint, state, issue_url FROM reports WHERE id = ?').bind(id).first(); }

export async function reserve(db, id, fingerprint, ipKey, now) {
  // One SQLite statement serializes both quota checks and reservation. Never use read-then-write counters.
  const result = await db.prepare(`INSERT OR IGNORE INTO reports (id, fingerprint, ip_key, created_at, state)
    SELECT ?, ?, ?, ?, 'pending'
    WHERE (SELECT COUNT(*) FROM reports WHERE created_at >= ?) < 20
      AND (SELECT COUNT(*) FROM reports WHERE ip_key = ? AND created_at >= ?) < 3`).bind(id, fingerprint, ipKey, now, Math.floor(now / 86400) * 86400, ipKey, now - 3600).run();
  return result.meta.changes === 1;
}

export async function finish(db, id, result) {
  await db.prepare("UPDATE reports SET state = ?, issue_url = ? WHERE id = ? AND state = 'pending'").bind(result.state, result.issue_url || null, id).run();
}

export function cleanup(db, now = Math.floor(Date.now() / 1000)) {
  return db.prepare('DELETE FROM reports WHERE created_at < ?').bind(now - 7 * 86400).run();
}
