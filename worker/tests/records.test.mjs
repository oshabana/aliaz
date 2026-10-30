import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";
import worker from "../src/index.ts";

// Executes the Worker's SQL against private SQLite. This checks SQL and rollback
// behavior; it is not a claim of deployed D1 runtime qualification.
class SqliteD1 {
  constructor() {
    this.db = new DatabaseSync(":memory:");
    this.db.exec("PRAGMA foreign_keys = ON");
    this.batches = 0;
  }
  prepare(sql) {
    const db = this.db;
    function statement(values = []) {
      return {
        bind: (...next) => statement(next),
        async first() { return db.prepare(sql).get(...values) ?? null; },
        async all() { return { results: db.prepare(sql).all(...values), success: true }; },
        async run() { return { meta: db.prepare(sql).run(...values), success: true }; },
        execute() { return { results: db.prepare(sql).all(...values), success: true }; },
      };
    }
    return statement();
  }
  async batch(statements) {
    this.batches += 1;
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const results = statements.map((statement) => statement.execute());
      this.db.exec("COMMIT");
      return results;
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }
}

const migrations = new URL("../migrations/", import.meta.url);
function migrate(db, names = readdirSync(migrations).filter((name) => name.endsWith(".sql")).sort()) {
  for (const name of names) db.exec(readFileSync(new URL(name, migrations), "utf8"));
}
async function fixture(t) {
  const binding = new SqliteD1();
  t.after(() => binding.db.close());
  migrate(binding.db);
  const tokens = new Map();
  for (const user of ["first", "second"]) {
    binding.db.prepare("INSERT INTO users VALUES (?, ?, 'unused', 1)").run(user, user);
    binding.db.prepare("INSERT INTO sync_state VALUES (?, 0)").run(user);
    const token = `local-fixture-${user}`;
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token));
    const hash = Buffer.from(digest).toString("base64url");
    binding.db.prepare("INSERT INTO sessions VALUES (?, ?, 1, ?)").run(hash, user, Math.floor(Date.now() / 1000) + 3600);
    tokens.set(user, token);
  }
  return {
    binding,
    async push(records, user = "first") {
      return worker.fetch(new Request("http://fixture.invalid/v1/records", {
        method: "POST", headers: { Authorization: `Bearer ${tokens.get(user)}`, "Content-Type": "application/json" },
        body: JSON.stringify({ records }),
      }), { DB: binding });
    },
    async pull(user = "first") {
      const response = await worker.fetch(new Request("http://fixture.invalid/v1/records?after=0", {
        headers: { Authorization: `Bearer ${tokens.get(user)}` },
      }), { DB: binding });
      assert.equal(response.status, 200);
      return response.json();
    },
  };
}
const record = (id, blob = "x".repeat(32)) => ({ id, record_type: "collection", encrypted_blob: blob, updated_at: 123 });

test("migration preserves existing ciphertext and allows equal IDs across accounts", async (t) => {
  const db = new DatabaseSync(":memory:");
  t.after(() => db.close());
  db.exec("PRAGMA foreign_keys = ON");
  migrate(db, ["0001_initial.sql", "0002_sessions.sql", "0003_auth_rate_limits.sql"]);
  db.exec("INSERT INTO users VALUES ('first', 'first', 'unused', 1), ('second', 'second', 'unused', 1)");
  db.exec("INSERT INTO vault_records VALUES ('collection:shared', 'first', 'collection', 'retained ciphertext', 7, 123)");
  migrate(db, ["0004_user_scoped_records.sql"]);
  db.exec("INSERT INTO vault_records VALUES ('collection:shared', 'second', 'collection', 'other ciphertext', 1, 124)");
  assert.equal(db.prepare("SELECT encrypted_blob FROM vault_records WHERE user_id = 'first'").get().encrypted_blob, "retained ciphertext");
  assert.equal(db.prepare("SELECT count(*) AS count FROM vault_records").get().count, 2);
});

test("separate accounts can push and pull the same deterministic collection ID", async (t) => {
  const f = await fixture(t);
  assert.equal((await f.push([record("collection:shared")])).status, 200);
  assert.equal((await f.push([record("collection:shared", "y".repeat(32))], "second")).status, 200);
  assert.equal((await f.pull()).records[0].encrypted_blob, "x".repeat(32));
  assert.equal((await f.pull("second")).records[0].encrypted_blob, "y".repeat(32));
});

test("invalid later record and null entry leave the entire upload untouched", async (t) => {
  const f = await fixture(t);
  for (const invalid of [null, record("invalid id"), { ...record("nan"), updated_at: null }]) {
    assert.equal((await f.push([record("valid"), invalid])).status, 400);
    assert.deepEqual(await f.pull(), { latest_version: 0, records: [] });
  }
  assert.equal(f.binding.batches, 0);
});

test("a storage failure rolls back earlier writes and the allocated versions", async (t) => {
  const f = await fixture(t);
  f.binding.db.exec("CREATE TRIGGER reject_fixture BEFORE INSERT ON vault_records WHEN NEW.id = 'fail' BEGIN SELECT RAISE(ABORT, 'fixture storage failure'); END");
  assert.equal((await f.push([record("valid"), record("fail")])).status, 500);
  assert.deepEqual(await f.pull(), { latest_version: 0, records: [] });
});

test("concurrent pushes allocate distinct increasing versions within atomic batches", async (t) => {
  const f = await fixture(t);
  const responses = await Promise.all([f.push([record("one"), record("two")]), f.push([record("three")])]);
  for (const response of responses) assert.equal(response.status, 200);
  const bodies = await Promise.all(responses.map((response) => response.json()));
  assert.deepEqual(bodies.flatMap((body) => body.records.map((record) => record.version)).sort((a, b) => a - b), [1, 2, 3]);
  const pulled = await f.pull();
  assert.equal(pulled.latest_version, 3);
  assert.equal(pulled.records.length, 3);
  assert.equal(f.binding.batches, 2);
});


test("command details survive an older client's command-only update", async (t) => {
  const f = await fixture(t);
  const metadata = { ...record("metadata:command-one"), record_type: "alias_metadata" };
  assert.equal((await f.push([{ ...record("command-one"), record_type: "alias" }, metadata])).status, 200);
  assert.equal((await f.push([{ ...record("command-one", "y".repeat(32)), record_type: "alias" }])).status, 200);
  const pulled = await f.pull();
  assert.equal(pulled.records.length, 2);
  assert.equal(pulled.records.find((record) => record.record_type === "alias_metadata").encrypted_blob, metadata.encrypted_blob);
});
