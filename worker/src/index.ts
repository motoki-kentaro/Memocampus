import { Hono } from 'hono';
import { SignJWT, jwtVerify } from 'jose';
import { getCookie, setCookie, deleteCookie } from 'hono/cookie';

type Bindings = {
  MEMOCAMPUS: Fetcher;
  OAUTH_KV: KVNamespace;
  GOOGLE_CLIENT_ID: string;
  GOOGLE_CLIENT_SECRET: string;
  ALLOWED_EMAIL: string;
  ALLOWED_SUB: string;
  JWT_SECRET: string;
  MEMOCAMPUS_UPSTREAM_TOKEN: string;
};

const app = new Hono<{ Bindings: Bindings }>();

// ----------------------------------------------------
// 0. OAuth 2.0 メタデータ・ディスカバリ (RFC 8414)
// ----------------------------------------------------
const metadataHandler = (c: any) => {
  const origin = new URL(c.req.url).origin;
  return c.json({
    issuer: origin,
    authorization_endpoint: `${origin}/oauth/authorize`,
    token_endpoint: `${origin}/oauth/token`,
    response_types_supported: ['code'],
    grant_types_supported: ['authorization_code'],
    token_endpoint_auth_methods_supported: ['client_secret_post', 'client_secret_basic', 'none'],
  });
};

app.get('/.well-known/oauth-authorization-server', metadataHandler);
app.get('/.well-known/openid-configuration', metadataHandler);

// ----------------------------------------------------
// 1. OAuth 認可エンドポイント
// ----------------------------------------------------
app.get('/oauth/authorize', async (c) => {
  const clientRedirectUri = c.req.query('redirect_uri');
  const clientState = c.req.query('state') || '';

  if (!clientRedirectUri) {
    return c.text('Invalid request', 400);
  }

  const sessionState = crypto.randomUUID();

  // ブラウザへCookie発行 (HttpOnly, Secure, SameSite=Lax, 5分期限)
  setCookie(c, 'oauth_state', sessionState, {
    path: '/oauth',
    secure: true,
    httpOnly: true,
    sameSite: 'Lax',
    maxAge: 300,
  });

  // KVに保存 (5分期限)
  await c.env.OAUTH_KV.put(
    `state:${sessionState}`,
    JSON.stringify({ clientRedirectUri, clientState }),
    { expirationTtl: 300 }
  );

  const workerCallback = `https://${new URL(c.req.url).host}/oauth/callback`;
  const googleAuthUrl = new URL('https://accounts.google.com/o/oauth2/v2/auth');
  googleAuthUrl.searchParams.set('client_id', c.env.GOOGLE_CLIENT_ID);
  googleAuthUrl.searchParams.set('redirect_uri', workerCallback);
  googleAuthUrl.searchParams.set('response_type', 'code');
  googleAuthUrl.searchParams.set('scope', 'openid email profile');
  googleAuthUrl.searchParams.set('state', sessionState);
  googleAuthUrl.searchParams.set('prompt', 'select_account');

  return c.redirect(googleAuthUrl.toString());
});

// ----------------------------------------------------
// 2. Google OAuth コールバック
// ----------------------------------------------------
app.get('/oauth/callback', async (c) => {
  const queryCode = c.req.query('code');
  const queryState = c.req.query('state');
  const cookieState = getCookie(c, 'oauth_state');

  // Cookieは検証有無を問わず即座に削除
  deleteCookie(c, 'oauth_state', { path: '/oauth' });

  // 判定1: stateの存在 & Cookieとの突合 (ログインCSRF防御)
  if (!queryCode || !queryState || !cookieState || queryState !== cookieState) {
    return c.text('Unauthorized', 400);
  }

  // KVのstate照合 & 即時削除 (リプレイ防御)
  const rawSession = await c.env.OAUTH_KV.get(`state:${queryState}`);
  if (!rawSession) {
    return c.text('Unauthorized', 400);
  }
  await c.env.OAUTH_KV.delete(`state:${queryState}`);
  const { clientRedirectUri, clientState } = JSON.parse(rawSession);

  // 判定2: Googleとのコード交換
  const workerCallback = `https://${new URL(c.req.url).host}/oauth/callback`;
  const tokenRes = await fetch('https://oauth2.googleapis.com/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      code: queryCode,
      client_id: c.env.GOOGLE_CLIENT_ID,
      client_secret: c.env.GOOGLE_CLIENT_SECRET,
      redirect_uri: workerCallback,
      grant_type: 'authorization_code',
    }),
  });

  if (!tokenRes.ok) {
    return c.text('Authentication failed', 400);
  }

  const tokenData: any = await tokenRes.json();

  // 判定3: OIDCエンドポイント (/oauth2/v3/userinfo) から属性取得
  const userRes = await fetch('https://www.googleapis.com/oauth2/v3/userinfo', {
    headers: { Authorization: `Bearer ${tokenData.access_token}` },
  });
  if (!userRes.ok) {
    return c.text('Authentication failed', 400);
  }

  const userData: any = await userRes.json();
  const sub = userData.sub;
  const emailVerified = userData.email_verified;
  const email = userData.email;

  // fail-closed判定 (設定漏れ・未定義時は絶対に403に落とす)
  const isAllowedSubConfigured = typeof c.env.ALLOWED_SUB === 'string' && c.env.ALLOWED_SUB.trim().length > 0;
  const isAllowedEmailConfigured = typeof c.env.ALLOWED_EMAIL === 'string' && c.env.ALLOWED_EMAIL.trim().length > 0;

  if (!isAllowedSubConfigured || !isAllowedEmailConfigured) {
    return c.text('Forbidden: Server configuration error', 403);
  }

  const isSubMatch = sub === c.env.ALLOWED_SUB.trim();
  const isEmailMatch = email?.toLowerCase() === c.env.ALLOWED_EMAIL.trim().toLowerCase();

  if (emailVerified !== true || !isSubMatch || !isEmailMatch) {
    return c.text('Forbidden', 403);
  }

  // 判定4: 認可コードを発行 (subを保存)
  const authCode = crypto.randomUUID();
  await c.env.OAUTH_KV.put(`auth_code:${authCode}`, sub, { expirationTtl: 180 });

  const targetRedirect = new URL(clientRedirectUri);
  targetRedirect.searchParams.set('code', authCode);
  if (clientState) {
    targetRedirect.searchParams.set('state', clientState);
  }

  return c.redirect(targetRedirect.toString());
});

// ----------------------------------------------------
// 3. トークンエンドポイント
// ----------------------------------------------------
app.post('/oauth/token', async (c) => {
  let code = '';
  const contentType = c.req.header('Content-Type') || '';

  if (contentType.includes('application/x-www-form-urlencoded')) {
    const body = await c.req.parseBody();
    code = String(body['code'] || '');
  } else if (contentType.includes('application/json')) {
    const body: any = await c.req.json().catch(() => ({}));
    code = body.code || '';
  }

  if (!code) {
    return c.json({ error: 'invalid_request' }, 400);
  }

  const userSub = await c.env.OAUTH_KV.get(`auth_code:${code}`);
  if (!userSub) {
    return c.json({ error: 'invalid_grant' }, 400);
  }
  await c.env.OAUTH_KV.delete(`auth_code:${code}`);

  // JWT発行 (subクレームにGoogleのsubをセット)
  const secret = new TextEncoder().encode(c.env.JWT_SECRET);
  const jwt = await new SignJWT({ sub: userSub })
    .setProtectedHeader({ alg: 'HS256' })
    .setIssuedAt()
    .setExpirationTime('30d')
    .sign(secret);

  return c.json({
    access_token: jwt,
    token_type: 'Bearer',
    expires_in: 30 * 24 * 60 * 60,
  });
});

// ----------------------------------------------------
// 4. MCP プロキシ (/mcp)
// ----------------------------------------------------
app.all('/mcp', async (c) => {
  const authHeader = c.req.header('Authorization');
  if (!authHeader?.startsWith('Bearer ')) {
    return c.json({ error: 'Unauthorized' }, 401);
  }

  const token = authHeader.slice(7);
  try {
    const secret = new TextEncoder().encode(c.env.JWT_SECRET);
    const { payload } = await jwtVerify(token, secret);

    // subの一致検証 (fail-closed)
    if (!c.env.ALLOWED_SUB || payload.sub !== c.env.ALLOWED_SUB.trim()) {
      return c.json({ error: 'Forbidden' }, 403);
    }
  } catch {
    return c.json({ error: 'Unauthorized' }, 401);
  }

  const targetUrl = new URL('http://127.0.0.1:8765/mcp');
  targetUrl.search = new URL(c.req.url).search;

  const headers = new Headers(c.req.raw.headers);
  headers.set('Host', '127.0.0.1:8765');
  headers.delete('Origin');
  headers.set('Authorization', `Bearer ${c.env.MEMOCAMPUS_UPSTREAM_TOKEN}`);

  try {
    const upstreamResponse = await c.env.MEMOCAMPUS.fetch(targetUrl.toString(), {
      method: c.req.method,
      headers: headers,
      body: c.req.method !== 'GET' && c.req.method !== 'HEAD' ? c.req.raw.body : undefined,
    });

    return new Response(upstreamResponse.body, {
      status: upstreamResponse.status,
      headers: upstreamResponse.headers,
    });
  } catch (err: any) {
    return c.json({ error: 'Bad Gateway' }, 502);
  }
});

app.notFound((c) => c.text('Not Found', 404));

export default app;