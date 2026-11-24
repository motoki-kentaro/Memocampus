// The confirmation of one policy action on a URL (Issue #43). Deleting has its own dialog (delete-ui.js, Issue #7).
// Nothing is done when an action is chosen: the host previews it first, the dialog shows
// what the host said, and only an explicit confirmation sends the change, bound to what was previewed
// (the policy's ETag, the vault's token). Nothing is shown as done before the host confirmed it.
//
// One dialog serves the URL tab and the Image tab. Every request belongs to one opening of it
// (`session`) and one step of that opening; an answer for an earlier opening or step is dropped, so a
// late answer can never fill the dialog of another item, another vault or another choice.

import { SearchError, VAULT_UNAVAILABLE_CODES } from "./search-client.js";
import { applyPolicyItem, previewPolicyItem } from "./vault-items-client.js";
import { messageTable, t } from "./i18n.js";

// Display text only, looked up by the host's decision or the action's protocol value. The values sent
// (action, scope, URL, ETag) never come from these texts, and no text decides what is allowed.
const DECISION_LABEL_KEYS = Object.freeze({ allow: "policyStateAllow", save_but_no_mcp: "policyStateSaveButNoMcp", never_save: "policyStateNeverSave" });
export const DECISION_LABEL = messageTable(DECISION_LABEL_KEYS);
const DECISION_TEXT_KEYS = Object.freeze({
  allow: "policyStateAllowLong",
  save_but_no_mcp: "policyStateSaveButNoMcpLong",
  never_save: "policyStateNeverSaveLong",
});
const DECISION_TEXT = messageTable(DECISION_TEXT_KEYS);
const ACTION_LABEL_KEYS = Object.freeze({
  never_save: "policyActionNeverSave",
  save_but_no_mcp: "policyActionSaveButNoMcp",
  publish: "policyActionPublish",
});
export const ACTION_LABEL = messageTable(ACTION_LABEL_KEYS);
const CONFIRM_LABEL_KEYS = Object.freeze({
  never_save: "policyConfirmNeverSave",
  save_but_no_mcp: "policyConfirmSaveButNoMcp",
  publish: "policyConfirmPublish",
});
const CONFIRM_LABEL = messageTable(CONFIRM_LABEL_KEYS);
/** How an action is said when it would leave the state as it is (`sameState`). */
const SAME_STATE_KEYS = Object.freeze({ never_save: "policySameNeverSave", save_but_no_mcp: "policySameSaveButNoMcp", publish: "policySamePublish" });
export const SAME_STATE = messageTable(SAME_STATE_KEYS);

/**
 * Whether `action` would leave a URL as the host judges it now (`decision`): a rule that cannot make it
 * stricter, or publishing a URL already public. Only the host's decision is compared; whether the
 * change can be made at all is the preview's answer.
 */
export function sameState(action, decision) {
  if (action === "never_save") return decision === "never_save";
  if (action === "save_but_no_mcp") return decision === "save_but_no_mcp" || decision === "never_save";
  if (action === "publish") return decision === "allow";
  return false;
}

const LAYER_NAME_KEYS = Object.freeze({ neverSave: "policyLayerNeverSave", saveButNoMcp: "policyLayerSaveButNoMcp" });
const LAYER_NAME = messageTable(LAYER_NAME_KEYS);
const KIND_LABEL = { pattern: "Pattern", regex: "Regex", domainWildcard: "Domain Wildcard", exact: "Exact URL" };
const REASON_TEXT_KEYS = Object.freeze({
  publish_blocked: "policyReasonPublishBlocked",
  layer_disabled: "policyReasonLayerDisabled",
  policy_limit: "policyReasonPolicyLimit",
  policy_migration_required: "policyReasonMigrationRequired",
  unsupported_url: "policyReasonUnsupportedUrl",
  invalid_url: "policyReasonInvalidUrl",
});
const reasonText = (reason) => (typeof reason === "string" && Object.hasOwn(REASON_TEXT_KEYS, reason) ? t(REASON_TEXT_KEYS[reason]) : null);
/** Reasons the settings page can resolve. */
const SETTINGS_REASONS = new Set(["publish_blocked", "layer_disabled", "policy_limit", "policy_migration_required"]);

const errorText = (error, fallback) => (error instanceof SearchError ? error.message : fallback);
const isVaultSwitched = (error) => error instanceof SearchError && error.code === "vault_switched";

/**
 * `elements`: the dialog's parts (see sidepanel.html, `#item-dialog`). `openPrivacySettings` opens the
 * settings page; `onVaultUnavailable` points at the vault settings.
 */
export function createItemDialog({ send, openPrivacySettings, onVaultUnavailable = () => {}, document: doc = document }) {
  const $ = (id) => doc.querySelector(`#${id}`);
  const dialog = $("item-dialog");
  const title = $("item-dialog-title");
  const target = $("item-dialog-target");
  const urlChoice = $("item-dialog-urls");
  const urlList = $("item-dialog-url-list");
  const scopeChoice = $("item-dialog-scope");
  const scopeUrl = $("item-scope-url");
  const scopeHost = $("item-scope-host");
  const body = $("item-dialog-body");
  const ack = $("item-dialog-ack");
  const ackBox = $("item-dialog-ack-box");
  const ackText = $("item-dialog-ack-text");
  const errorLine = $("item-dialog-error");
  const confirmButton = $("item-dialog-confirm");
  const repreviewButton = $("item-dialog-repreview");
  const settingsButton = $("item-dialog-settings");
  const cancelButton = $("item-dialog-cancel");

  let session = null;

  function paragraph(text, className = "") {
    const p = doc.createElement("p");
    p.textContent = text;
    if (className) p.className = className;
    return p;
  }

  function list(lines) {
    const ul = doc.createElement("ul");
    ul.className = "item-dialog-list";
    for (const text of lines) {
      const li = doc.createElement("li");
      li.textContent = text;
      ul.append(li);
    }
    return ul;
  }

  /** Enables what the current state allows; nothing while a request runs. */
  function update() {
    const s = session;
    if (!s) return;
    const busy = s.busy;
    scopeUrl.disabled = busy || s.kind !== "policy" || s.action === "publish" || !s.url;
    scopeHost.disabled = scopeUrl.disabled;
    for (const input of s.urlInputs ?? []) input.disabled = busy || input.dataset.unavailable === "true";
    ackBox.disabled = busy;
    cancelButton.disabled = busy;
    repreviewButton.disabled = busy;
    const ready = s.ready && !busy && (!s.needsAck || ackBox.checked);
    confirmButton.disabled = !ready;
  }

  function setError(text) {
    errorLine.textContent = text;
    errorLine.hidden = text === "";
  }

  function open(next) {
    close({ restoreFocus: false, notify: true });
    session = { ...next, step: 0, busy: false, ready: false, needsAck: false };
    title.textContent = next.title;
    target.textContent = next.targetText ?? "";
    target.hidden = !next.targetText;
    urlList.replaceChildren();
    urlChoice.hidden = true;
    scopeChoice.hidden = true;
    scopeUrl.checked = true;
    scopeHost.checked = false;
    ack.hidden = true;
    ackBox.checked = false;
    body.replaceChildren();
    setError("");
    repreviewButton.hidden = true;
    settingsButton.hidden = true;
    confirmButton.textContent = next.confirmLabel;
    confirmButton.className = next.destructive ? "danger" : "";
    dialog.hidden = false;
    update();
    dialog.focus();
    return session;
  }

  /** Closes the dialog; `notify` tells the opener (which resets its drop-down). */
  function close({ restoreFocus = true, notify = true } = {}) {
    const s = session;
    if (!s) return;
    session = null;
    const hadFocus = dialog.contains(doc.activeElement);
    dialog.hidden = true;
    body.replaceChildren();
    urlList.replaceChildren();
    if (notify) s.onClose?.();
    if (restoreFocus && hadFocus) s.opener?.focus?.();
  }

  /** Starts a step of `s`; its answer counts only while it is still the latest step of the open dialog. */
  function step(s) {
    s.step += 1;
    const mine = s.step;
    return () => session === s && s.step === mine;
  }

  // --- Policy changes ---------------------------------------------------------------------------

  /**
   * `targets`: `[{ url, label, decision?, unavailable? }]`. One target is the URL itself; several (an
   * image) must be chosen from, none is chosen for the person. `note`: shown with every preview.
   */
  function openPolicy({ owner, action, targets, targetText, note = "", opener, onApplied, onClose }) {
    const s = open({ kind: "policy", owner, action, title: ACTION_LABEL[action], targetText, confirmLabel: CONFIRM_LABEL[action], opener, onApplied, onClose, note, url: null, scope: "url", preview: null });
    if (targets.length === 1 && !targets[0].unavailable) {
      s.url = targets[0].url;
      if (!targetText) {
        target.textContent = t("policyTargetUrl", s.url);
        target.hidden = false;
      }
      void previewPolicy(s);
    } else {
      renderTargets(s, targets);
      body.replaceChildren(paragraph(t("policyChooseUrl")));
      if (note) body.append(paragraph(note, "item-dialog-note"));
    }
    scopeChoice.hidden = action === "publish";
    update();
  }

  function renderTargets(s, targets) {
    urlChoice.hidden = false;
    const rows = targets.map((choice, index) => {
      const row = doc.createElement("label");
      row.className = "item-url-option";
      const input = doc.createElement("input");
      input.type = "radio";
      input.name = "item-url";
      input.className = "item-url";
      input.value = choice.url;
      input.id = `item-url-${index}`;
      input.dataset.unavailable = String(Boolean(choice.unavailable));
      input.disabled = Boolean(choice.unavailable);
      const text = doc.createElement("span");
      const state = choice.decision ? t("policyCurrentState", DECISION_LABEL[choice.decision]) : t("policyCurrentStateUnknown");
      text.textContent = t("policyUrlChoice", choice.label, state, choice.unavailable ? t("policyUrlUnavailable", choice.unavailable) : "", choice.url);
      row.append(input, text);
      return row;
    });
    urlList.replaceChildren(...rows);
    s.targets = targets;
    s.urlInputs = rows.map((row) => row.children[0]);
  }

  async function previewPolicy(s) {
    const current = step(s);
    s.ready = false;
    s.preview = null;
    s.needsAck = false;
    ack.hidden = true;
    ackBox.checked = false;
    repreviewButton.hidden = true;
    settingsButton.hidden = true;
    setError("");
    body.replaceChildren(paragraph(t("policyChecking")));
    update();
    try {
      const preview = await previewPolicyItem({ url: s.url, action: s.action, scope: s.scope }, { send });
      if (!current()) return;
      s.preview = preview;
      renderPolicyPreview(s, preview);
    } catch (error) {
      if (!current()) return;
      body.replaceChildren();
      setError(errorText(error, t("policyCheckFailed")));
      repreviewButton.hidden = false;
      if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
    }
    update();
  }

  function ruleLine(change) {
    const verb = t(change.op === "add" ? "policyRuleAdd" : "policyRuleRemove");
    return t("policyRuleLine", verb, LAYER_NAME[change.layer], KIND_LABEL[change.rule.kind], change.rule.pattern);
  }

  function blockerLine(blocker) {
    if (blocker.builtin) return t("policyBlockerBuiltin", LAYER_NAME[blocker.layer]);
    return t("policyBlockerRule", LAYER_NAME[blocker.layer], blocker.ruleIndex + 1, KIND_LABEL[blocker.kind]);
  }

  function renderPolicyPreview(s, preview) {
    const parts = [];
    parts.push(paragraph(t("policyTargetUrl", preview.url)));
    if (s.action !== "publish") {
      // The scope shown is the host's preview's, never the radio button's.
      parts.push(paragraph(t(preview.scope === "host" ? "policyScopeHost" : "policyScopeUrl"), preview.scope === "host" ? "item-dialog-warning" : ""));
    }
    parts.push(paragraph(t("policyStateBefore", DECISION_TEXT[preview.current])));
    parts.push(paragraph(t("policyStateAfter", DECISION_TEXT[preview.expected])));
    if (preview.changes.length > 0) {
      parts.push(paragraph(t("policyChangesHeading")));
      parts.push(list(preview.changes.map(ruleLine)));
    }
    if (s.note) parts.push(paragraph(s.note, "item-dialog-note"));
    if (s.action !== "publish") parts.push(paragraph(t("policyRulesKeepData"), "item-dialog-note"));
    let ready = preview.executable;
    if (!preview.executable) {
      parts.push(paragraph(reasonText(preview.reason) ?? errorText(new SearchError(preview.reason), t("policyCannotRun")), "item-dialog-warning"));
      if (preview.blockers.length > 0) {
        parts.push(paragraph(t("policyBlockersHeading")));
        parts.push(list(preview.blockers.map(blockerLine)));
      }
      settingsButton.hidden = !SETTINGS_REASONS.has(preview.reason);
    } else if (preview.changes.length === 0) {
      ready = false;
      parts.push(paragraph(t(s.action === "publish" ? "policyAlreadyPublic" : "policyRuleExists")));
    } else if (preview.expected === preview.current && s.action !== "publish") {
      parts.push(paragraph(t("policyRuleAddedStateSame")));
    }
    if (preview.affectsOtherUrls && ready) {
      s.needsAck = true;
      ack.hidden = false;
      ackText.textContent = t("policyAckHost");
    }
    s.ready = ready;
    body.replaceChildren(...parts);
  }

  async function applyPolicy(s) {
    const preview = s.preview;
    if (!preview || !s.ready || s.busy || (s.needsAck && !ackBox.checked)) return;
    const current = step(s);
    s.busy = true;
    setError("");
    update();
    try {
      const result = await applyPolicyItem({ url: preview.url, action: preview.action, scope: preview.scope, etag: preview.etag }, { send });
      if (!current()) return;
      s.busy = false;
      close({ restoreFocus: true });
      s.onApplied?.({ ...result, url: preview.url, action: preview.action, scope: preview.scope });
    } catch (error) {
      if (!current()) return;
      s.busy = false;
      // What is stored is unknown or unchanged: never shown as done, and never sent again as it was.
      s.ready = false;
      setError(error instanceof SearchError && error.code === "policy_conflict" ? t("policyConflictRetry", error.message, t("policyRecheck")) : errorText(error, t("policyChangeFailed")));
      repreviewButton.hidden = false;
      update();
    }
  }

  function failPreview(s, error, fallback) {
    if (isVaultSwitched(error)) return vaultMoved(s);
    body.replaceChildren();
    setError(errorText(error, fallback));
    repreviewButton.hidden = false;
    if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
  }

  /** The vault is no longer the one the list came from: nothing is deleted, the list starts again. */
  function vaultMoved(s) {
    close({ restoreFocus: false });
    s.onVaultMismatch?.();
  }

  // --- Events -----------------------------------------------------------------------------------

  confirmButton.addEventListener("click", () => {
    const s = session;
    if (!s) return;
    if (s.kind === "policy") void applyPolicy(s);
  });
  repreviewButton.addEventListener("click", () => {
    const s = session;
    if (!s || s.busy) return;
    if (s.kind === "policy") void previewPolicy(s);
  });
  cancelButton.addEventListener("click", () => {
    if (session && !session.busy) close();
  });
  settingsButton.addEventListener("click", () => openPrivacySettings());
  ackBox.addEventListener("change", () => update());
  for (const radio of [scopeUrl, scopeHost]) {
    radio.addEventListener("change", () => {
      const s = session;
      if (!s || s.kind !== "policy" || s.busy || !s.url) return;
      s.scope = scopeHost.checked ? "host" : "url";
      void previewPolicy(s);
    });
  }
  urlList.addEventListener("change", (event) => {
    const input = event.target.closest?.("input.item-url");
    const s = session;
    if (!input || !s || s.kind !== "policy" || s.busy || input.disabled) return;
    s.url = input.value;
    s.scope = "url";
    scopeUrl.checked = true;
    scopeHost.checked = false;
    void previewPolicy(s);
  });
  dialog.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || !session || session.busy) return;
    event.preventDefault();
    close();
  });

  return {
    openPolicy,
    /** Closes the dialog if it belongs to `owner` (a list or a selection that is gone). */
    cancel(owner) {
      if (session && (owner === undefined || session.owner === owner)) close({ restoreFocus: false });
    },
    get owner() {
      return session?.owner ?? null;
    },
    get busy() {
      return Boolean(session?.busy);
    },
  };
}
