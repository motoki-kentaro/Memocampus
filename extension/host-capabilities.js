// What the Native Host says it can store (Issue #41, Slice 2). Asked by the queue before it sends a
// visit with a `navigation`: a host from before #41 would store the visit and silently ignore the
// navigation, so only a host that names `navigation_v1` is sent one.
//
// Native Messaging here is one host process per message (sendNativeMessage), so there is no lasting
// connection to attach an answer to. The queue asks once per delivery run (`flush`) and asks again
// after any failure or doubtful acknowledgement; the acknowledgement of each event is checked anyway.

export const CAPABILITY_NAVIGATION = "navigation_v1";
export const CAPABILITY_TIMEOUT_MS = 10_000;

function withTimeout(promise, timeoutMs) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(Object.assign(new Error("timeout"), { code: "timeout" })), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/**
 * `{ ok: true, navigation }` when the host answered (an old host answers `unknown_type`, which means
 * `navigation: false`), or `{ ok: false, code }`: "timeout" (no answer), "host_unavailable" (the host
 * could not be started or stopped), "invalid_response" (an answer of no known form). Never throws.
 */
export async function checkHostCapabilities({ send, createId = () => crypto.randomUUID(), timeoutMs = CAPABILITY_TIMEOUT_MS }) {
  const requestId = createId();
  let response;
  try {
    response = await withTimeout(
      Promise.resolve().then(() => send({ type: "host_capabilities", requestId })),
      timeoutMs,
    );
  } catch (error) {
    return { ok: false, code: error?.code === "timeout" ? "timeout" : "host_unavailable" };
  }
  if (response?.ok === true && response.type === "host_capabilities_result" && response.requestId === requestId && Array.isArray(response.capabilities)) {
    return { ok: true, navigation: response.capabilities.includes(CAPABILITY_NAVIGATION) };
  }
  // A host from before #41 does not know the message, and says so without an id.
  if (response?.ok === false && response.error === "unknown_type" && response.eventId === null) return { ok: true, navigation: false };
  return { ok: false, code: "invalid_response" };
}
