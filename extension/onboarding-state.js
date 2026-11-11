// First-run requests. Presets are page-local drafts; every start is followed by an authoritative
// read, including a lost response or a competing page. Nothing resends a start automatically.
import { callHost } from "./search-client.js";
import { readActive } from "./options-state.js";

export async function loadPreset({ send }) {
  const reply = await callHost({ type: "policy_preset", presetId: "default" }, { send, resultType: "policy_preset_result" });
  // Reuse the editor's structural reader, with a draft-only identity, never shown as Active.
  const active = readActive({ ...reply, etag: "draft-only", revision: reply.policy?.revision });
  if (!active || active.schemaVersion !== 2 || reply.presetId !== "default") throw new Error("invalid_response");
  return active;
}

export async function startDraft({ send, policy }) {
  try {
    const reply = await callHost({ type: "policy_start", policy }, { send, resultType: "policy_start_result" });
    const ok = reply.state === "ready" && typeof reply.etag === "string" && reply.etag !== "" && Number.isSafeInteger(reply.revision) && reply.revision > 0;
    return { ok, code: ok ? null : "invalid_response" };
  } catch (error) {
    return { ok: false, code: error.code ?? "host_unavailable" };
  }
}
