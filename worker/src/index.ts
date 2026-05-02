import { argon2idAsync } from "@noble/hashes/argon2.js";

export interface Env {
  DB: D1Database;
}

type AccountRequest = {
  username?: unknown;
  password?: unknown;
};

type UploadRecord = {
  id?: unknown;
  record_type?: unknown;
  encrypted_blob?: unknown;
  updated_at?: unknown;
};

type Session = {
  user_id: string;
};

const SESSION_TTL_SECONDS = 60 * 60 * 24 * 90;
const ARGON2_PARAMS = {
  t: 1,
  m: 4096,
  p: 1,
  dkLen: 32,
  maxmem: 8 * 1024 * 1024,
};
const LOGIN_USERNAME_LIMIT = 8;
const LOGIN_IP_LIMIT = 60;
const LOGIN_WINDOW_SECONDS = 15 * 60;
const REGISTER_USERNAME_LIMIT = 3;
const REGISTER_IP_LIMIT = 20;
const REGISTER_WINDOW_SECONDS = 60 * 60;
const RATE_LIMIT_RETENTION_SECONDS = 24 * 60 * 60;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      if (request.method === "OPTIONS") {
        return new Response(null, { status: 204, headers: corsHeaders() });
      }

      const url = new URL(request.url);

      if (request.method === "GET" && url.pathname === "/health") {
        const row = await env.DB.prepare("SELECT 1 AS ok").first<{ ok: number }>();
        return json({ ok: row?.ok === 1 });
      }

      if (request.method === "POST" && url.pathname === "/v1/register") {
        return await register(request, env);
      }

      if (request.method === "POST" && url.pathname === "/v1/login") {
        return await login(request, env);
      }

      if (request.method === "GET" && url.pathname === "/v1/records") {
        const session = await requireSession(request, env);
        const after = parseVersion(url.searchParams.get("after"));
        const state = await syncState(env, session.user_id);
        const records = await env.DB.prepare(
          "SELECT id, record_type, encrypted_blob, version, updated_at FROM vault_records WHERE user_id = ?1 AND version > ?2 ORDER BY version ASC",
        )
          .bind(session.user_id, after)
          .all();
        return json({
          latest_version: state,
          records: records.results ?? [],
        });
      }

      if (request.method === "POST" && url.pathname === "/v1/records") {
        const session = await requireSession(request, env);
        return await pushRecords(request, env, session);
      }

      return json({ error: "not_found" }, 404);
    } catch (error) {
      if (isHttpError(error)) {
        return json({ error: error.message }, error.status);
      }
      return json({ error: "internal_error" }, 500);
    }
  },
};

async function register(request: Request, env: Env): Promise<Response> {
  const body = await readAccountRequest(request);
  await enforceAuthRateLimit(request, env, "register", body.username, [
    { scope: "username", limit: REGISTER_USERNAME_LIMIT, windowSeconds: REGISTER_WINDOW_SECONDS },
    { scope: "ip", limit: REGISTER_IP_LIMIT, windowSeconds: REGISTER_WINDOW_SECONDS },
  ]);

  const userId = crypto.randomUUID();
  const createdAt = nowUnix();
  const passwordHash = await hashPassword(body.password);

  try {
    await env.DB.batch([
      env.DB.prepare(
        "INSERT INTO users (id, username, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
      ).bind(userId, body.username, passwordHash, createdAt),
      env.DB.prepare(
        "INSERT INTO sync_state (user_id, latest_version) VALUES (?1, 0)",
      ).bind(userId),
    ]);
  } catch {
    throw new HttpError(409, "username_unavailable");
  }

  const token = await createSession(env, userId);
  return json({ user_id: userId, token, latest_version: 0 }, 201);
}

async function login(request: Request, env: Env): Promise<Response> {
  const body = await readAccountRequest(request);
  await enforceAuthRateLimit(request, env, "login", body.username, [
    { scope: "username", limit: LOGIN_USERNAME_LIMIT, windowSeconds: LOGIN_WINDOW_SECONDS },
    { scope: "ip", limit: LOGIN_IP_LIMIT, windowSeconds: LOGIN_WINDOW_SECONDS },
  ]);

  const user = await env.DB.prepare(
    "SELECT id, password_hash FROM users WHERE username = ?1",
  )
    .bind(body.username)
    .first<{ id: string; password_hash: string }>();

  if (!user || !(await verifyPassword(body.password, user.password_hash))) {
    throw new HttpError(401, "invalid_credentials");
  }

  const token = await createSession(env, user.id);
  await clearAuthRateLimit(env, "login", body.username, clientIp(request));
  const latestVersion = await syncState(env, user.id);
  return json({ user_id: user.id, token, latest_version: latestVersion });
}

async function pushRecords(
  request: Request,
  env: Env,
  session: Session,
): Promise<Response> {
  const body = (await request.json().catch(() => null)) as
    | { records?: UploadRecord[] }
    | null;
  if (!body || !Array.isArray(body.records)) {
    throw new HttpError(400, "records_required");
  }
  if (body.records.length > 500) {
    throw new HttpError(413, "too_many_records");
  }

  let latestVersion = await syncState(env, session.user_id);
  const pushed: Array<{ id: string; version: number }> = [];

  for (const record of body.records) {
    const normalized = validateUploadRecord(record);
    const owner = await env.DB.prepare(
      "SELECT user_id FROM vault_records WHERE id = ?1",
    )
      .bind(normalized.id)
      .first<{ user_id: string }>();
    if (owner && owner.user_id !== session.user_id) {
      throw new HttpError(409, "record_id_conflict");
    }

    latestVersion += 1;
    await env.DB.prepare(
      `INSERT INTO vault_records (id, user_id, record_type, encrypted_blob, version, updated_at)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6)
       ON CONFLICT(id) DO UPDATE SET
         record_type = excluded.record_type,
         encrypted_blob = excluded.encrypted_blob,
         version = excluded.version,
         updated_at = excluded.updated_at`,
    )
      .bind(
        normalized.id,
        session.user_id,
        normalized.record_type,
        normalized.encrypted_blob,
        latestVersion,
        normalized.updated_at,
      )
      .run();
    pushed.push({ id: normalized.id, version: latestVersion });
  }

  await env.DB.prepare(
    "UPDATE sync_state SET latest_version = ?2 WHERE user_id = ?1",
  )
    .bind(session.user_id, latestVersion)
    .run();

  return json({ latest_version: latestVersion, records: pushed });
}

async function readAccountRequest(
  request: Request,
): Promise<{ username: string; password: string }> {
  const body = (await request.json().catch(() => null)) as AccountRequest | null;
  if (!body || typeof body.username !== "string" || typeof body.password !== "string") {
    throw new HttpError(400, "username_and_password_required");
  }
  const username = body.username.trim();
  if (!/^[A-Za-z0-9_.-]{3,64}$/.test(username)) {
    throw new HttpError(400, "invalid_username");
  }
  if (body.password.length < 12) {
    throw new HttpError(400, "password_too_short");
  }
  return { username, password: body.password };
}

function validateUploadRecord(record: UploadRecord): {
  id: string;
  record_type: string;
  encrypted_blob: string;
  updated_at: number;
} {
  if (
    typeof record.id !== "string" ||
    typeof record.record_type !== "string" ||
    typeof record.encrypted_blob !== "string" ||
    typeof record.updated_at !== "number"
  ) {
    throw new HttpError(400, "invalid_record");
  }
  if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(record.id)) {
    throw new HttpError(400, "invalid_record_id");
  }
  if (record.record_type !== "alias") {
    throw new HttpError(400, "invalid_record_type");
  }
  if (record.encrypted_blob.length < 32 || record.encrypted_blob.length > 64_000) {
    throw new HttpError(400, "invalid_encrypted_blob");
  }
  return {
    id: record.id,
    record_type: record.record_type,
    encrypted_blob: record.encrypted_blob,
    updated_at: Math.trunc(record.updated_at),
  };
}

async function requireSession(request: Request, env: Env): Promise<Session> {
  const authorization = request.headers.get("Authorization") ?? "";
  const match = authorization.match(/^Bearer\s+(.+)$/i);
  if (!match) {
    throw new HttpError(401, "missing_token");
  }
  const tokenHash = await sha256Base64Url(match[1]);
  const session = await env.DB.prepare(
    "SELECT user_id FROM sessions WHERE token_hash = ?1 AND expires_at > ?2",
  )
    .bind(tokenHash, nowUnix())
    .first<Session>();
  if (!session) {
    throw new HttpError(401, "invalid_token");
  }
  return session;
}

async function createSession(env: Env, userId: string): Promise<string> {
  const token = randomBase64Url(32);
  const createdAt = nowUnix();
  await env.DB.prepare(
    "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
  )
    .bind(
      await sha256Base64Url(token),
      userId,
      createdAt,
      createdAt + SESSION_TTL_SECONDS,
    )
    .run();
  return token;
}

async function syncState(env: Env, userId: string): Promise<number> {
  const state = await env.DB.prepare(
    "SELECT latest_version FROM sync_state WHERE user_id = ?1",
  )
    .bind(userId)
    .first<{ latest_version: number }>();
  return state?.latest_version ?? 0;
}

type AuthRateLimitRule = {
  scope: "username" | "ip";
  limit: number;
  windowSeconds: number;
};

async function enforceAuthRateLimit(
  request: Request,
  env: Env,
  action: "login" | "register",
  username: string,
  rules: AuthRateLimitRule[],
): Promise<void> {
  const ip = clientIp(request);

  await env.DB.prepare("DELETE FROM auth_rate_limits WHERE updated_at < ?1")
    .bind(nowUnix() - RATE_LIMIT_RETENTION_SECONDS)
    .run();

  for (const rule of rules) {
    const key = await authRateLimitKey(action, rule.scope, username, ip);
    await hitRateLimit(env, key, rule.limit, rule.windowSeconds);
  }
}

async function hitRateLimit(
  env: Env,
  key: string,
  limit: number,
  windowSeconds: number,
): Promise<void> {
  const timestamp = nowUnix();
  const existing = await env.DB.prepare(
    "SELECT attempts, window_start FROM auth_rate_limits WHERE key = ?1",
  )
    .bind(key)
    .first<{ attempts: number; window_start: number }>();

  if (!existing || timestamp - existing.window_start >= windowSeconds) {
    await env.DB.prepare(
      "INSERT INTO auth_rate_limits (key, attempts, window_start, updated_at) VALUES (?1, 1, ?2, ?2) ON CONFLICT(key) DO UPDATE SET attempts = 1, window_start = excluded.window_start, updated_at = excluded.updated_at",
    )
      .bind(key, timestamp)
      .run();
    return;
  }

  if (existing.attempts >= limit) {
    throw new HttpError(429, "rate_limited");
  }

  await env.DB.prepare(
    "UPDATE auth_rate_limits SET attempts = attempts + 1, updated_at = ?2 WHERE key = ?1",
  )
    .bind(key, timestamp)
    .run();
}

async function clearAuthRateLimit(
  env: Env,
  action: "login" | "register",
  username: string,
  ip: string,
): Promise<void> {
  await env.DB.batch([
    env.DB.prepare("DELETE FROM auth_rate_limits WHERE key = ?1").bind(
      await authRateLimitKey(action, "username", username, ip),
    ),
    env.DB.prepare("DELETE FROM auth_rate_limits WHERE key = ?1").bind(
      await authRateLimitKey(action, "ip", username, ip),
    ),
  ]);
}

async function authRateLimitKey(
  action: "login" | "register",
  scope: "username" | "ip",
  username: string,
  ip: string,
): Promise<string> {
  const value = scope === "username" ? username.toLowerCase() : ip;
  return `auth:${action}:${scope}:${await sha256Base64Url(value)}`;
}

function clientIp(request: Request): string {
  return (
    request.headers.get("CF-Connecting-IP") ??
    request.headers.get("True-Client-IP") ??
    "unknown"
  );
}

async function hashPassword(password: string): Promise<string> {
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const hash = await argon2idAsync(password, salt, ARGON2_PARAMS);
  return `argon2id$v=19$m=${ARGON2_PARAMS.m},t=${ARGON2_PARAMS.t},p=${ARGON2_PARAMS.p}$${base64Url(salt)}$${base64Url(hash)}`;
}

async function verifyPassword(password: string, stored: string): Promise<boolean> {
  if (stored.startsWith("argon2id$")) {
    return verifyArgon2idPassword(password, stored);
  }

  const [algorithm, iterationsText, saltText, hashText] = stored.split("$");
  if (algorithm !== "pbkdf2_sha256") {
    return false;
  }
  const iterations = Number(iterationsText);
  const salt = parseBase64Url(saltText);
  const expected = parseBase64Url(hashText);
  const actual = await pbkdf2(password, salt, iterations);
  return constantTimeEqual(actual, expected);
}

async function verifyArgon2idPassword(
  password: string,
  stored: string,
): Promise<boolean> {
  const [algorithm, version, paramsText, saltText, hashText] = stored.split("$");
  if (algorithm !== "argon2id" || version !== "v=19") {
    return false;
  }
  const params = parseArgon2Params(paramsText);
  const salt = parseBase64Url(saltText);
  const expected = parseBase64Url(hashText);
  const actual = await argon2idAsync(password, salt, {
    ...params,
    dkLen: expected.length,
    maxmem: ARGON2_PARAMS.maxmem,
  });
  return constantTimeEqual(actual, expected);
}

function parseArgon2Params(value: string): { m: number; t: number; p: number } {
  const pairs = new Map(
    value.split(",").map((part) => {
      const [key, raw] = part.split("=");
      return [key, Number(raw)];
    }),
  );
  const m = pairs.get("m");
  const t = pairs.get("t");
  const p = pairs.get("p");
  if (!m || !t || !p || m < 1024 || t < 1 || p < 1) {
    throw new HttpError(400, "invalid_password_hash");
  }
  return { m, t, p };
}

async function pbkdf2(
  password: string,
  salt: Uint8Array,
  iterations: number,
): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(password),
    "PBKDF2",
    false,
    ["deriveBits"],
  );
  const bits = await crypto.subtle.deriveBits(
    { name: "PBKDF2", hash: "SHA-256", salt, iterations },
    key,
    256,
  );
  return new Uint8Array(bits);
}

async function sha256Base64Url(value: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  );
  return base64Url(new Uint8Array(digest));
}

function constantTimeEqual(a: Uint8Array, b: Uint8Array): boolean {
  let diff = a.length ^ b.length;
  for (let index = 0; index < Math.max(a.length, b.length); index += 1) {
    diff |= (a[index] ?? 0) ^ (b[index] ?? 0);
  }
  return diff === 0;
}

function parseVersion(value: string | null): number {
  if (value === null) {
    return 0;
  }
  const version = Number(value);
  if (!Number.isInteger(version) || version < 0) {
    throw new HttpError(400, "invalid_version");
  }
  return version;
}

function randomBase64Url(bytes: number): string {
  return base64Url(crypto.getRandomValues(new Uint8Array(bytes)));
}

function base64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function parseBase64Url(value: string): Uint8Array {
  const padded = value.replaceAll("-", "+").replaceAll("_", "/").padEnd(
    Math.ceil(value.length / 4) * 4,
    "=",
  );
  const binary = atob(padded);
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
}

function nowUnix(): number {
  return Math.floor(Date.now() / 1000);
}

function json(body: unknown, status = 200): Response {
  return Response.json(body, { status, headers: corsHeaders() });
}

function corsHeaders(): HeadersInit {
  return {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Headers": "Authorization, Content-Type",
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
  };
}

class HttpError extends Error {
  readonly isHttpError = true;

  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

function isHttpError(error: unknown): error is HttpError {
  return (
    error instanceof HttpError ||
    (typeof error === "object" &&
      error !== null &&
      "isHttpError" in error &&
      "status" in error &&
      (error as { isHttpError?: unknown }).isHttpError === true &&
      typeof (error as { status?: unknown }).status === "number")
  );
}
