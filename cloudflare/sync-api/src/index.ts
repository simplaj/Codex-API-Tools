export interface Env {
  DB: D1Database;
  SESSION_BUCKET: R2Bucket;
  REGISTER_RATE_LIMITER?: RateLimitBinding;
  ADMIN_BOOTSTRAP_TOKEN?: string;
  OBJECT_PREFIX?: string;
  REGISTRATION_INVITE_CODE?: string;
}

type AuthContext = {
  userId: string;
  deviceId: string;
};

type RateLimitBinding = {
  limit(options: { key: string }): Promise<{ success: boolean }>;
};

type RegisterBody = {
  email?: string;
  deviceName?: string;
  platform?: string;
  inviteCode?: string;
};

type SessionManifest = {
  format: string;
  sessionId: string;
  relativePath: string;
  sourceDir: string;
  title?: string | null;
  cwd?: string | null;
  providerName?: string | null;
  model?: string | null;
  rawSha256: string;
  encryptedSha256: string;
  encryptedSize: number;
  blobKey?: string | null;
  uploadedAtMs: number;
  deviceName: string;
};

const ENVELOPE_FORMAT = "codex-tools-session-v1";
const MAX_UPLOAD_BYTES = 50 * 1024 * 1024;
const DEFAULT_INVITE_CODE = "sub2api.simplaj.top";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      if (request.method === "OPTIONS") {
        return withCors(new Response(null, { status: 204 }));
      }

      const url = new URL(request.url);
      const path = url.pathname.replace(/\/+$/, "") || "/";

      if (request.method === "GET" && path === "/v1/health") {
        return json({ ok: true, service: "codex-tools-sync-api" });
      }

      if (request.method === "POST" && path === "/v1/devices/register") {
        return handleRegister(request, env);
      }

      const auth = await authenticate(request, env);

      if (request.method === "GET" && path === "/v1/sessions") {
        return handleListSessions(request, env, auth);
      }

      const putMatch = path.match(/^\/v1\/sessions\/([^/]+)\/versions\/([a-f0-9]{64})$/);
      if (request.method === "PUT" && putMatch) {
        return handlePutVersion(request, env, auth, decodeSessionId(putMatch[1]), putMatch[2]);
      }

      const latestMatch = path.match(/^\/v1\/sessions\/([^/]+)\/versions\/latest$/);
      if (request.method === "GET" && latestMatch) {
        return handleLatestVersion(env, auth, decodeSessionId(latestMatch[1]));
      }

      const blobMatch = path.match(/^\/v1\/sessions\/([^/]+)\/versions\/([a-f0-9]{64})\/blob$/);
      if (request.method === "GET" && blobMatch) {
        return handleGetBlob(env, auth, decodeSessionId(blobMatch[1]), blobMatch[2]);
      }

      return json({ ok: false, error: "not_found" }, 404);
    } catch (error) {
      return errorResponse(error);
    }
  }
};

async function handleRegister(request: Request, env: Env): Promise<Response> {
  const expected = env.ADMIN_BOOTSTRAP_TOKEN;
  const bearer = bearerToken(request);
  const adminAuthenticated = Boolean(bearer && expected && timingSafeEqual(bearer, expected));
  if (bearer && !adminAuthenticated) {
    return json({ ok: false, error: "unauthorized" }, 401);
  }

  if (!adminAuthenticated) {
    await enforceRegisterRateLimit(request, env);
  }

  const body = await request.json().catch(() => null) as RegisterBody | null;
  const email = normalizeEmail(body?.email);
  if (!email) {
    return json({ ok: false, error: "email_required" }, 400);
  }
  if (!adminAuthenticated) {
    validateInviteCode(env, registerInviteCode(request, body));
  }

  const now = Date.now();
  const userId = `usr_${randomId()}`;
  const deviceId = `dev_${randomId()}`;
  const deviceToken = `ctd_${randomId()}_${randomId()}`;
  const tokenHash = await sha256Hex(deviceToken);
  const deviceName = cleanText(body?.deviceName, 120) || "unknown-device";
  const platform = cleanText(body?.platform, 80);

  const existing = await env.DB.prepare("SELECT id FROM users WHERE email = ?1")
    .bind(email)
    .first<{ id: string }>();
  const finalUserId = existing?.id || userId;
  if (!existing) {
    await env.DB.prepare(
      "INSERT INTO users (id, email, created_at_ms) VALUES (?1, ?2, ?3)"
    ).bind(finalUserId, email, now).run();
  }

  await env.DB.prepare(
    "INSERT INTO devices (id, user_id, name, platform, token_hash, created_at_ms, last_seen_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
  ).bind(deviceId, finalUserId, deviceName, platform, tokenHash, now, now).run();
  await audit(env, finalUserId, deviceId, "device.register", deviceName);

  return json({
    ok: true,
    userId: finalUserId,
    deviceId,
    deviceToken
  });
}

async function enforceRegisterRateLimit(request: Request, env: Env): Promise<void> {
  if (!env.REGISTER_RATE_LIMITER) {
    throw new HttpError(500, "register_rate_limit_not_configured");
  }
  const { success } = await env.REGISTER_RATE_LIMITER.limit({
    key: `register:${clientIp(request)}`
  });
  if (!success) {
    throw new HttpError(429, "rate_limited");
  }
}

function validateInviteCode(env: Env, inviteCode: string | null): void {
  const expected = normalizeInviteCode(env.REGISTRATION_INVITE_CODE || DEFAULT_INVITE_CODE);
  const received = normalizeInviteCode(inviteCode);
  if (!received || !timingSafeEqual(received, expected)) {
    throw new HttpError(403, "invalid_invite_code");
  }
}

async function handleListSessions(
  request: Request,
  env: Env,
  auth: AuthContext
): Promise<Response> {
  const url = new URL(request.url);
  const limit = Math.min(Math.max(Number(url.searchParams.get("limit") || 100), 1), 500);
  const rows = await env.DB.prepare(
    `SELECT sv.*
     FROM session_versions sv
     JOIN (
       SELECT session_id, MAX(uploaded_at_ms) AS uploaded_at_ms
       FROM session_versions
       WHERE user_id = ?1
       GROUP BY session_id
     ) latest
       ON latest.session_id = sv.session_id
      AND latest.uploaded_at_ms = sv.uploaded_at_ms
     WHERE sv.user_id = ?1
     ORDER BY sv.uploaded_at_ms DESC
     LIMIT ?2`
  ).bind(auth.userId, limit).all<Record<string, unknown>>();

  return json({
    ok: true,
    sessions: rows.results.map(rowToManifest)
  });
}

async function handlePutVersion(
  request: Request,
  env: Env,
  auth: AuthContext,
  sessionId: string,
  rawSha256: string
): Promise<Response> {
  const manifest = parseManifestHeader(request.headers.get("x-codex-tools-manifest"));
  validateManifest(manifest, sessionId, rawSha256);

  const encrypted = await request.arrayBuffer();
  if (encrypted.byteLength <= 0) {
    return json({ ok: false, error: "empty_upload" }, 400);
  }
  if (encrypted.byteLength > MAX_UPLOAD_BYTES) {
    return json({ ok: false, error: "upload_too_large", maxBytes: MAX_UPLOAD_BYTES }, 413);
  }
  if (encrypted.byteLength !== manifest.encryptedSize) {
    return json({ ok: false, error: "encrypted_size_mismatch" }, 400);
  }
  const encryptedSha256 = await sha256Hex(encrypted);
  if (encryptedSha256 !== manifest.encryptedSha256) {
    return json({ ok: false, error: "encrypted_sha256_mismatch" }, 400);
  }

  const now = Date.now();
  const sessionRowId = `ses_${auth.userId}_${sessionId}`.replace(/[^A-Za-z0-9_:-]/g, "_");
  const sourceDir = manifest.sourceDir === "archived_sessions" ? "archived_sessions" : "sessions";
  const blobKey = `${objectPrefix(env)}/${auth.userId}/sessions/${sessionId}/versions/${rawSha256}.jsonl.zst.enc`;
  const title = cleanText(manifest.title, 500);
  const cwd = cleanText(manifest.cwd, 2000);
  const providerName = cleanText(manifest.providerName, 200);
  const model = cleanText(manifest.model, 200);
  const relativePath = cleanText(manifest.relativePath, 4000) || `${sourceDir}/${sessionId}.jsonl`;

  await env.SESSION_BUCKET.put(blobKey, encrypted, {
    httpMetadata: { contentType: "application/octet-stream" },
    customMetadata: {
      userId: auth.userId,
      sessionId,
      rawSha256,
      encryptedSha256
    }
  });

  await env.DB.prepare(
    `INSERT INTO sessions (
       id, user_id, session_id, title, cwd, provider_name, model, source_dir, relative_path,
       archived, first_seen_at_ms, last_seen_at_ms, created_at_ms, updated_at_ms
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
     ON CONFLICT(user_id, session_id) DO UPDATE SET
       title = excluded.title,
       cwd = excluded.cwd,
       provider_name = excluded.provider_name,
       model = excluded.model,
       source_dir = excluded.source_dir,
       relative_path = excluded.relative_path,
       archived = excluded.archived,
       last_seen_at_ms = excluded.last_seen_at_ms,
       updated_at_ms = excluded.updated_at_ms`
  ).bind(
    sessionRowId,
    auth.userId,
    sessionId,
    title,
    cwd,
    providerName,
    model,
    sourceDir,
    relativePath,
    sourceDir === "archived_sessions" ? 1 : 0,
    manifest.uploadedAtMs || now,
    manifest.uploadedAtMs || now,
    now,
    now
  ).run();

  const versionId = `ver_${auth.userId}_${sessionId}_${rawSha256}`.replace(/[^A-Za-z0-9_:-]/g, "_");
  await env.DB.prepare(
    `INSERT INTO session_versions (
       id, user_id, session_row_id, session_id, device_id, raw_sha256, encrypted_sha256,
       encrypted_size, blob_key, relative_path, source_dir, title, cwd, provider_name, model,
       uploaded_at_ms, created_at_ms
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
     ON CONFLICT(user_id, session_id, raw_sha256) DO UPDATE SET
       encrypted_sha256 = excluded.encrypted_sha256,
       encrypted_size = excluded.encrypted_size,
       blob_key = excluded.blob_key,
       uploaded_at_ms = excluded.uploaded_at_ms`
  ).bind(
    versionId,
    auth.userId,
    sessionRowId,
    sessionId,
    auth.deviceId,
    rawSha256,
    encryptedSha256,
    encrypted.byteLength,
    blobKey,
    relativePath,
    sourceDir,
    title,
    cwd,
    providerName,
    model,
    manifest.uploadedAtMs || now,
    now
  ).run();
  await audit(env, auth.userId, auth.deviceId, "session.upload", sessionId);

  return json({
    ok: true,
    manifest: {
      ...manifest,
      blobKey,
      encryptedSha256,
      encryptedSize: encrypted.byteLength
    }
  });
}

async function handleLatestVersion(env: Env, auth: AuthContext, sessionId: string): Promise<Response> {
  const row = await env.DB.prepare(
    `SELECT * FROM session_versions
     WHERE user_id = ?1 AND session_id = ?2
     ORDER BY uploaded_at_ms DESC
     LIMIT 1`
  ).bind(auth.userId, sessionId).first<Record<string, unknown>>();
  if (!row) {
    return json({ ok: false, error: "session_not_found" }, 404);
  }
  return json({ ok: true, manifest: rowToManifest(row) });
}

async function handleGetBlob(
  env: Env,
  auth: AuthContext,
  sessionId: string,
  rawSha256: string
): Promise<Response> {
  const row = await env.DB.prepare(
    `SELECT * FROM session_versions
     WHERE user_id = ?1 AND session_id = ?2 AND raw_sha256 = ?3
     LIMIT 1`
  ).bind(auth.userId, sessionId, rawSha256).first<Record<string, unknown>>();
  if (!row) {
    return json({ ok: false, error: "version_not_found" }, 404);
  }
  const blobKey = String(row.blob_key || "");
  if (!blobKey.startsWith(`${objectPrefix(env)}/${auth.userId}/`)) {
    return json({ ok: false, error: "invalid_blob_key" }, 500);
  }
  const object = await env.SESSION_BUCKET.get(blobKey);
  if (!object) {
    return json({ ok: false, error: "blob_not_found" }, 404);
  }
  await audit(env, auth.userId, auth.deviceId, "session.download", sessionId);
  return withCors(new Response(object.body, {
    headers: {
      "content-type": "application/octet-stream",
      "x-codex-tools-manifest": btoa(JSON.stringify(rowToManifest(row)))
    }
  }));
}

async function authenticate(request: Request, env: Env): Promise<AuthContext> {
  const token = bearerToken(request);
  if (!token) {
    throw new HttpError(401, "missing_bearer_token");
  }
  const tokenHash = await sha256Hex(token);
  const row = await env.DB.prepare(
    `SELECT id, user_id
     FROM devices
     WHERE token_hash = ?1 AND revoked_at_ms IS NULL
     LIMIT 1`
  ).bind(tokenHash).first<{ id: string; user_id: string }>();
  if (!row) {
    throw new HttpError(401, "invalid_device_token");
  }
  await env.DB.prepare("UPDATE devices SET last_seen_at_ms = ?1 WHERE id = ?2")
    .bind(Date.now(), row.id)
    .run();
  return { userId: row.user_id, deviceId: row.id };
}

function rowToManifest(row: Record<string, unknown>): SessionManifest {
  return {
    format: ENVELOPE_FORMAT,
    sessionId: String(row.session_id || ""),
    relativePath: String(row.relative_path || ""),
    sourceDir: String(row.source_dir || "sessions"),
    title: nullableString(row.title),
    cwd: nullableString(row.cwd),
    providerName: nullableString(row.provider_name),
    model: nullableString(row.model),
    rawSha256: String(row.raw_sha256 || ""),
    encryptedSha256: String(row.encrypted_sha256 || ""),
    encryptedSize: Number(row.encrypted_size || 0),
    blobKey: String(row.blob_key || ""),
    uploadedAtMs: Number(row.uploaded_at_ms || 0),
    deviceName: ""
  };
}

function parseManifestHeader(value: string | null): SessionManifest {
  if (!value) {
    throw new HttpError(400, "missing_manifest_header");
  }
  try {
    return JSON.parse(atob(value)) as SessionManifest;
  } catch {
    throw new HttpError(400, "invalid_manifest_header");
  }
}

function validateManifest(manifest: SessionManifest, sessionId: string, rawSha256: string): void {
  validateSessionId(sessionId);
  if (manifest.format !== ENVELOPE_FORMAT) {
    throw new HttpError(400, "invalid_manifest_format");
  }
  if (manifest.sessionId !== sessionId) {
    throw new HttpError(400, "session_id_mismatch");
  }
  if (manifest.rawSha256 !== rawSha256) {
    throw new HttpError(400, "raw_sha256_mismatch");
  }
  if (!/^[a-f0-9]{64}$/.test(manifest.encryptedSha256 || "")) {
    throw new HttpError(400, "invalid_encrypted_sha256");
  }
}

function decodeSessionId(value: string): string {
  try {
    const sessionId = decodeURIComponent(value);
    validateSessionId(sessionId);
    return sessionId;
  } catch (error) {
    if (error instanceof HttpError) {
      throw error;
    }
    throw new HttpError(400, "invalid_session_id");
  }
}

function validateSessionId(sessionId: string): void {
  if (!/^[A-Za-z0-9._:-]{1,256}$/.test(sessionId)) {
    throw new HttpError(400, "invalid_session_id");
  }
}

async function audit(
  env: Env,
  userId: string | null,
  deviceId: string | null,
  eventType: string,
  target: string
): Promise<void> {
  await env.DB.prepare(
    "INSERT INTO audit_events (id, user_id, device_id, event_type, target, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
  ).bind(`aud_${randomId()}`, userId, deviceId, eventType, target, Date.now()).run();
}

function bearerToken(request: Request): string | null {
  const header = request.headers.get("authorization") || "";
  const match = header.match(/^Bearer\s+(.+)$/i);
  return match?.[1]?.trim() || null;
}

function registerInviteCode(request: Request, body: RegisterBody | null): string | null {
  return cleanText(body?.inviteCode, 200)
    || cleanText(headerValue(request, "x-codex-tools-invite-code"), 200)
    || cleanText(new URL(request.url).searchParams.get("invite_code"), 200);
}

function headerValue(request: Request, name: string): string | null {
  const value = request.headers.get(name);
  return value && value.trim() ? value.trim() : null;
}

function clientIp(request: Request): string {
  const forwarded = request.headers.get("x-forwarded-for") || "";
  return request.headers.get("cf-connecting-ip")
    || forwarded.split(",").map(value => value.trim()).find(Boolean)
    || request.headers.get("x-real-ip")
    || "unknown";
}

function normalizeInviteCode(value: unknown): string {
  return typeof value === "string" ? value.trim().toLowerCase() : "";
}

async function sha256Hex(value: string | ArrayBuffer): Promise<string> {
  const input = typeof value === "string" ? new TextEncoder().encode(value) : value;
  const hash = await crypto.subtle.digest("SHA-256", input);
  return [...new Uint8Array(hash)].map(byte => byte.toString(16).padStart(2, "0")).join("");
}

function timingSafeEqual(left: string, right: string): boolean {
  if (left.length !== right.length) {
    return false;
  }
  let result = 0;
  for (let index = 0; index < left.length; index += 1) {
    result |= left.charCodeAt(index) ^ right.charCodeAt(index);
  }
  return result === 0;
}

function randomId(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return [...bytes].map(byte => byte.toString(16).padStart(2, "0")).join("");
}

function normalizeEmail(value: unknown): string | null {
  if (typeof value !== "string") {
    return null;
  }
  const email = value.trim().toLowerCase();
  if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) {
    return null;
  }
  return email;
}

function cleanText(value: unknown, maxLength: number): string | null {
  if (typeof value !== "string") {
    return null;
  }
  const text = value.trim();
  if (!text) {
    return null;
  }
  return text.slice(0, maxLength);
}

function nullableString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function objectPrefix(env: Env): string {
  return (env.OBJECT_PREFIX || "users").replace(/^\/+|\/+$/g, "");
}

function json(body: unknown, status = 200): Response {
  return withCors(new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" }
  }));
}

function withCors(response: Response): Response {
  const next = new Response(response.body, response);
  next.headers.set("access-control-allow-origin", "*");
  next.headers.set("access-control-allow-methods", "GET,POST,PUT,OPTIONS");
  next.headers.set(
    "access-control-allow-headers",
    [
      "authorization",
      "content-type",
      "x-codex-tools-manifest",
      "x-codex-tools-invite-code"
    ].join(",")
  );
  return next;
}

function errorResponse(error: unknown): Response {
  if (error instanceof HttpError) {
    return json({ ok: false, error: error.message }, error.status);
  }
  const message = error instanceof Error ? error.message : String(error);
  return json({ ok: false, error: "internal_error", message }, 500);
}

class HttpError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}
