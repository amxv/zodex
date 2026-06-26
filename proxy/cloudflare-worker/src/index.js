const HEALTH_PATH = "/health";
const MCP_ROOT_PATH = "/mcp/";
const RETRYABLE_STATUSES = new Set([408, 425, 429, 500, 502, 503, 504]);
const RETRY_DELAYS_MS = [0, 400, 1200, 2500];
const WARMUP_TIMEOUTS_MS = [1500, 3000, 6000];
const UPSTREAM_RESPONSE_TIMEOUT_MS = 20000;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const route = resolveRoute(url.pathname);

    if (!route) {
      return json(
        {
          ok: true,
          component: "zodex-cloudflare-worker",
          routes: ["/health", "/mcp", "/mcp/"],
          spriteOrigin: env.SPRITE_ORIGIN,
        },
        200,
      );
    }

    const preparedRequest = await prepareRequest(request);

    await warmSprite(env);
    return proxyWithRetry(preparedRequest, env, route.upstreamPath);
  },
};

function resolveRoute(pathname) {
  if (pathname === HEALTH_PATH) {
    return { kind: "health", upstreamPath: HEALTH_PATH };
  }

  if (pathname === "/mcp" || pathname === MCP_ROOT_PATH) {
    return { kind: "mcp", upstreamPath: MCP_ROOT_PATH };
  }

  if (pathname.startsWith(MCP_ROOT_PATH)) {
    return { kind: "mcp", upstreamPath: pathname };
  }

  return null;
}

async function prepareRequest(request) {
  const url = new URL(request.url);
  const headers = new Headers(request.headers);
  headers.delete("content-length");
  headers.delete("host");
  headers.set("x-forwarded-host", url.host);
  headers.set("x-forwarded-proto", url.protocol.replace(":", ""));
  headers.set("x-proxy-origin", "cloudflare-worker");

  return {
    method: request.method,
    headers,
    search: url.search,
    bodyBuffer: shouldSendBody(request.method) ? await request.arrayBuffer() : null,
  };
}

async function warmSprite(env) {
  let lastError = null;

  for (const timeoutMs of WARMUP_TIMEOUTS_MS) {
    try {
      const response = await fetchWithTimeout(
        buildUpstreamUrl(env, HEALTH_PATH),
        {
          method: "GET",
          headers: noCacheHeaders(),
        },
        timeoutMs,
      );

      response.body?.cancel();

      if (response.ok) {
        return;
      }
    } catch (error) {
      lastError = error;
    }

    await sleep(250);
  }

  if (lastError) {
    console.warn("sprite warmup did not complete before proxying", formatError(lastError));
  }
}

async function proxyWithRetry(preparedRequest, env, upstreamPath) {
  let lastError = null;
  let lastRetryableResponse = null;

  for (let index = 0; index < RETRY_DELAYS_MS.length; index += 1) {
    if (RETRY_DELAYS_MS[index] > 0) {
      await sleep(RETRY_DELAYS_MS[index]);
    }

    try {
      const response = await proxyRequest(preparedRequest, env, upstreamPath);

      if (!RETRYABLE_STATUSES.has(response.status)) {
        return response;
      }

      lastRetryableResponse = response;
      response.body?.cancel();
    } catch (error) {
      lastError = error;
    }
  }

  if (lastRetryableResponse) {
    return lastRetryableResponse;
  }

  if (lastError) {
    return json(
      {
        error: "upstream_fetch_failed",
        detail: formatError(lastError),
      },
      502,
    );
  }

  return json(
    {
      error: "upstream_unavailable",
      detail: "Sprite did not become ready in time.",
    },
    502,
  );
}

async function proxyRequest(preparedRequest, env, upstreamPath) {
  const upstreamUrl = buildUpstreamUrl(env, upstreamPath, preparedRequest.search);
  const response = await fetchWithTimeout(
    upstreamUrl,
    {
      method: preparedRequest.method,
      headers: preparedRequest.headers,
      body: buildRequestBody(preparedRequest.bodyBuffer),
      redirect: "manual",
    },
    UPSTREAM_RESPONSE_TIMEOUT_MS,
  );

  return relayResponse(response, env);
}

function buildRequestBody(bodyBuffer) {
  if (!bodyBuffer) {
    return undefined;
  }

  return bodyBuffer.slice(0);
}

function relayResponse(response, env) {
  const headers = new Headers(response.headers);
  headers.set("cache-control", "no-store");
  headers.set("x-proxy-upstream", upstreamHost(env));

  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

function buildUpstreamUrl(env, pathname, search = "") {
  const origin = env.SPRITE_ORIGIN;
  if (!origin) {
    throw new Error("SPRITE_ORIGIN is not configured");
  }

  const url = new URL(pathname, ensureTrailingSlash(origin));
  url.search = search;
  return url;
}

function upstreamHost(env) {
  try {
    return new URL(env.SPRITE_ORIGIN).host;
  } catch {
    return "unknown";
  }
}

function ensureTrailingSlash(value) {
  return value.endsWith("/") ? value : `${value}/`;
}

function shouldSendBody(method) {
  return method !== "GET" && method !== "HEAD";
}

function noCacheHeaders() {
  return {
    "cache-control": "no-store",
    pragma: "no-cache",
  };
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function fetchWithTimeout(input, init, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort("timeout"), timeoutMs);

  try {
    return await fetch(input, {
      ...init,
      signal: controller.signal,
      redirect: "manual",
    });
  } finally {
    clearTimeout(timer);
  }
}

function formatError(error) {
  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
}

function json(payload, status) {
  return new Response(JSON.stringify(payload, null, 2), {
    status,
    headers: {
      "cache-control": "no-store",
      "content-type": "application/json; charset=utf-8",
    },
  });
}
