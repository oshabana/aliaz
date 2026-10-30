import { argon2idAsync } from "@noble/hashes/argon2.js";
import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = realpathSync(join(dirname(fileURLToPath(import.meta.url)), ".."));
const state = join(root, ".wrangler", "test-accounts");
// Sync login accepts usernames only. Registered real users receive UUID IDs;
// the seed uses its canonical usernames as IDs to recognize only its own rows.
const accounts = [
  { username: "adminuser", password: "Adminuser123!" },
  { username: "testuser", password: "Testuser123!" },
];

function execute(command) {
  const result = spawnSync(join(root, "node_modules", ".bin", "wrangler"), [
    "d1", "execute", "aliaz-sync", "--config", join(root, "wrangler.toml"),
    "--local", "--persist-to", state, "--json", "--command", command,
  ], { cwd: root, encoding: "utf8", timeout: 60000, env: { ...process.env, WRANGLER_SEND_METRICS: "false", CI: "true" } });
  if (result.error || result.status !== 0) {
    throw new Error("Local D1 seed failed. Install repository dependencies and apply migrations to .wrangler/test-accounts. No remote fallback is allowed.");
  }
  try {
    return JSON.parse(result.stdout);
  } catch {
    throw new Error("Wrangler returned an invalid local D1 response.");
  }
}

async function seed() {
  if (process.env.NODE_ENV !== "development" || process.env.CONFIRM_LOCAL_TEST_ACCOUNTS !== "aliaz") {
    throw new Error("Set NODE_ENV=development and CONFIRM_LOCAL_TEST_ACCOUNTS=aliaz to seed isolated local D1 storage.");
  }
  const result = execute("SELECT id, username FROM users WHERE username IN ('adminuser', 'testuser') OR id IN ('adminuser', 'testuser')");
  const rows = result.flatMap((entry) => entry.results ?? []);
  for (const row of rows) {
    if (!accounts.some((account) => row.username === account.username && row.id === account.username)) {
      throw new Error("Refusing to overwrite an existing account that was not created by this seed.");
    }
  }
  const statements = [];
  for (const account of accounts) {
    const salt = randomBytes(16);
    const hash = await argon2idAsync(account.password, salt, { t: 1, m: 4096, p: 1, dkLen: 32, maxmem: 8 * 1024 * 1024 });
    const passwordHash = `argon2id$v=19$m=4096,t=1,p=1$${salt.toString("base64url")}$${Buffer.from(hash).toString("base64url")}`;
    const id = account.username;
    statements.push(
      `INSERT INTO users (id, username, password_hash, created_at) VALUES ('${id}', '${account.username}', '${passwordHash}', unixepoch()) ON CONFLICT(id) DO UPDATE SET password_hash = excluded.password_hash WHERE users.username = excluded.username`,
      `INSERT INTO sync_state (user_id, latest_version) VALUES ('${id}', 0) ON CONFLICT(user_id) DO NOTHING`,
    );
  }
  execute(statements.join("; "));
  console.log("Provisioned adminuser and testuser in isolated local D1 storage. Existing vault records are unchanged.");
}

seed().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
