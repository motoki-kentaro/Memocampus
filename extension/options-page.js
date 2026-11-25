// The Privacy settings page (options.html). It reads the active policy from the host, lets the person
// edit a draft in memory, has the host check and test the draft, and applies it only when asked,
// in place of the policy it was made from (its ETag). First use previews a host preset through the
// same editor and starts only on the person's click. Neither start nor apply is retried automatically.
// Rule text and URLs are set as text or input values only and
// never logged.
//
// Individual edits use the URL checker. CSV batches enter only after whole-candidate Host
// validation and identity checks. Rules are never matched by this page or typed into the tables.

import { describeErrorCode, createLatestGate } from "./search-client.js";
import { loadPreset, startDraft } from "./onboarding-state.js";
import { vaultRequest, describeStatus } from "./vault-client.js";
import { messageTable, t } from "./i18n.js";
import { CsvImportError, csvImportCandidate, downloadCsv, draftZoneCsv, readPolicyCsvFile } from "./policy-csv.js";
import {
  CATEGORY_KEYS,
  LAYER_KEYS,
  LAYER_OF_CATEGORY,
  RULE_KINDS,
  applyDraft,
  approveDomain,
  checkDomain,
  compareAfterApply,
  createDraft,
  listApprovals,
  loadActivePolicy,
  matchState,
  revokeDomain,
  sameRule,
  testRule,
  testUrl,
  validateDraft,
} from "./options-state.js";

const LAYER_ID = { neverSave: "never-save", saveButNoMcp: "save-but-no-mcp" };
// Display text comes from the catalogs (Issue #77 Slice 4); the tables are looked up only with their own keys.
const LAYER_NAME_KEYS = Object.freeze({ neverSave: "policyLayerNeverSave", saveButNoMcp: "policyLayerSaveButNoMcp" });
const LAYER_NAME = messageTable(LAYER_NAME_KEYS);
const CATEGORY_NAME_KEYS = Object.freeze({
  authentication: "optionsCategoryAuthentication", credentials: "optionsCategoryCredentials", government: "optionsCategoryGovernment",
  webmail: "optionsCategoryWebmail", aiChats: "optionsCategoryAiChats",
});
const CATEGORY_NAME = messageTable(CATEGORY_NAME_KEYS);
/** The text of `table` for `code`, or undefined unless `code` is one of its own entries. */
const lookup = (table, code) => (typeof code === "string" && Object.hasOwn(table, code) ? table[code] : undefined);
const KIND_LABEL = { pattern: "Pattern", regex: "Regex", domainWildcard: "Domain Wildcard", exact: "Exact URL" };
/** What the rule field shows for each kind. The domain wildcard is the only kind with an example in it. */
const RULE_PLACEHOLDER = { pattern: "", regex: "", domainWildcard: "*.example.com", exact: "" };
/** Why the host restricts a domain wildcard, by its reason; the date is that of the bundled list. */
const PSL_REASON_KEYS = Object.freeze({
  public_suffix: "optionsPslPublicSuffix",
  delegated_boundary: "optionsPslDelegatedBoundary",
  unknown_suffix: "optionsPslUnknownSuffix",
  not_registrable: "optionsPslNotRegistrable",
});

// The label names the state in words, so it never depends on colour alone.
const STATE_LABEL_KEYS = Object.freeze({
  loading: "optionsStateLoading",
  ready: "optionsStateReady",
  uninitialized: "optionsStateUninitialized",
  invalid: "optionsStateInvalid",
  unsupported: "optionsStateUnsupported",
  busy: "optionsStateBusy",
  unavailable: "optionsStateUnavailable",
  host_unavailable: "optionsStateHostUnavailable",
  invalid_response: "optionsStateInvalidResponse",
});
const STATE_LABEL = messageTable(STATE_LABEL_KEYS);
const stateLabel = (state) => lookup(STATE_LABEL, state) ?? STATE_LABEL.unavailable;

const notShown = () => t("optionsNotShown");
/** The explanation of a state without an active policy; all but the first add that nothing is shown. */
const EXPLANATION_KEYS = Object.freeze({
  uninitialized: "optionsExplainUninitialized",
  invalid: "optionsExplainInvalid",
  unsupported: "optionsExplainUnsupported",
  busy: "optionsExplainBusy",
  unavailable: "optionsErrorPolicyUnavailable",
  invalid_response: "optionsErrorInvalidResponse",
});

// What the host says about a rule or a draft. Never the rule's text: the host does not send it back.
const PROBLEM_TEXT_KEYS = Object.freeze({
  invalid_pattern: "optionsProblemInvalidPattern",
  invalid_regex: "optionsProblemInvalidRegex",
  unsupported_regex: "optionsProblemUnsupportedRegex",
  regex_limit: "optionsProblemRegexLimit",
  invalid_domain_wildcard: "optionsProblemInvalidDomainWildcard",
  domain_wildcard_not_approved: "optionsProblemDomainNotApproved",
  psl_unavailable: "optionsProblemPslUnavailable",
  invalid_exact_url: "optionsProblemInvalidExactUrl",
  invalid_rule_form: "optionsProblemInvalidRule",
  invalid_rule: "optionsProblemInvalidRule",
  too_many_rules: "optionsProblemTooManyRules",
  too_many_regex_rules: "optionsProblemTooManyRegexRules",
  policy_too_large: "optionsProblemPolicyTooLarge",
  invalid_version: "optionsProblemInvalidVersion",
  invalid_revision: "optionsProblemInvalidRevision",
  invalid_policy_document: "optionsProblemInvalidDocument",
  invalid_policy: "optionsProblemInvalidPolicy",
});
const PROBLEM_TEXT = messageTable(PROBLEM_TEXT_KEYS);
const ERROR_TEXT_KEYS = Object.freeze({
  policy_invalid: "optionsErrorPolicyInvalid",
  policy_unsupported: "optionsErrorPolicyUnsupported",
  policy_conflict: "optionsErrorPolicyConflict",
  policy_busy: "optionsErrorPolicyBusy",
  policy_unavailable: "optionsErrorPolicyUnavailable",
  policy_not_initialized: "optionsErrorPolicyNotInitialized",
  policy_version_mismatch: "optionsErrorVersionMismatch",
  invalid_etag: "optionsErrorInvalidEtag",
  policy_draft_too_large: "optionsErrorDraftTooLarge",
  invalid_policy_draft: "optionsErrorInvalidDraft",
  invalid_url: "optionsErrorInvalidUrl",
  timeout: "optionsErrorTimeout",
  host_unavailable: "optionsErrorHostUnavailable",
  invalid_response: "optionsErrorInvalidResponse",
  unknown_type: "optionsErrorUnknownType",
  domain_in_use: "optionsErrorDomainInUse",
  approvals_unavailable: "optionsErrorApprovalsUnavailable",
  domain_mismatch: "optionsErrorDomainMismatch",
  domain_not_restricted: "optionsErrorDomainNotRestricted",
  invalid_domain: "optionsErrorInvalidDomain",
});
const ERROR_TEXT = messageTable(ERROR_TEXT_KEYS);
const problemText = (code) => lookup(PROBLEM_TEXT, code) ?? lookup(ERROR_TEXT, code) ?? describeErrorCode(code);
const errorText = (code) => lookup(ERROR_TEXT, code) ?? lookup(PROBLEM_TEXT, code) ?? describeErrorCode(code);
const DRAFT_CODES = new Set(["policy_invalid", "policy_unsupported"]);

/** Why a reading of the policy gave no active policy, in words. */
const loadReason = (loaded) =>
  loaded.code && loaded.state === "host_unavailable" ? t("optionsLoadReasonError", stateLabel(loaded.state), errorText(loaded.code)) : t("optionsLoadReason", stateLabel(loaded.state));

const DECISION_TEXT_KEYS = Object.freeze({ never_save: "optionsDecisionNeverSave", save_but_no_mcp: "optionsDecisionSaveButNoMcp", allow: "optionsDecisionAllow" });
const DECISION_TEXT = messageTable(DECISION_TEXT_KEYS);

function relaxationText(item) {
  if (item.type === "category") return t("optionsRelaxCategory", CATEGORY_NAME[item.key]);
  if (item.type === "layer") return t("optionsRelaxLayer", LAYER_NAME[item.key]);
  return t("optionsRelaxRules", LAYER_NAME[item.key], String(item.count));
}

/**
 * The message `note` gives (a text, or a function returning it), or "" when it cannot be read. A message that
 * cannot be read is reported as an uncaught error (reportError: the page itself logs nothing; the error names
 * only the message key) and is not replaced by another message: the step it ends must still end, and nothing
 * may read as done that is not.
 */
function readMessage(note) {
  try {
    return typeof note === "function" ? note() : note;
  } catch (error) {
    globalThis.reportError?.(error);
    return "";
  }
}

/** The name of the button that makes a draft take effect: "記憶を開始する" on first use, "適用" otherwise. */
const applyButtonName = (onboarding) => t(onboarding ? "optionsStartRecording" : "optionsApply");

export function startOptionsPage({ document, window, send, download = downloadCsv }) {
  const $ = (id) => document.querySelector(`#${id}`);
  const loadGate = createLatestGate();
  const validateGate = createLatestGate();
  const urlGate = createLatestGate();

  let view = { state: "loading" };
  let onboarding = false;
  let startPending = false;
  let vaultReady = false;
  let draft = null;
  let loading = false;
  let notice = "";
  /** null, "validating" (the 検証 button), "applying" or "checking" (reading after an apply). */
  let busy = null;
  /**
   * Why applying waits for the person: "conflict" or "changed" (another policy is stored: reload
   * first), "applied" or "unknown" (the outcome of an apply is not confirmed: check it first).
   */
  let blocked = null;
  /** The last validation, `{ draft, version, ok, code, problems }`; shown only for that very draft. */
  let validation = null;
  let validationNote = "";
  let applyNote = "";
  /** Bumped by every change of the draft or the test URL: an older URL test answer is dropped. */
  let testEpoch = 0;
  /** The draft and change the open privacy confirmation is about. */
  let confirming = null;
  const rows = { neverSave: [], saveButNoMcp: [] };
  const exports = { neverSave: null, saveButNoMcp: null };
  const imports = { neverSave: null, saveButNoMcp: null };
  let importing = null;

  const locked = () => busy === "applying" || busy === "checking" || busy === "importing";
  const canEdit = () => Boolean(draft?.editable) && !locked();
  const canImport = () => view.state === "ready" && !onboarding && Boolean(draft?.editable) &&
    busy === null && blocked === null && !loading && !confirming;
  const canApply = () => !onboarding && Boolean(draft?.editable) && draft.dirty() && busy === null && blocked === null && !loading;
  const currentValidation = () => (validation && validation.draft === draft && validation.version === draft.version ? validation : null);

  function explanation() {
    if (view.state === "ready") return t(view.editable ? "optionsReadyV2" : "optionsReadyV1");
    if (view.state === "loading") return "";
    if (view.state === "host_unavailable") return `${describeErrorCode(view.code)}\n${notShown()}`;
    const key = lookup(EXPLANATION_KEYS, view.state);
    if (key === undefined) return notShown();
    return view.state === "uninitialized" ? t(key) : `${t(key)}\n${notShown()}`;
  }

  // --- Rendering ----------------------------------------------------------------------------------

  function renderStatus() {
    const ready = view.state === "ready";
    const state = $("policy-state");
    state.textContent = stateLabel(view.state);
    state.dataset.state = view.state;
    $("policy-version").textContent = ready ? `Schema ${view.active.schemaVersion} / Engine ${view.active.engineVersion}` : "—";
    $("policy-revision").textContent = ready ? String(view.active.revision) : "—";
    $("policy-explanation").textContent = explanation();
    $("reload").disabled = loading || locked();
    $("options").setAttribute("aria-busy", String(loading || busy !== null));
    $("onboarding").hidden = !onboarding;
    $("start-recording").hidden = !onboarding;
    $("start-recording").disabled = !onboarding || !draft || !vaultReady || busy !== null || startPending || loading;
    $("start-recheck").hidden = !onboarding || !startPending;
    $("start-recheck").disabled = busy !== null;
    $("vault-recheck").disabled = busy !== null;
    $("checker-description").textContent = t("optionsCheckerDescription", applyButtonName(onboarding));
  }

  function renderCategories() {
    $("categories").hidden = !draft;
    const note = $("categories-note");
    note.textContent = !draft && !loading ? notShown() : draft && !draft.editable ? t("optionsV1NotEditable") : "";
    note.hidden = note.textContent === "";
    const weakened = new Set(draft?.weakened() ?? []);
    for (const key of CATEGORY_KEYS) {
      const input = $(`category-${key}`);
      input.checked = draft ? draft.category(key) : false;
      input.disabled = !canEdit();
      const layerNote = $(`category-${key}-note`);
      const layer = LAYER_OF_CATEGORY[key];
      layerNote.textContent = draft && !draft.layerEnabled(layer) ? t("optionsCategoryLayerOff", LAYER_NAME[layer]) : "";
      layerNote.hidden = layerNote.textContent === "";
      // A warning only: the person's choice stands.
      $(`category-${key}-warning`).hidden = !weakened.has(key);
    }
  }

  function cell(...nodes) {
    const td = document.createElement("td");
    td.append(...nodes);
    return td;
  }
  function textCell(text, className = "") {
    const td = document.createElement("td");
    td.className = className;
    td.textContent = text;
    return td;
  }

  function ruleCode(rule) {
    const code = document.createElement("code");
    code.className = "rule-text";
    code.textContent = typeof rule === "string" ? rule : rule.pattern;
    return code;
  }

  /** A rule of a draft that cannot be edited (version 1). */
  function readOnlyRow(rule) {
    const row = document.createElement("tr");
    const kind = typeof rule === "string" ? t("optionsKindPatternV1") : KIND_LABEL[rule.kind];
    row.append(textCell(kind), cell(ruleCode(rule)), textCell(t("optionsNotEditable"), "muted"));
    return { row };
  }

  function button(text, className, label) {
    const element = document.createElement("button");
    element.type = "button";
    element.className = className;
    element.textContent = text;
    element.setAttribute("aria-label", label);
    return element;
  }

  /**
   * A rule of an editable draft. Its kind and text are shown, not typed in: a change goes through
   * the URL checker ("チェッカーで編集"). Deleting needs no match.
   */
  function editableRow(layerKey, rule, index) {
    const id = `${LAYER_ID[layerKey]}-rule-${index}`;
    const place = t("optionsRowPlace", LAYER_NAME[layerKey], index + 1);
    const row = document.createElement("tr");
    const code = ruleCode(rule);
    code.id = `${id}-text`;
    const error = document.createElement("p");
    error.id = `${id}-error`;
    error.className = "rule-error";
    error.hidden = true;
    const edit = button(t("optionsEditInChecker"), "rule-edit", t("optionsEditRuleLabel", place));
    edit.setAttribute("aria-describedby", `${id}-error`);
    edit.addEventListener("click", () => startEdit(layerKey, index));
    const remove = button(t("optionsDelete"), "rule-delete", t("optionsDeleteRuleLabel", place));
    remove.addEventListener("click", () => deleteRule(layerKey, index));
    const actions = document.createElement("div");
    actions.className = "row-actions";
    actions.append(edit, remove);
    row.append(textCell(KIND_LABEL[rule.kind]), cell(code, error), cell(actions));
    return { row, code, error, edit, remove };
  }

  /** Builds the rows of a layer again; only when rules are added, removed or replaced. */
  function buildRules(key) {
    const layer = draft?.layer(key);
    const fragment = document.createDocumentFragment();
    rows[key] = (layer?.rules ?? []).map((rule, index) => (draft.editable ? editableRow(key, rule, index) : readOnlyRow(rule)));
    for (const { row } of rows[key]) fragment.append(row);
    $(`${LAYER_ID[key]}-rules`).replaceChildren(fragment);
  }

  function renderLayer(key) {
    const id = LAYER_ID[key];
    const layer = draft?.layer(key);
    const count = layer?.rules.length ?? 0;
    const toggle = $(`${id}-enabled`);
    toggle.checked = layer?.enabled ?? false;
    toggle.disabled = !canEdit();
    $(`${id}-warning`).hidden = !(layer && view.active.policy.layers[key].enabled && !layer.enabled);
    $(`${id}-state`).textContent = !layer ? "" : t(layer.enabled ? "optionsLayerOn" : "optionsLayerOff", String(count));
    $(`${id}-state`).hidden = !layer;
    $(`${id}-table`).hidden = count === 0;
    const empty = $(`${id}-empty`);
    empty.textContent = layer ? t("optionsNoRules") : loading ? "" : notShown();
    empty.hidden = count > 0 || empty.textContent === "";
    $(`${id}-hint`).hidden = !draft?.editable;
    $(`${id}-export`).disabled = !draft || loading;
    const exported = $(`${id}-export-status`);
    exported.textContent = exports[key]?.text ?? "";
    exported.className = exports[key]?.failed ? "error-text" : "status";
    exported.hidden = !exports[key];
    $(`${id}-import`).disabled = !canImport();
    const imported = $(`${id}-import-status`);
    imported.textContent = imports[key]?.text ?? "";
    imported.className = imports[key]?.failed ? "error-text" : "status";
    imported.hidden = !imports[key];
    rows[key].forEach((controls, index) => {
      if (!controls.edit) return;
      controls.edit.disabled = !canEdit();
      controls.remove.disabled = !canEdit();
      const target = editing !== null && editing.layer === key && editing.index === index && editTargetHolds();
      controls.row.dataset.editing = target ? "true" : "";
    });
  }

  /** Problems of the current validation on their rows and layers; the rest goes to the summary. */
  function renderValidation() {
    const current = draft ? currentValidation() : null;
    const problems = current?.problems ?? [];
    const general = [];
    for (const key of LAYER_KEYS) {
      const layerMessages = [];
      rows[key].forEach((controls, index) => {
        if (!controls.error) return;
        const own = problems.filter((problem) => problem.layer === key && problem.ruleIndex === index);
        controls.error.textContent = own.length ? t("optionsErrorPrefix", own.map((problem) => problemText(problem.code)).join(" ")) : "";
        controls.error.hidden = own.length === 0;
        controls.code.dataset.invalid = own.length ? "true" : "";
      });
      for (const problem of problems.filter((problem) => problem.layer === key)) {
        if (problem.ruleIndex === null) layerMessages.push(problemText(problem.code));
        // A place this page does not have is not put on another row.
        else if (!rows[key][problem.ruleIndex]?.error) general.push(problemText(problem.code));
      }
      const layerError = $(`${LAYER_ID[key]}-error`);
      layerError.textContent = layerMessages.length ? t("optionsErrorPrefix", layerMessages.join(" ")) : "";
      layerError.hidden = layerMessages.length === 0;
    }
    for (const problem of problems) if (problem.layer === null) general.push(problemText(problem.code));

    const status = $("validation-status");
    let text = validationNote;
    if (busy === "validating") text = t("optionsValidating");
    else if (current?.ok) text = t("optionsValidated");
    else if (current && DRAFT_CODES.has(current.code)) {
      const placed = problems.length - general.length;
      text = t(placed > 0 ? "optionsValidationProblemsPlaced" : "optionsValidationProblems", String(problems.length)) + (general.length ? ` ${general.join(" ")}` : "");
    } else if (current) text = t("optionsValidationFailed", errorText(current.code));
    status.textContent = text;
    status.hidden = text === "";
    status.dataset.result = current ? (current.ok ? "ok" : "error") : "";
  }

  function applyReason() {
    if (onboarding) return t("optionsReasonOnboarding");
    if (!draft) return t("optionsReasonNoDraft");
    if (!draft.editable) return t("optionsReasonV1");
    if (busy === "applying" || busy === "checking") return t("optionsReasonApplying");
    if (busy === "validating") return t("optionsReasonValidating");
    if (busy === "importing") return t("optionsReasonImporting");
    if (blocked === "conflict" || blocked === "changed") return t("optionsReasonReload");
    if (blocked) return t("optionsReasonCheckResult");
    if (!draft.dirty()) return t("optionsReasonClean");
    return t("optionsReasonReady");
  }

  function renderFooter() {
    const dirty = draft?.dirty() ?? false;
    $("dirty-status").textContent = dirty ? t("optionsDirty") : notice;
    $("discard").disabled = !dirty || locked();
    $("validate").disabled = !draft?.editable || busy !== null;
    $("apply").disabled = !canApply();
    $("apply").hidden = onboarding;
    $("apply-reason").textContent = applyReason();
    const status = $("apply-status");
    status.textContent = applyNote;
    status.hidden = applyNote === "";
    const recheck = $("apply-recheck");
    recheck.hidden = blocked !== "applied" && blocked !== "unknown";
    recheck.disabled = busy !== null;
  }

  function renderTestControls() {
    $("test-url").disabled = !draft || locked();
    $("test-url-run").disabled = !draft || locked();
  }

  function render() {
    renderStatus();
    renderCategories();
    for (const key of LAYER_KEYS) renderLayer(key);
    renderValidation();
    renderTestControls();
    renderChecker();
    renderFooter();
  }

  function exportLayer(key) {
    if (!draft || loading) return;
    try {
      const snapshot = draftZoneCsv(draft, key, { schemaVersion: view.active.schemaVersion });
      download({ document, window, ...snapshot });
      exports[key] = { text: t("optionsExportRequested") };
    } catch (error) {
      // Never show exception messages: they could contain rule text or browser/private details.
      exports[key] = { failed: true, text: t(error?.code === "csv_unsupported_kind" ? "optionsExportUnknownKind" : error?.code === "csv_invalid_data" ? "optionsExportInvalidData" : "optionsExportFailed") };
    }
    renderLayer(key);
  }

  const IMPORT_ERROR_KEYS = Object.freeze({
    csv_read: "optionsCsvRead", csv_size: "optionsCsvSize",
    csv_utf8: "optionsCsvUtf8", csv_empty: "optionsCsvEmpty",
    csv_header: "optionsCsvHeader", csv_columns: "optionsCsvColumns",
    csv_quote: "optionsCsvQuote", csv_newline: "optionsCsvNewline",
    csv_control: "optionsCsvControl", csv_kind: "optionsCsvKind",
    csv_field: "optionsCsvField",
    csv_records: "optionsCsvRecords",
    csv_regex_count: "optionsCsvRegexCount",
    csv_draft_size: "optionsCsvDraftSize",
    csv_readonly: "optionsCsvReadonly",
  });
  const IMPORT_ERRORS = messageTable(IMPORT_ERROR_KEYS);
  /** An import that added nothing: [line], a reason and an optional host problem (the English reads with spaces). */
  const importFailed = (reason, { line = null, detail = "" } = {}) =>
    (line === null ? t("optionsImportFailed", reason, detail) : t("optionsImportFailedAt", String(line), reason, detail)).trimEnd();

  /** Ends the import (busy, the file input) and draws the page; `text` is the message or a function giving it. */
  function finishImport(operation, text = "", failed = false) {
    if (importing !== operation) return;
    importing = null;
    busy = null;
    $(`${LAYER_ID[operation.key]}-import-file`).value = "";
    const shown = readMessage(text);
    imports[operation.key] = shown ? { text: shown, failed } : null;
    render();
  }

  function importStillCurrent(operation) {
    return importing === operation && busy === "importing" && draft === operation.draft &&
      draft.version === operation.version && view === operation.view &&
      view.active.etag === operation.etag && !loading && blocked === null;
  }

  function chooseImport(key) {
    if (!canImport()) return;
    const operation = { key, draft, version: draft.version, view, etag: view.active.etag };
    importing = operation;
    busy = "importing";
    imports[key] = null;
    render();
    const input = $(`${LAYER_ID[key]}-import-file`);
    input.value = "";
    try { input.click(); }
    catch { finishImport(operation, () => t("optionsImportPickerFailed"), true); }
  }

  async function importSelected(operation, file) {
    if (!file) return finishImport(operation); // Cancel: no error, no Draft change, no Host call.
    const conflict = () => finishImport(operation, () => t("optionsImportConflict"), true);
    try {
      const parsed = await readPolicyCsvFile(file);
      if (!importStillCurrent(operation)) return conflict();
      if (parsed.rules.length === 0) return finishImport(operation, () => t("optionsImportNothing"));
      const snapshot = operation.draft.policy;
      const count = snapshot.layers[operation.key].rules.length;
      const candidate = csvImportCandidate(snapshot, operation.key, parsed);
      const checked = await validateDraft({ send, policy: candidate, requireMatchingVersion: true });
      if (!importStillCurrent(operation)) return conflict();
      if (!checked.ok) {
        const row = checked.problems.find((p) => p.layer === operation.key && Number.isSafeInteger(p.ruleIndex) && p.ruleIndex >= count && p.ruleIndex < count + parsed.rules.length);
        const policyError = DRAFT_CODES.has(checked.code) || checked.problems.length > 0;
        // Only static codes/messages; never echo Host exceptions or rule text.
        return finishImport(operation, () => importFailed(t(policyError ? "optionsImportPolicyFailed" : "optionsImportHostFailed"), {
          line: row ? parsed.lines[row.ruleIndex - count] : null,
          detail: (row && lookup(PROBLEM_TEXT, row.code)) ?? "",
        }), true);
      }
      // A second screen/process may have changed Active during file IO/validation. Read, don't adopt.
      const current = await loadActivePolicy({ send });
      if (!importStillCurrent(operation)) return conflict();
      if (current.state !== "ready") return finishImport(operation, () => t("optionsImportNoActive"), true);
      if (current.active.etag !== operation.etag) {
        // Blocked first: the page must not offer applying over the other policy even if the message cannot be read.
        blocked = "conflict";
        finishImport(operation, () => t("optionsImportEtagConflict"), true);
        return;
      }
      const appended = candidate.layers[operation.key].rules.slice(count);
      if (!operation.draft.appendRulesAtomically(operation.key, appended, { expectedVersion: operation.version })) return conflict();
      resetChecker();
      finishImport(operation, () => t("optionsImportDone", String(parsed.rules.length)));
      edited(true, [operation.key]);
    } catch (error) {
      finishImport(operation, () => (error instanceof CsvImportError
        ? importFailed(lookup(IMPORT_ERRORS, error.code) ?? t("optionsCsvReadFailed"), { line: error.line })
        : importFailed(t("optionsCsvImportIncomplete"))), true);
    }
  }

  // --- Tests --------------------------------------------------------------------------------------

  /** Drops every test answer shown or still to come: they were about another draft or URL. */
  function invalidateTests() {
    testEpoch += 1;
    showUrlResult("", null);
  }

  function showUrlResult(text, decision) {
    $("test-url-result").hidden = text === "";
    const line = $("test-url-decision");
    line.textContent = text;
    line.dataset.decision = decision ?? "";
    $("test-url-caveat").hidden = !decision;
  }

  const testUrlValue = () => $("test-url").value.trim();

  async function runUrlTest() {
    if (!draft || locked()) return;
    const url = testUrlValue();
    if (url === "") {
      showUrlResult(t("optionsEnterUrl"), null);
      $("test-url").focus();
      return;
    }
    const epoch = testEpoch;
    const token = urlGate.next();
    showUrlResult(t("optionsJudging"), null);
    const answer = await testUrl({ send, policy: draft.policy, url });
    if (epoch !== testEpoch || !urlGate.isCurrent(token)) return;
    if (answer.ok) showUrlResult(t("optionsDecision", lookup(DECISION_TEXT, answer.decision) ?? String(answer.decision)), answer.decision);
    else if (DRAFT_CODES.has(answer.code)) showUrlResult(t("optionsJudgeDraftProblem"), null);
    else showUrlResult(t("optionsJudgeFailed", errorText(answer.code)), null);
  }

  // --- Editing ------------------------------------------------------------------------------------

  /** After any change of the draft: earlier answers and the open confirmation no longer apply. */
  function edited(changed = true, rebuild = []) {
    if (!changed) return;
    notice = "";
    validationNote = "";
    checkerNote = "";
    if (blocked === null) applyNote = "";
    hideRelaxConfirm(false);
    for (const key of rebuild) buildRules(key);
    invalidateTests();
    render();
  }

  function deleteRule(layerKey, index) {
    if (!canEdit()) return;
    edited(draft.deleteRule(layerKey, index), [layerKey]);
    const next = rows[layerKey][index] ?? rows[layerKey][index - 1];
    (next?.remove ?? $(`${LAYER_ID[layerKey]}-heading`)).focus();
  }

  // --- URL checker --------------------------------------------------------------------------------
  //
  // The person writes a rule and a URL; the host alone says whether the rule matches. A rule can be
  // added to a group, or replace the rule being edited, only while the latest answer is a match about
  // exactly the kind, text and URL the checker holds now. Every add and every change is checked again
  // when it is made, not only when its button is drawn.

  /** The rule being edited: `{ draft, layer, index, rule, layout }`, or null for a new rule. */
  let editing = null;
  /** The latest check, `{ token, kind, pattern, url, state, code }`; "checking" until answered. */
  let check = null;
  let checkerNote = "";
  let helpPinned = false;
  let highlighted = null;
  const checkGate = createLatestGate();
  /** A request to approve the domain of the current check is on its way. */
  let approving = false;
  /** Approved domains as the host listed them: `{ state: "loading" | "ready" | "failed", items, psl, code }`. */
  let approvals = { state: "loading", items: [], psl: null, code: null };
  let approvalsNote = "";
  /** The domain whose revocation is on its way, or null. */
  let revoking = null;
  const approvalsGate = createLatestGate();

  const checkerKind = () => RULE_KINDS.find((kind) => $(`checker-kind-${kind}`).checked) ?? null;
  /** What the checker holds, as typed: nothing is trimmed, converted or completed. */
  const checkerInput = () => ({ kind: checkerKind(), pattern: $("checker-rule").value, url: $("checker-url").value });
  const sameInput = (a, b) => a.kind === b.kind && a.pattern === b.pattern && a.url === b.url;
  const complete = (input) => RULE_KINDS.includes(input.kind) && input.pattern !== "" && input.url !== "";
  const canCheck = () => Boolean(draft?.editable) && !locked();
  /** The latest check if it is about what the checker holds now. */
  const currentCheck = () => (check && sameInput(check, checkerInput()) ? check : null);

  /** The rule the host has just matched, exactly as the checker holds it; null otherwise. */
  function currentMatch() {
    const current = currentCheck();
    if (!current || current.state !== "match" || !complete(current)) return null;
    return { kind: current.kind, pattern: current.pattern };
  }

  function editTargetHolds() {
    if (!editing || editing.draft !== draft || draft.layout(editing.layer) !== editing.layout) return false;
    return sameRule(draft.rule(editing.layer, editing.index), editing.rule);
  }

  /** Why no rule can be added or changed now, or "" if the match allows it. */
  function checkerReason() {
    if (!draft) return t("optionsCheckerNoDraft");
    if (!draft.editable) return t("optionsCheckerV1");
    if (locked()) return t("optionsCheckerLocked");
    if (editing && !editTargetHolds()) return t("optionsCheckerEditGone");
    const current = currentCheck();
    if (!current) return t(complete(checkerInput()) ? "optionsCheckerCheckToAdd" : "optionsCheckerEnter");
    if (current.state === "checking") return t("optionsCheckerChecking");
    if (current.state === "no_match") return t("optionsCheckerNoMatch");
    if (current.state !== "match") return t("optionsCheckerNotJudged");
    if (!editing) return "";
    if (sameRule(current, editing.rule)) return t("optionsCheckerUnchanged");
    const others = draft.layer(editing.layer).rules.filter((_, index) => index !== editing.index);
    if (others.some((other) => sameRule(other, current))) return t("optionsCheckerDuplicate");
    return "";
  }

  const canAddTo = (layerKey) => !editing && checkerReason() === "" && !draft.hasRule(layerKey, currentMatch());
  const canReflect = () => editing !== null && checkerReason() === "";

  const RESULT_TEXT = {
    checking: () => t("optionsResultChecking"),
    match: () => t("optionsResultMatch"),
    no_match: () => t("optionsResultNoMatch"),
    rule_error: (code) => t("optionsResultRuleError", problemText(code)),
    url_error: (code) => t("optionsResultUrlError", errorText(code)),
    approval_needed: () => t("optionsResultApprovalNeeded"),
    failed: (code) => t("optionsJudgeFailed", errorText(code ?? "invalid_response")),
  };

  function renderChecker() {
    const enabled = canCheck();
    for (const kind of RULE_KINDS) $(`checker-kind-${kind}`).disabled = !enabled;
    for (const id of ["checker-rule", "checker-url", "checker-run"]) $(id).disabled = !enabled;
    const kind = checkerKind();
    $("checker-rule").placeholder = RULE_PLACEHOLDER[kind] ?? "";
    // A screen reader reads a description even when it is hidden, so the note is named only while shown.
    $("checker-kind-note").hidden = kind !== "domainWildcard";
    $("checker-rule").setAttribute("aria-describedby", kind === "domainWildcard" ? "checker-kind-note checker-gate" : "checker-gate");

    const current = currentCheck();
    const result = $("checker-result");
    result.textContent = current ? RESULT_TEXT[current.state](current.code) : t("optionsCheckerNotChecked");
    result.dataset.result = current ? current.state : "none";

    const banner = $("checker-edit");
    banner.hidden = editing === null;
    banner.textContent = editing ? t("optionsEditingBanner", LAYER_NAME[editing.layer], editing.index + 1) : "";
    $("checker-add").hidden = editing !== null;
    $("checker-edit-actions").hidden = editing === null;
    $("checker-apply-edit").disabled = !canReflect();
    $("checker-cancel-edit").disabled = locked();

    const reason = checkerReason();
    const match = currentMatch();
    for (const key of LAYER_KEYS) {
      $(`checker-add-${LAYER_ID[key]}`).disabled = !draft || !canAddTo(key);
      const notes = [];
      if (draft && match && draft.hasRule(key, match)) notes.push(t("optionsCheckerAlreadyAdded"));
      if (draft?.editable && !draft.layerEnabled(key)) notes.push(t("optionsCheckerGroupOff"));
      if (key === "saveButNoMcp" && draft && match && draft.hasRule("neverSave", match)) notes.push(t("optionsCheckerAlsoNeverSave"));
      const note = $(`checker-${LAYER_ID[key]}-note`);
      note.textContent = notes.join(" ");
      note.hidden = notes.length === 0;
    }
    $("checker-gate").textContent = reason || t(editing ? "optionsCheckerMatchReplace" : "optionsCheckerMatchAdd");
    const status = $("checker-status");
    status.textContent = checkerNote;
    status.hidden = checkerNote === "";
    renderApproval(current);
  }

  /** The approval offered for the current check, when the host restricts its domain. */
  function renderApproval(current) {
    const offer = current?.state === "approval_needed" ? current.approval : null;
    $("checker-approval").hidden = offer === null;
    $("checker-approve").disabled = offer === null || approving || !canCheck();
    if (!offer) return;
    const reasonKey = lookup(PSL_REASON_KEYS, offer.reason);
    $("checker-approval-reason").textContent = reasonKey ? t(reasonKey, String(offer.psl.snapshotDate), String(offer.domain)) : errorText("invalid_response");
    $("checker-approval-domain").textContent = t("optionsApprovalDomain", String(offer.domain));
  }

  /** A change of the kind, rule or URL: the answer about what was there before no longer counts. */
  function checkerChanged() {
    if (check && !sameInput(check, checkerInput())) {
      check = null;
      checkGate.next();
    }
    checkerNote = "";
    renderChecker();
  }

  /** Forgets every check and edit: after a reload, a discard or an apply, a match is found again. */
  function resetChecker() {
    check = null;
    editing = null;
    checkGate.next();
  }

  async function runCheck() {
    if (!canCheck()) return;
    const input = checkerInput();
    if (!complete(input)) {
      check = null;
      checkGate.next();
      checkerNote = "";
      renderChecker();
      $(input.pattern === "" ? "checker-rule" : "checker-url").focus();
      return;
    }
    // The question already on its way is not sent again.
    if (currentCheck()?.state === "checking") return;
    const token = checkGate.next();
    check = { token, ...input, state: "checking", code: null };
    checkerNote = "";
    renderChecker();
    const rule = { kind: input.kind, pattern: input.pattern };
    const answer = await testRule({ send, rule, url: input.url });
    if (!checkGate.isCurrent(token) || check?.token !== token) return;
    let state = matchState(answer);
    let code = answer.ok ? null : answer.code;
    let approval = null;
    if (state === "approval_needed") {
      // Why the host restricts the domain, and which domain an approval would be for: from the host.
      const diagnosis = await checkDomain({ send, rule });
      if (!checkGate.isCurrent(token) || check?.token !== token) return;
      if (diagnosis.ok && diagnosis.status === "restricted" && !diagnosis.approved) approval = diagnosis;
      else [state, code] = ["failed", diagnosis.ok ? "invalid_response" : diagnosis.code];
    }
    check = { ...check, state, code, approval };
    renderChecker();
  }

  /** Approves the domain the host restricts in the current check. It adds no rule: a new check follows. */
  async function approve() {
    const current = currentCheck();
    if (approving || !canCheck() || current?.state !== "approval_needed" || !current.approval) return renderChecker();
    const { domain } = current.approval;
    approving = true;
    checkerNote = "";
    renderChecker();
    const result = await approveDomain({ send, rule: { kind: current.kind, pattern: current.pattern }, domain });
    approving = false;
    if (result.ok) {
      if (check?.token === current.token) {
        check = null;
        checkGate.next();
      }
      checkerNote = t("optionsApproved", String(domain));
      void refreshApprovals();
    } else {
      checkerNote = t("optionsNotApproved", errorText(result.code));
    }
    renderChecker();
  }

  // --- Approved domains ---------------------------------------------------------------------------

  async function refreshApprovals() {
    const token = approvalsGate.next();
    approvals = { ...approvals, state: "loading" };
    renderApprovals();
    const result = await listApprovals({ send });
    if (!approvalsGate.isCurrent(token)) return;
    approvals = result.ok
      ? { state: "ready", items: result.approvals, psl: result.psl, code: null }
      : { state: "failed", items: [], psl: null, code: result.code };
    renderApprovals();
  }

  async function revoke(domain) {
    if (revoking !== null || locked()) return;
    revoking = domain;
    approvalsNote = "";
    renderApprovals();
    const result = await revokeDomain({ send, domain });
    revoking = null;
    approvalsNote = result.ok ? t("optionsRevoked", String(domain)) : t("optionsNotRevoked", errorText(result.code));
    await refreshApprovals();
  }

  function renderApprovals() {
    const stateText =
      approvals.state === "loading" ? t("optionsApprovalsLoading")
        : approvals.state === "failed" ? t("optionsApprovalsFailed", errorText(approvals.code ?? "invalid_response"))
          : approvals.items.length ? "" : t("optionsApprovalsEmpty");
    $("approvals-state").textContent = stateText;
    $("approvals-psl").textContent = approvals.psl ? t("optionsApprovalsPsl", String(approvals.psl.snapshotDate)) : "";
    const list = $("approvals-list");
    list.replaceChildren(
      ...approvals.items.map((item, index) => {
        const li = document.createElement("li");
        const name = document.createElement("code");
        name.textContent = item.domain;
        const state = document.createElement("span");
        state.className = "approval-state";
        state.textContent = t("optionsApprovedAt", String(item.approvedAt));
        const note = document.createElement("p");
        note.id = `approval-${index}-note`;
        note.className = "note";
        note.textContent =
          item.inUse === true ? t("optionsApprovalInUse") : item.inUse === null ? t("optionsApprovalInUseUnknown") : "";
        note.hidden = note.textContent === "";
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = t("optionsRevoke");
        button.setAttribute("aria-label", t("optionsRevokeLabel", String(item.domain)));
        button.setAttribute("aria-describedby", note.id);
        button.disabled = item.inUse === true || revoking !== null || locked();
        button.addEventListener("click", () => void revoke(item.domain));
        li.append(name, state, button, note);
        return li;
      }),
    );
    const status = $("approvals-status");
    status.textContent = approvalsNote;
    status.hidden = approvalsNote === "";
  }

  /** Scrolls to a rule just added or changed, marks it for a moment and moves focus to it. */
  function showRow(layerKey, index) {
    const controls = rows[layerKey][index];
    if (!controls) return;
    if (highlighted) highlighted.dataset.added = "";
    highlighted = controls.row;
    const row = controls.row;
    row.dataset.added = "true";
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches === true;
    row.scrollIntoView({ block: "center", behavior: reduce ? "auto" : "smooth" });
    controls.edit.focus({ preventScroll: true });
    const timer = setTimeout(() => {
      if (row.dataset.added === "true") row.dataset.added = "";
    }, 2000);
    timer?.unref?.();
  }

  function addTo(layerKey) {
    // Checked again here: a button drawn enabled earlier, or a click from a script, is not enough.
    const match = currentMatch();
    if (!match || !canAddTo(layerKey)) return renderChecker();
    const index = draft.appendRule(layerKey, match);
    if (index < 0) return renderChecker();
    edited(true, [layerKey]);
    checkerNote = t("optionsAdded", LAYER_NAME[layerKey], index + 1, applyButtonName(onboarding));
    renderChecker();
    showRow(layerKey, index);
  }

  function startEdit(layerKey, index) {
    if (!canEdit()) return;
    const rule = draft.rule(layerKey, index);
    if (!rule) return;
    // Nothing in the draft changes until a match is reflected.
    editing = { draft, layer: layerKey, index, rule, layout: draft.layout(layerKey) };
    for (const kind of RULE_KINDS) $(`checker-kind-${kind}`).checked = kind === rule.kind;
    $("checker-rule").value = rule.pattern;
    check = null;
    checkGate.next();
    checkerNote = "";
    render();
    $("checker").scrollIntoView({ block: "start" });
    $("checker-rule").focus({ preventScroll: true });
  }

  function reflectEdit() {
    const match = currentMatch();
    if (!match || !canReflect()) return renderChecker();
    const { layer, index, rule, layout } = editing;
    const outcome = draft.replaceRule(layer, index, { expected: rule, next: match, layoutSeen: layout });
    if (outcome !== "replaced") return renderChecker();
    editing = null;
    edited(true, [layer]);
    checkerNote = t("optionsReplaced", LAYER_NAME[layer], index + 1, applyButtonName(onboarding));
    renderChecker();
    showRow(layer, index);
  }

  function cancelEdit() {
    if (!editing || locked()) return;
    editing = null;
    checkerNote = t("optionsEditStopped");
    render();
    $("checker-rule").focus();
  }

  function showHelp(shown) {
    $("checker-help").hidden = !shown;
    $("checker-help-toggle").setAttribute("aria-expanded", String(shown));
  }

  // --- Validating and applying --------------------------------------------------------------------

  async function runValidation() {
    if (!draft?.editable || busy !== null) return;
    const target = draft;
    const version = draft.version;
    const token = validateGate.next();
    busy = "validating";
    validationNote = "";
    render();
    const result = await validateDraft({ send, policy: target.policy });
    if (!validateGate.isCurrent(token)) return;
    busy = null;
    if (draft !== target || draft.version !== version) {
      validationNote = t("optionsValidationStale");
    } else {
      validation = { draft: target, version, ...result };
    }
    render();
  }

  /** Ends a start or an apply step (busy) and draws the page; `note` is the message or a function giving it. */
  function finish(note) {
    busy = null;
    applyNote = readMessage(note);
    render();
    $("apply-status").focus();
  }

  /** Takes the policy just read as the active one, with a new draft of it. */
  function adopt(loaded, note) {
    onboarding = false;
    startPending = false;
    view = loaded;
    draft = createDraft(loaded.active, { editable: loaded.editable });
    blocked = null;
    validation = null;
    validationNote = "";
    resetChecker();
    for (const key of LAYER_KEYS) buildRules(key);
    invalidateTests();
    finish(note);
    // What the active policy uses has changed.
    void refreshApprovals();
  }

  async function refreshVault() {
    vaultReady = false;
    try {
      const reply = await vaultRequest("vault_status", {}, { send });
      vaultReady = reply.status?.state === "ready";
      $("onboarding-vault").textContent = vaultReady ? t("optionsVaultReady") : t("optionsVaultNotReady", describeStatus(reply.status));
    } catch {
      $("onboarding-vault").textContent = t("optionsVaultUnknown");
    }
    render();
  }

  async function checkStart(result = null) {
    busy = "checking";
    render();
    const loaded = await loadActivePolicy({ send });
    if (loaded.state === "ready") {
      adopt(loaded, () => t("optionsStartConfirmed"));
      return;
    }
    if (loaded.state === "uninitialized") {
      startPending = false;
      finish(() => t("optionsStartNotStarted", result && !result.ok ? errorText(result.code) : ""));
      await refreshVault();
      return;
    }
    // A damaged/lost policy must no longer show the first-use explanation or a preset.
    if (["invalid", "unsupported"].includes(loaded.state)) {
      onboarding = false;
      view = loaded;
      draft = null;
      for (const key of LAYER_KEYS) buildRules(key);
    }
    startPending = true;
    finish(() => t("optionsStartUnknown", loadReason(loaded)));
  }

  async function startRecording() {
    if (!onboarding || !draft || !vaultReady || busy !== null || startPending || loading) return;
    // Lock editing and repeated clicks before the first await.
    busy = "applying";
    const target = draft;
    const version = draft.version;
    const submitted = draft.policy;
    resetChecker();
    render();
    const checked = await validateDraft({ send, policy: submitted });
    validation = { draft: target, version, ...checked };
    if (!checked.ok) return finish(() => t("optionsStartInvalid"));
    // Another page may have started while this one was open. Never apply over that policy.
    const current = await loadActivePolicy({ send });
    if (current.state === "ready") return adopt(current, () => t("optionsStartedElsewhere"));
    if (current.state !== "uninitialized") return checkStart();
    startPending = true;
    const result = await startDraft({ send, policy: submitted });
    return checkStart(result);
  }

  async function apply() {
    if (!canApply()) return;
    const target = draft;
    const version = draft.version;
    const submitted = draft.policy;
    const expectedEtag = view.active.etag;
    busy = "applying";
    resetChecker();
    validateGate.next();
    validationNote = "";
    applyNote = t("optionsApplyValidating");
    render();
    const checked = await validateDraft({ send, policy: submitted });
    if (!checked.ok) {
      validation = { draft: target, version, ...checked };
      finish(() => (DRAFT_CODES.has(checked.code) ? t("optionsApplyDraftProblem") : t("optionsApplyValidateFailed", errorText(checked.code))));
      return;
    }
    applyNote = t("optionsApplying");
    render();
    const result = await applyDraft({ send, expectedEtag, policy: submitted });
    if (result.outcome === "applied") return confirmApplied({ etag: result.etag, revision: result.revision });
    if (result.outcome === "refused") {
      if (result.code === "policy_conflict") {
        blocked = "conflict";
        finish(() => t("optionsApplyConflict"));
        return;
      }
      if (result.problems.length) validation = { draft: target, version, ok: false, code: "policy_invalid", problems: result.problems };
      finish(() => t("optionsApplyRefused", errorText(result.code)));
      return;
    }
    pending = { expectedEtag, submitted, code: result.code };
    return checkUnknown();
  }

  /** The apply that was answered and the reading of the policy it stored. */
  let applied = null;
  async function confirmApplied(outcome = applied) {
    applied = outcome;
    blocked = null;
    busy = "checking";
    applyNote = t("optionsAppliedLoading", String(outcome.revision));
    render();
    const loaded = await loadActivePolicy({ send });
    if (loaded.state === "ready") {
      applied = null;
      adopt(loaded, () =>
        loaded.active.etag === outcome.etag
          ? t("optionsApplied", String(outcome.revision))
          : t("optionsAppliedNewer", String(outcome.revision), String(loaded.active.revision)),
      );
      return;
    }
    blocked = "applied";
    finish(() => t("optionsAppliedUnread", String(outcome.revision), loadReason(loaded)));
  }

  /** An apply whose outcome is unknown, compared with the policy read afterwards. */
  let pending = null;
  async function checkUnknown() {
    blocked = null;
    busy = "checking";
    applyNote = t("optionsApplyUnknownChecking", errorText(pending.code));
    render();
    const loaded = await loadActivePolicy({ send });
    if (loaded.state !== "ready") {
      blocked = "unknown";
      finish(() => t("optionsApplyUnknown", loadReason(loaded)));
      return;
    }
    const verdict = compareAfterApply({ expectedEtag: pending.expectedEtag, submitted: pending.submitted, loaded: loaded.active });
    const answered = pending.code;
    pending = null;
    if (verdict === "applied") {
      adopt(loaded, () => t("optionsApplyConfirmed", String(loaded.active.revision)));
    } else if (verdict === "not_applied") {
      finish(() => t("optionsApplyNotApplied", errorText(answered)));
    } else {
      blocked = "changed";
      finish(() => t("optionsApplyChanged", String(loaded.active.revision)));
    }
  }

  function showRelaxConfirm(found) {
    const list = $("relax-confirm-list");
    const fragment = document.createDocumentFragment();
    for (const item of found) {
      const li = document.createElement("li");
      li.textContent = relaxationText(item);
      fragment.append(li);
    }
    list.replaceChildren(fragment);
    confirming = { draft, version: draft.version };
    $("relax-confirm").hidden = false;
    // Cancel first, so that Enter or Space right away does not apply.
    $("relax-confirm-no").focus();
  }

  function hideRelaxConfirm(focusApply) {
    if (confirming === null) return;
    confirming = null;
    $("relax-confirm").hidden = true;
    if (focusApply) $("apply").focus();
  }

  function requestApply() {
    if (!canApply()) return;
    const found = draft.relaxations();
    if (found.length === 0) void apply();
    else showRelaxConfirm(found);
  }

  // --- Loading ------------------------------------------------------------------------------------

  async function load() {
    if (locked()) return;
    const token = loadGate.next();
    validateGate.next();
    loading = true;
    busy = null;
    blocked = null;
    applied = null;
    pending = null;
    view = { state: "loading" };
    onboarding = false;
    draft = null;
    notice = "";
    validation = null;
    validationNote = "";
    applyNote = "";
    for (const key of LAYER_KEYS) { exports[key] = null; imports[key] = null; }
    $("reload-confirm").hidden = true;
    confirming = null;
    $("relax-confirm").hidden = true;
    resetChecker();
    checkerNote = "";
    for (const key of LAYER_KEYS) buildRules(key);
    invalidateTests();
    render();
    const reading = loadActivePolicy({ send });
    void refreshApprovals();
    const result = await reading;
    if (!loadGate.isCurrent(token)) return;
    loading = false;
    view = result;
    draft = result.state === "ready" ? createDraft(result.active, { editable: result.editable }) : null;
    if (result.state === "uninitialized") {
      onboarding = true;
      try {
        const preset = await loadPreset({ send });
        if (!loadGate.isCurrent(token)) return;
        // Kept only as the editor's comparison baseline; status remains uninitialized.
        view = { ...result, active: preset };
        draft = createDraft(preset, { editable: true });
        startPending = false;
      } catch {
        applyNote = t("optionsPresetFailed");
      }
      await refreshVault();
    }
    for (const key of LAYER_KEYS) buildRules(key);
    render();
  }

  function showReloadConfirm(shown) {
    $("reload-confirm").hidden = !shown;
    (shown ? $("reload-confirm-yes") : $("reload")).focus();
  }

  // --- Events -------------------------------------------------------------------------------------

  for (const key of CATEGORY_KEYS) {
    $(`category-${key}`).addEventListener("change", (event) => {
      if (!canEdit()) return render();
      edited(draft.setCategory(key, event.target.checked));
    });
  }
  for (const key of LAYER_KEYS) {
    $(`${LAYER_ID[key]}-import`).addEventListener("click", () => chooseImport(key));
    const input = $(`${LAYER_ID[key]}-import-file`);
    input.addEventListener("change", () => {
      if (importing?.key === key && !importing.reading) {
        importing.reading = true;
        void importSelected(importing, input.files?.[0] ?? null);
      }
    });
    input.addEventListener("cancel", () => {
      if (importing?.key === key && !importing.reading) finishImport(importing);
    });
    $(`${LAYER_ID[key]}-export`).addEventListener("click", () => exportLayer(key));
    $(`${LAYER_ID[key]}-enabled`).addEventListener("change", (event) => {
      if (!canEdit()) return render();
      edited(draft.setLayerEnabled(key, event.target.checked));
    });
    $(`checker-add-${LAYER_ID[key]}`).addEventListener("click", () => addTo(key));
  }
  // Enter checks, except while an input method is composing.
  const onEnter = (run) => (event) => {
    if (event.key !== "Enter" || event.isComposing || event.keyCode === 229) return;
    event.preventDefault();
    void run();
  };
  for (const kind of RULE_KINDS) $(`checker-kind-${kind}`).addEventListener("change", checkerChanged);
  for (const id of ["checker-rule", "checker-url"]) {
    $(id).addEventListener("input", checkerChanged);
    $(id).addEventListener("keydown", onEnter(runCheck));
  }
  $("checker-run").addEventListener("click", () => void runCheck());
  $("checker-approve").addEventListener("click", () => void approve());
  $("checker-apply-edit").addEventListener("click", reflectEdit);
  $("checker-cancel-edit").addEventListener("click", cancelEdit);
  const help = $("checker-help-toggle");
  help.addEventListener("click", () => {
    helpPinned = !helpPinned;
    showHelp(helpPinned);
  });
  help.addEventListener("focus", () => showHelp(true));
  help.addEventListener("blur", () => showHelp(helpPinned));
  help.addEventListener("mouseenter", () => showHelp(true));
  help.addEventListener("mouseleave", () => showHelp(helpPinned || document.activeElement === help));
  help.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    helpPinned = false;
    showHelp(false);
  });
  $("test-url").addEventListener("input", () => invalidateTests());
  $("test-url").addEventListener("keydown", onEnter(runUrlTest));
  $("test-url-run").addEventListener("click", () => void runUrlTest());
  $("validate").addEventListener("click", () => void runValidation());
  $("discard").addEventListener("click", () => {
    if (!draft?.dirty() || locked()) return;
    draft.discard();
    resetChecker();
    edited(true, LAYER_KEYS);
    notice = t("optionsDiscarded");
    render();
  });
  $("apply").addEventListener("click", requestApply);
  $("start-recording").addEventListener("click", () => void startRecording());
  $("start-recheck").addEventListener("click", () => void checkStart());
  $("vault-recheck").addEventListener("click", () => void refreshVault());
  $("relax-confirm-no").addEventListener("click", () => hideRelaxConfirm(true));
  $("relax-confirm").addEventListener("keydown", (event) => {
    if (event.key === "Escape") hideRelaxConfirm(true);
  });
  $("relax-confirm-yes").addEventListener("click", () => {
    const about = confirming;
    hideRelaxConfirm(false);
    // Only the draft that was confirmed is applied; a change since then asks again.
    if (about && about.draft === draft && about.version === draft.version) void apply();
  });
  $("apply-recheck").addEventListener("click", () => {
    if (busy !== null) return;
    if (blocked === "applied" && applied) void confirmApplied();
    else if (blocked === "unknown" && pending) void checkUnknown();
  });
  $("reload").addEventListener("click", () => {
    if (locked()) return;
    if (draft?.dirty()) showReloadConfirm(true);
    else void load();
  });
  $("reload-confirm-yes").addEventListener("click", () => {
    $("reload-confirm").hidden = true;
    // The reload button is disabled while reading, so focus waits on the section heading.
    $("active-heading").focus();
    void load();
  });
  $("reload-confirm-no").addEventListener("click", () => showReloadConfirm(false));

  // Chrome may skip this prompt (for instance before any interaction with the page); the in-page
  // reload asks on its own. The draft is not kept anywhere to survive the page.
  window.addEventListener("beforeunload", (event) => {
    if (!draft?.dirty()) return;
    event.preventDefault();
    event.returnValue = "";
  });

  // The checker starts on Pattern, as options.html marks it.
  if (!checkerKind()) $("checker-kind-pattern").checked = true;
  const ready = load();
  return {
    ready,
    reload: load,
    get view() {
      return view;
    },
    get draft() {
      return draft;
    },
    get busy() {
      return busy;
    },
    get blocked() {
      return blocked;
    },
  };
}
