export interface Env {
  DB: D1Database;
  SESSION_BUCKET: R2Bucket;
  REGISTER_RATE_LIMITER?: RateLimitBinding;
  OBJECT_PREFIX?: string;
  REGISTRATION_INVITE_CODE?: string;
}

type AuthContext = {
  userId: string;
  deviceId: string;
};

type DeviceTokenAuth = {
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
  syncKeyProof?: string;
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

type ChunkDescriptor = {
  index: number;
  size: number;
  sha256: string;
  key: string;
};

type ChunkCompleteDescriptor = {
  index?: number;
  size?: number;
  sha256?: string;
};

type ChunkCompleteBody = {
  chunks?: ChunkCompleteDescriptor[];
};

type ChunkedBlobManifest = {
  format: string;
  sessionId: string;
  rawSha256: string;
  encryptedSha256: string;
  encryptedSize: number;
  chunks: ChunkDescriptor[];
};

const ENVELOPE_FORMAT = "codex-tools-session-v1";
const CHUNK_MANIFEST_FORMAT = "codex-tools-chunk-manifest-v1";
const MAX_UPLOAD_BYTES = 50 * 1024 * 1024;
const MAX_CHUNKED_UPLOAD_BYTES = 1024 * 1024 * 1024;
const MAX_CHUNK_COUNT = 256;
const CHUNK_MANIFEST_SUFFIX = ".chunks.json";
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

      const chunkMatch = path.match(/^\/v1\/sessions\/([^/]+)\/versions\/([a-f0-9]{64})\/chunks\/(\d+)$/);
      if (chunkMatch) {
        const sessionId = decodeSessionId(chunkMatch[1]);
        const rawSha256 = chunkMatch[2];
        const chunkIndex = Number(chunkMatch[3]);
        if (request.method === "PUT") {
          return handlePutChunk(request, env, auth, sessionId, rawSha256, chunkIndex);
        }
        if (request.method === "GET") {
          return handleGetChunk(env, auth, sessionId, rawSha256, chunkIndex);
        }
      }

      const chunkManifestMatch = path.match(/^\/v1\/sessions\/([^/]+)\/versions\/([a-f0-9]{64})\/chunks\/manifest$/);
      if (request.method === "GET" && chunkManifestMatch) {
        return handleGetChunkManifest(env, auth, decodeSessionId(chunkManifestMatch[1]), chunkManifestMatch[2]);
      }

      const completeMatch = path.match(/^\/v1\/sessions\/([^/]+)\/versions\/([a-f0-9]{64})\/chunks\/complete$/);
      if (request.method === "POST" && completeMatch) {
        return handleCompleteChunkedVersion(
          request,
          env,
          auth,
          decodeSessionId(completeMatch[1]),
          completeMatch[2]
        );
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
  await enforceRegisterRateLimit(request, env);

  const body = await request.json().catch(() => null) as RegisterBody | null;
  const email = normalizeEmail(body?.email);
  if (!email) {
    return json({ ok: false, error: "email_required" }, 400);
  }
  validateInviteCode(env, registerInviteCode(request, body));
  const syncKeyProof = normalizeSyncKeyProof(body?.syncKeyProof);
  if (!syncKeyProof) {
    return json({ ok: false, error: "sync_key_required" }, 400);
  }

  const now = Date.now();
  const userId = `usr_${randomId()}`;
  const deviceId = `dev_${randomId()}`;
  const deviceToken = `ctd_${randomId()}_${randomId()}`;
  const tokenHash = await sha256Hex(deviceToken);
  const deviceName = cleanText(body?.deviceName, 120) || "unknown-device";
  const platform = cleanText(body?.platform, 80);
  const requestedSyncKeyHash = syncKeyProof ? await syncKeyHash(email, syncKeyProof) : null;

  const existing = await env.DB.prepare("SELECT id, sync_key_hash FROM users WHERE email = ?1")
    .bind(email)
    .first<{ id: string; sync_key_hash: string | null }>();
  const finalUserId = existing?.id || userId;

  if (existing?.sync_key_hash) {
    if (!requestedSyncKeyHash || !timingSafeEqual(requestedSyncKeyHash, existing.sync_key_hash)) {
      return json({ ok: false, error: "invalid_sync_key" }, 403);
    }
  } else if (existing) {
    const tokenAuth = await authenticateDeviceTokenOnly(request, env);
    if (!tokenAuth || tokenAuth.userId !== existing.id) {
      return json({ ok: false, error: "sync_key_not_set" }, 403);
    }
    await env.DB.prepare("UPDATE users SET sync_key_hash = ?1 WHERE id = ?2")
      .bind(requestedSyncKeyHash, existing.id)
      .run();
    await audit(env, existing.id, tokenAuth.deviceId, "user.sync_key.set", email);
  } else if (!existing) {
    await env.DB.prepare(
      "INSERT INTO users (id, email, sync_key_hash, created_at_ms) VALUES (?1, ?2, ?3, ?4)"
    ).bind(finalUserId, email, requestedSyncKeyHash, now).run();
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

function forceUpload(request: Request): boolean {
  return (request.headers.get("x-codex-tools-force") || "").toLowerCase() === "true";
}

async function handleListSessions(
  request: Request,
  env: Env,
  auth: AuthContext
): Promise<Response> {
  const url = new URL(request.url);
  const limit = Math.min(Math.max(Number(url.searchParams.get("limit") || 100), 1), 500);
  const offset = Math.max(Number(url.searchParams.get("offset") || 0), 0);
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
     LIMIT ?2 OFFSET ?3`
  ).bind(auth.userId, limit, offset).all<Record<string, unknown>>();

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

  if (!forceUpload(request)) {
    const existing = await env.DB.prepare(
      `SELECT * FROM session_versions
       WHERE user_id = ?1 AND session_id = ?2 AND raw_sha256 = ?3
       LIMIT 1`
    ).bind(auth.userId, sessionId, rawSha256).first<Record<string, unknown>>();
    if (existing) {
      await audit(env, auth.userId, auth.deviceId, "session.upload.skip_existing", sessionId);
      return json({
        ok: true,
        skipped: true,
        manifest: rowToManifest(existing)
      });
    }
  }

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

  const blobKey = `${objectPrefix(env)}/${auth.userId}/sessions/${sessionId}/versions/${rawSha256}.jsonl.zst.enc`;

  await env.SESSION_BUCKET.put(blobKey, encrypted, {
    httpMetadata: { contentType: "application/octet-stream" },
    customMetadata: {
      userId: auth.userId,
      sessionId,
      rawSha256,
      encryptedSha256
    }
  });

  await saveVersionMetadata(env, auth, sessionId, rawSha256, manifest, blobKey, encryptedSha256, encrypted.byteLength);
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

async function saveVersionMetadata(
  env: Env,
  auth: AuthContext,
  sessionId: string,
  rawSha256: string,
  manifest: SessionManifest,
  blobKey: string,
  encryptedSha256: string,
  encryptedSize: number
): Promise<void> {
  const now = Date.now();
  const sessionRowId = `ses_${auth.userId}_${sessionId}`.replace(/[^A-Za-z0-9_:-]/g, "_");
  const sourceDir = manifest.sourceDir === "archived_sessions" ? "archived_sessions" : "sessions";
  const title = cleanText(manifest.title, 500);
  const cwd = cleanText(manifest.cwd, 2000);
  const providerName = cleanText(manifest.providerName, 200);
  const model = cleanText(manifest.model, 200);
  const relativePath = cleanText(manifest.relativePath, 4000) || `${sourceDir}/${sessionId}.jsonl`;

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
    encryptedSize,
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
}

async function handlePutChunk(
  request: Request,
  env: Env,
  auth: AuthContext,
  sessionId: string,
  rawSha256: string,
  chunkIndex: number
): Promise<Response> {
  validateSessionId(sessionId);
  validateChunkIndex(chunkIndex);
  const manifest = parseManifestHeader(request.headers.get("x-codex-tools-manifest"));
  validateManifest(manifest, sessionId, rawSha256);
  validateChunkedManifest(manifest);

  const expectedSha256 = request.headers.get("x-codex-tools-chunk-sha256") || "";
  const expectedSize = Number(request.headers.get("x-codex-tools-chunk-size") || 0);
  if (!/^[a-f0-9]{64}$/.test(expectedSha256)) {
    return json({ ok: false, error: "invalid_chunk_sha256" }, 400);
  }

  const chunk = await request.arrayBuffer();
  if (chunk.byteLength <= 0) {
    return json({ ok: false, error: "empty_chunk" }, 400);
  }
  if (chunk.byteLength > MAX_UPLOAD_BYTES) {
    return json({ ok: false, error: "chunk_too_large", maxBytes: MAX_UPLOAD_BYTES }, 413);
  }
  if (!Number.isSafeInteger(expectedSize) || expectedSize !== chunk.byteLength) {
    return json({ ok: false, error: "chunk_size_mismatch" }, 400);
  }
  const chunkSha256 = await sha256Hex(chunk);
  if (chunkSha256 !== expectedSha256) {
    return json({ ok: false, error: "chunk_sha256_mismatch" }, 400);
  }

  const key = chunkKey(env, auth.userId, sessionId, rawSha256, chunkIndex);
  await env.SESSION_BUCKET.put(key, chunk, {
    httpMetadata: { contentType: "application/octet-stream" },
    customMetadata: {
      userId: auth.userId,
      sessionId,
      rawSha256,
      chunkIndex: String(chunkIndex),
      chunkSha256
    }
  });
  await audit(env, auth.userId, auth.deviceId, "session.upload.chunk", `${sessionId}:${chunkIndex}`);
  return json({
    ok: true,
    chunk: {
      index: chunkIndex,
      size: chunk.byteLength,
      sha256: chunkSha256
    }
  });
}

async function handleCompleteChunkedVersion(
  request: Request,
  env: Env,
  auth: AuthContext,
  sessionId: string,
  rawSha256: string
): Promise<Response> {
  const manifest = parseManifestHeader(request.headers.get("x-codex-tools-manifest"));
  validateManifest(manifest, sessionId, rawSha256);
  validateChunkedManifest(manifest);

  if (!forceUpload(request)) {
    const existing = await env.DB.prepare(
      `SELECT * FROM session_versions
       WHERE user_id = ?1 AND session_id = ?2 AND raw_sha256 = ?3
       LIMIT 1`
    ).bind(auth.userId, sessionId, rawSha256).first<Record<string, unknown>>();
    if (existing) {
      await audit(env, auth.userId, auth.deviceId, "session.upload.skip_existing", sessionId);
      return json({
        ok: true,
        skipped: true,
        manifest: rowToManifest(existing)
      });
    }
  }

  const body = await request.json().catch(() => null) as ChunkCompleteBody | null;
  const requestedChunks = body?.chunks || [];
  if (!Array.isArray(requestedChunks) || requestedChunks.length <= 0) {
    return json({ ok: false, error: "chunks_required" }, 400);
  }
  if (requestedChunks.length > MAX_CHUNK_COUNT) {
    return json({ ok: false, error: "too_many_chunks", maxChunks: MAX_CHUNK_COUNT }, 400);
  }

  const chunks: ChunkDescriptor[] = [];
  let totalSize = 0;
  for (let expectedIndex = 0; expectedIndex < requestedChunks.length; expectedIndex += 1) {
    const chunk = requestedChunks[expectedIndex];
    const index = Number(chunk.index);
    const size = Number(chunk.size);
    const sha256 = String(chunk.sha256 || "");
    if (index !== expectedIndex || !Number.isSafeInteger(size) || size <= 0 || !/^[a-f0-9]{64}$/.test(sha256)) {
      return json({ ok: false, error: "invalid_chunk_descriptor", index: expectedIndex }, 400);
    }
    const key = chunkKey(env, auth.userId, sessionId, rawSha256, index);
    const object = await env.SESSION_BUCKET.head(key);
    if (!object) {
      return json({ ok: false, error: "missing_chunk", index }, 400);
    }
    if (object.size !== size || object.customMetadata?.chunkSha256 !== sha256) {
      return json({ ok: false, error: "chunk_metadata_mismatch", index }, 400);
    }
    totalSize += size;
    chunks.push({ index, size, sha256, key });
  }
  if (totalSize !== manifest.encryptedSize) {
    return json({ ok: false, error: "encrypted_size_mismatch" }, 400);
  }

  const blobKey = chunkManifestKey(env, auth.userId, sessionId, rawSha256);
  const chunkManifest: ChunkedBlobManifest = {
    format: CHUNK_MANIFEST_FORMAT,
    sessionId,
    rawSha256,
    encryptedSha256: manifest.encryptedSha256,
    encryptedSize: manifest.encryptedSize,
    chunks
  };
  await env.SESSION_BUCKET.put(blobKey, JSON.stringify(chunkManifest), {
    httpMetadata: { contentType: "application/json" },
    customMetadata: {
      userId: auth.userId,
      sessionId,
      rawSha256,
      encryptedSha256: manifest.encryptedSha256,
      chunkCount: String(chunks.length)
    }
  });

  await saveVersionMetadata(env, auth, sessionId, rawSha256, manifest, blobKey, manifest.encryptedSha256, manifest.encryptedSize);
  await audit(env, auth.userId, auth.deviceId, "session.upload.chunked", sessionId);
  return json({
    ok: true,
    chunked: true,
    manifest: {
      ...manifest,
      blobKey,
      encryptedSize: manifest.encryptedSize
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
  if (blobKey.endsWith(CHUNK_MANIFEST_SUFFIX)) {
    const chunkManifest = JSON.parse(await object.text()) as ChunkedBlobManifest;
    validateStoredChunkManifest(chunkManifest, sessionId, rawSha256);
    const stream = streamChunks(env, chunkManifest);
    return withCors(new Response(stream, {
      headers: {
        "content-type": "application/octet-stream",
        "x-codex-tools-manifest": btoa(JSON.stringify(rowToManifest(row)))
      }
    }));
  }
  return withCors(new Response(object.body, {
    headers: {
      "content-type": "application/octet-stream",
      "x-codex-tools-manifest": btoa(JSON.stringify(rowToManifest(row)))
    }
  }));
}

async function handleGetChunk(
  env: Env,
  auth: AuthContext,
  sessionId: string,
  rawSha256: string,
  chunkIndex: number
): Promise<Response> {
  validateSessionId(sessionId);
  validateChunkIndex(chunkIndex);
  const row = await env.DB.prepare(
    `SELECT * FROM session_versions
     WHERE user_id = ?1 AND session_id = ?2 AND raw_sha256 = ?3
     LIMIT 1`
  ).bind(auth.userId, sessionId, rawSha256).first<Record<string, unknown>>();
  if (!row) {
    return json({ ok: false, error: "version_not_found" }, 404);
  }
  const blobKey = String(row.blob_key || "");
  if (!blobKey.endsWith(CHUNK_MANIFEST_SUFFIX)) {
    return json({ ok: false, error: "version_not_chunked" }, 400);
  }
  const object = await env.SESSION_BUCKET.get(chunkKey(env, auth.userId, sessionId, rawSha256, chunkIndex));
  if (!object) {
    return json({ ok: false, error: "chunk_not_found", index: chunkIndex }, 404);
  }
  return withCors(new Response(object.body, {
    headers: { "content-type": "application/octet-stream" }
  }));
}

async function handleGetChunkManifest(
  env: Env,
  auth: AuthContext,
  sessionId: string,
  rawSha256: string
): Promise<Response> {
  validateSessionId(sessionId);
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
  if (!blobKey.endsWith(CHUNK_MANIFEST_SUFFIX)) {
    return json({ ok: false, error: "version_not_chunked" }, 400);
  }
  const object = await env.SESSION_BUCKET.get(blobKey);
  if (!object) {
    return json({ ok: false, error: "chunk_manifest_not_found" }, 404);
  }
  const chunkManifest = JSON.parse(await object.text()) as ChunkedBlobManifest;
  validateStoredChunkManifest(chunkManifest, sessionId, rawSha256);
  await audit(env, auth.userId, auth.deviceId, "session.download.chunk_manifest", sessionId);
  return json({
    ok: true,
    chunks: chunkManifest.chunks.map(chunk => ({
      index: chunk.index,
      size: chunk.size,
      sha256: chunk.sha256
    }))
  });
}

async function authenticate(request: Request, env: Env): Promise<AuthContext> {
  const tokenAuth = await authenticateDeviceTokenOnly(request, env);
  if (!tokenAuth) {
    throw new HttpError(401, "invalid_device_token");
  }
  const row = await env.DB.prepare(
    `SELECT email, sync_key_hash, disabled_at_ms
     FROM users
     WHERE id = ?1
     LIMIT 1`
  ).bind(tokenAuth.userId).first<{ email: string; sync_key_hash: string | null; disabled_at_ms: number | null }>();
  if (!row || row.disabled_at_ms) {
    throw new HttpError(403, "user_disabled");
  }
  if (!row.sync_key_hash) {
    throw new HttpError(403, "sync_key_not_set");
  }
  const syncKeyProof = normalizeSyncKeyProof(request.headers.get("x-codex-tools-sync-key-proof"));
  if (!syncKeyProof) {
    throw new HttpError(401, "missing_sync_key_proof");
  }
  const requestedSyncKeyHash = await syncKeyHash(row.email, syncKeyProof);
  if (!timingSafeEqual(requestedSyncKeyHash, row.sync_key_hash)) {
    throw new HttpError(403, "invalid_sync_key");
  }
  await env.DB.prepare("UPDATE devices SET last_seen_at_ms = ?1 WHERE id = ?2")
    .bind(Date.now(), tokenAuth.deviceId)
    .run();
  return { userId: tokenAuth.userId, deviceId: tokenAuth.deviceId };
}

async function authenticateDeviceTokenOnly(request: Request, env: Env): Promise<DeviceTokenAuth | null> {
  const token = bearerToken(request);
  if (!token) {
    return null;
  }
  const tokenHash = await sha256Hex(token);
  const row = await env.DB.prepare(
    `SELECT id, user_id
     FROM devices
     WHERE token_hash = ?1 AND revoked_at_ms IS NULL
     LIMIT 1`
  ).bind(tokenHash).first<{ id: string; user_id: string }>();
  if (!row) {
    return null;
  }
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

function validateChunkedManifest(manifest: SessionManifest): void {
  if (!Number.isSafeInteger(manifest.encryptedSize) || manifest.encryptedSize <= MAX_UPLOAD_BYTES) {
    throw new HttpError(400, "invalid_chunked_encrypted_size");
  }
  if (manifest.encryptedSize > MAX_CHUNKED_UPLOAD_BYTES) {
    throw new HttpError(413, "chunked_upload_too_large");
  }
}

function validateChunkIndex(index: number): void {
  if (!Number.isSafeInteger(index) || index < 0 || index >= MAX_CHUNK_COUNT) {
    throw new HttpError(400, "invalid_chunk_index");
  }
}

function validateStoredChunkManifest(manifest: ChunkedBlobManifest, sessionId: string, rawSha256: string): void {
  if (manifest.format !== CHUNK_MANIFEST_FORMAT || manifest.sessionId !== sessionId || manifest.rawSha256 !== rawSha256) {
    throw new HttpError(500, "invalid_chunk_manifest");
  }
  if (!Array.isArray(manifest.chunks) || manifest.chunks.length <= 0 || manifest.chunks.length > MAX_CHUNK_COUNT) {
    throw new HttpError(500, "invalid_chunk_manifest");
  }
  let totalSize = 0;
  for (let expectedIndex = 0; expectedIndex < manifest.chunks.length; expectedIndex += 1) {
    const chunk = manifest.chunks[expectedIndex];
    if (
      chunk.index !== expectedIndex
      || !Number.isSafeInteger(chunk.size)
      || chunk.size <= 0
      || !/^[a-f0-9]{64}$/.test(chunk.sha256 || "")
      || typeof chunk.key !== "string"
      || chunk.key.length <= 0
    ) {
      throw new HttpError(500, "invalid_chunk_manifest");
    }
    totalSize += chunk.size;
  }
  if (totalSize !== manifest.encryptedSize || !/^[a-f0-9]{64}$/.test(manifest.encryptedSha256 || "")) {
    throw new HttpError(500, "invalid_chunk_manifest");
  }
}

function chunkKey(env: Env, userId: string, sessionId: string, rawSha256: string, chunkIndex: number): string {
  return `${objectPrefix(env)}/${userId}/sessions/${sessionId}/versions/${rawSha256}/chunks/${chunkIndex}.part`;
}

function chunkManifestKey(env: Env, userId: string, sessionId: string, rawSha256: string): string {
  return `${objectPrefix(env)}/${userId}/sessions/${sessionId}/versions/${rawSha256}${CHUNK_MANIFEST_SUFFIX}`;
}

function streamChunks(env: Env, manifest: ChunkedBlobManifest): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    async start(controller) {
      try {
        for (const chunk of manifest.chunks) {
          const object = await env.SESSION_BUCKET.get(chunk.key);
          if (!object) {
            throw new HttpError(404, `chunk_not_found:${chunk.index}`);
          }
          const reader = object.body.getReader();
          while (true) {
            const { done, value } = await reader.read();
            if (done) {
              break;
            }
            if (value) {
              controller.enqueue(value);
            }
          }
        }
        controller.close();
      } catch (error) {
        controller.error(error);
      }
    }
  });
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

function normalizeSyncKeyProof(value: unknown): string | null {
  if (typeof value !== "string") {
    return null;
  }
  const proof = value.trim();
  if (!/^[a-f0-9]{64}$/.test(proof)) {
    return null;
  }
  return proof;
}

async function syncKeyHash(email: string, syncKeyProof: string): Promise<string> {
  return sha256Hex(`codex-tools-sync-login-hash-v1\0${email}\0${syncKeyProof}`);
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
      "x-codex-tools-invite-code",
      "x-codex-tools-sync-key-proof",
      "x-codex-tools-force",
      "x-codex-tools-chunk-sha256",
      "x-codex-tools-chunk-size"
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
