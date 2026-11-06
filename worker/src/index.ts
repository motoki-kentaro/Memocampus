import { Hono } from 'hono';
import { SignJWT, jwtVerify } from 'jose';

type Bindings = {
  MEMOCAMPUS: Fetcher;
  OAUTH_KV: KVNamespace;
  GOOGLE_CLIENT_ID: string;
  GOOGLE_CLIENT_SECRET: string;
  ALLOWED_EMAIL: string;
  JWT_SECRET: string;
  MEMOCAMPUS_UPSTREAM_TOKEN: string;
};

const app = new Hono<{ Bindings: Bindings }>();

// ----------------------------------------------------
// 0. OAuth 2.0 メタデータ・ディスカバリ (Gemini 検証用)
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
    return c.text('Missing redirect_uri', 400);
  }

  const sessionState = crypto.randomUUID();
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
  const code = c.req.query('code');
  const state = c.req.query('state');

  if (!code || !state) {
    return c.text('Invalid OAuth callback parameter', 400);
  }

  const rawSession = await c.env.OAUTH_KV.get(`state:${state}`);
  if (!rawSession) {
    return c.text('State expired or invalid. Please retry authentication.', 400);
  }
  await c.env.OAUTH_KV.delete(`state:${state}`);
  const { clientRedirectUri, clientState } = JSON.parse(rawSession);

  const workerCallback = `https://${new URL(c.req.url).host}/oauth/callback`;
  const tokenRes = await fetch('https://oauth2.googleapis.com/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      code,
      client_id: c.env.GOOGLE_CLIENT_ID,
      client_secret: c.env.GOOGLE_CLIENT_SECRET,
      redirect_uri: workerCallback,
      grant_type: 'authorization_code',
    }),
  });

  if (!tokenRes.ok) {
    return c.text('Failed to exchange code with Google', 502);
  }

  const tokenData: any = await tokenRes.json();

  const userRes = await fetch('https://www.googleapis.com/oauth2/v2/userinfo', {
    headers: { Authorization: `Bearer ${tokenData.access_token}` },
  });
  const userData: any = await userRes.json();

  if (userData.email !== c.env.ALLOWED_EMAIL) {
    return c.text(`Forbidden: Account ${userData.email} is not authorized.`, 403);
  }

  const authCode = crypto.randomUUID();
  await c.env.OAUTH_KV.put(`auth_code:${authCode}`, userData.email, { expirationTtl: 180 });

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
    return c.json({ error: 'invalid_request', error_description: 'Missing code' }, 400);
  }

  const userEmail = await c.env.OAUTH_KV.get(`auth_code:${code}`);
  if (!userEmail) {
    return c.json({ error: 'invalid_grant', error_description: 'Code expired or invalid' }, 400);
  }
  await c.env.OAUTH_KV.delete(`auth_code:${code}`);

  const secret = new TextEncoder().encode(c.env.JWT_SECRET);
  const jwt = await new SignJWT({ email: userEmail })
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
// 4. MCP プロキシ
// ----------------------------------------------------
app.all('/mcp', async (c) => {
  const authHeader = c.req.header('Authorization');
  if (!authHeader?.startsWith('Bearer ')) {
    return c.json({ error: 'Unauthorized: Missing or invalid token' }, 401);
  }

  const token = authHeader.slice(7);
  try {
    const secret = new TextEncoder().encode(c.env.JWT_SECRET);
    const { payload } = await jwtVerify(token, secret);

    if (payload.email !== c.env.ALLOWED_EMAIL) {
      return c.json({ error: 'Forbidden' }, 403);
    }
  } catch {
    return c.json({ error: 'Unauthorized: Invalid token signature or expired' }, 401);
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
    return c.json({
      error: 'VPC Upstream Failed',
      message: err.message || String(err),
    }, 502);
  }
});

app.notFound((c) => c.text('Not Found', 404));

export default app;