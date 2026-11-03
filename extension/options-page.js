// The Privacy settings page (options.html). It reads the active policy from the host, lets the person
// edit a draft in memory, has the host check and test the draft, and applies it only when asked,
// in place of the policy it was made from (its ETag). It never initializes, starts or migrates, and
// never sends an apply twice by itself. Rule text and URLs are set as text or input values only and
// never logged.
//
// A rule enters the draft, or changes in it, only through the URL checker, and only after the host
// has matched exactly that rule against exactly that URL (see "URL checker" below). Rules are never
// typed into the tables, and the page never matches a rule itself.

import { describeErrorCode, createLatestGate } from "./search-client.js";
import {
  CATEGORY_KEYS,
  LAYER_KEYS,
  LAYER_OF_CATEGORY,
  RULE_KINDS,
  applyDraft,
  compareAfterApply,
  createDraft,
  loadActivePolicy,
  matchState,
  sameRule,
  testRule,
  testUrl,
  validateDraft,
} from "./options-state.js";

const LAYER_ID = { neverSave: "never-save", saveButNoMcp: "save-but-no-mcp" };
const LAYER_NAME = { neverSave: "保存しないグループ", saveButNoMcp: "MCPサーバに見せないグループ" };
const CATEGORY_NAME = { authentication: "認証関連", credentials: "認証情報", government: "行政サイト", webmail: "Webメール", aiChats: "AIチャット" };
const KIND_LABEL = { pattern: "Pattern", regex: "Regex" };

// The label names the state in words, so it never depends on colour alone.
const STATE_LABEL = {
  loading: "確認中…",
  ready: "Ready（利用可能）",
  uninitialized: "Not Initialized（未初期化）",
  invalid: "Invalid（設定ファイルが無効です）",
  unsupported: "Unsupported（現在のバージョンでは対応していません）",
  busy: "Unavailable（使用中）",
  unavailable: "Unavailable（アクセスできません）",
  host_unavailable: "Unavailable（Native Hostに接続できません）",
  invalid_response: "Unavailable（不正な応答）",
};

const NOT_SHOWN = "現在のプライバシー設定を取得できていないため、内容は表示していません。";
const EXPLANATION = {
  loading: "",
  uninitialized:
    "プライバシー設定はまだ初期化されていません。初期化されるまで自動記録は行われません。初期化はサイドパネルから行えます。この画面から初期化することはありません。",
  invalid: `プライバシー設定を読み取れない（欠落・破損）ため、自動記録を停止しています。設定ファイルは変更していません。復旧機能は今後この画面に追加します。\n${NOT_SHOWN}`,
  unsupported: `プライバシー設定に、現在のバージョンでは対応していない内容が含まれているため、自動記録を停止しています。設定ファイルは変更していません。新しいバージョンで作られた設定であれば、Native Hostを更新してください。\n${NOT_SHOWN}`,
  busy: `プライバシー設定が使用中です。しばらくしてから再読み込みしてください。\n${NOT_SHOWN}`,
  unavailable: `プライバシー設定にアクセスできません。\n${NOT_SHOWN}`,
  invalid_response: `Native Hostから不正な応答を受け取りました。\n${NOT_SHOWN}`,
};
const READY_V1 =
  "この設定は旧形式（v1）のため、この画面では編集できません。編集するには明示的な移行（Migration）が必要です。移行機能は今後この画面に追加します。自動では移行しません。";
const READY_V2 = "基本保護とルールを変更できます。変更は下書きとしてこの画面にだけ保持され、「適用」を押すまで反映されません。";

// What the host says about a rule or a draft. Never the rule's text: the host does not send it back.
const PROBLEM_TEXT = {
  invalid_pattern: "パターンの書き方が正しくありません（空欄、空白や \\ ? # @ : を含む、ホスト名がない、など）。",
  invalid_regex: "正規表現の書き方が正しくありません。",
  unsupported_regex: "このバージョンでは使えない正規表現です（先読み・後読み・後方参照）。",
  regex_limit: "正規表現が長すぎるか複雑すぎます。上限は正規表現ルールの数によって変わります。",
  invalid_rule_form: "ルールの形式が正しくありません。",
  invalid_rule: "ルールの形式が正しくありません。",
  too_many_rules: "このグループのルールが多すぎます（上限1,000件）。",
  too_many_regex_rules: "正規表現ルールが多すぎます（2つのグループの合計で上限128件）。",
  policy_too_large: "設定が大きすぎて保存できません。",
  invalid_version: "設定のバージョンが正しくありません。",
  invalid_revision: "設定のRevisionが正しくありません。",
  invalid_policy_document: "設定の形式が正しくありません。",
  invalid_policy: "この下書きは使用できません。",
};
const ERROR_TEXT = {
  policy_invalid: "下書きに問題があります。",
  policy_unsupported: "下書きに、このバージョンでは使えない正規表現が含まれています。",
  policy_conflict: "ほかの画面またはプロセスでプライバシー設定が変更されています。",
  policy_busy: "プライバシー設定が使用中です。",
  policy_unavailable: "プライバシー設定にアクセスできません。",
  policy_not_initialized: "プライバシー設定が初期化されていません。",
  policy_version_mismatch: "下書きと現在の設定のバージョンが一致しません。",
  invalid_etag: "適用に必要な情報（ETag）が正しくありません。",
  policy_draft_too_large: "下書きが大きすぎます（上限1 MiB）。",
  invalid_policy_draft: "下書きの形式が正しくありません。",
  invalid_url: "このURLは判定に使えません（長すぎるなど）。",
  timeout: "Native Hostから応答がありません。",
  host_unavailable: "Native Hostに接続できません。",
  invalid_response: "Native Hostから不正な応答を受け取りました。",
  unknown_type: "Native Hostがこの操作に対応していません。Hostを更新してください。",
};
const problemText = (code) => PROBLEM_TEXT[code] ?? ERROR_TEXT[code] ?? describeErrorCode(code);
const errorText = (code) => ERROR_TEXT[code] ?? PROBLEM_TEXT[code] ?? describeErrorCode(code);
const DRAFT_CODES = new Set(["policy_invalid", "policy_unsupported"]);

/** Why a reading of the policy gave no active policy, in words. */
const loadReason = (loaded) => `（状態: ${STATE_LABEL[loaded.state] ?? STATE_LABEL.unavailable}${loaded.code && loaded.state === "host_unavailable" ? `、${errorText(loaded.code)}` : ""}）`;

const DECISION_TEXT = { never_save: "保存しません", save_but_no_mcp: "Vaultに保存しますが、MCPには見せません", allow: "保存対象です" };

function relaxationText(item) {
  if (item.type === "category") return `基本保護「${CATEGORY_NAME[item.key]}」をOFFにする`;
  if (item.type === "layer") return `「${LAYER_NAME[item.key]}」を無効にする`;
  return `「${LAYER_NAME[item.key]}」の既存のルール${item.count}件を削除するか、内容・種類を変更する`;
}

export function startOptionsPage({ document, window, send }) {
  const $ = (id) => document.querySelector(`#${id}`);
  const loadGate = createLatestGate();
  const validateGate = createLatestGate();
  const urlGate = createLatestGate();

  let view = { state: "loading" };
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

  const locked = () => busy === "applying" || busy === "checking";
  const canEdit = () => Boolean(draft?.editable) && !locked();
  const canApply = () => Boolean(draft?.editable) && draft.dirty() && busy === null && blocked === null && !loading;
  const currentValidation = () => (validation && validation.draft === draft && validation.version === draft.version ? validation : null);

  function explanation() {
    if (view.state === "ready") return view.editable ? READY_V2 : READY_V1;
    if (view.state === "host_unavailable") return `${describeErrorCode(view.code)}\n${NOT_SHOWN}`;
    return EXPLANATION[view.state] ?? NOT_SHOWN;
  }

  // --- Rendering ----------------------------------------------------------------------------------

  function renderStatus() {
    const ready = view.state === "ready";
    const state = $("policy-state");
    state.textContent = STATE_LABEL[view.state] ?? STATE_LABEL.unavailable;
    state.dataset.state = view.state;
    $("policy-version").textContent = ready ? `Schema ${view.active.schemaVersion} / Engine ${view.active.engineVersion}` : "—";
    $("policy-revision").textContent = ready ? String(view.active.revision) : "—";
    $("policy-explanation").textContent = explanation();
    $("reload").disabled = loading || locked();
    $("options").setAttribute("aria-busy", String(loading || busy !== null));
  }

  function renderCategories() {
    $("categories").hidden = !draft;
    const note = $("categories-note");
    note.textContent = !draft && !loading ? NOT_SHOWN : draft && !draft.editable ? "旧形式（v1）のため編集できません。" : "";
    note.hidden = note.textContent === "";
    const weakened = new Set(draft?.weakened() ?? []);
    for (const key of CATEGORY_KEYS) {
      const input = $(`category-${key}`);
      input.checked = draft ? draft.category(key) : false;
      input.disabled = !canEdit();
      const layerNote = $(`category-${key}-note`);
      const layer = LAYER_OF_CATEGORY[key];
      layerNote.textContent = draft && !draft.layerEnabled(layer) ? `「${LAYER_NAME[layer]}」が無効のため、現在この項目は判定に使われていません。` : "";
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
    const kind = typeof rule === "string" ? "Pattern（v1）" : KIND_LABEL[rule.kind];
    row.append(textCell(kind), cell(ruleCode(rule)), textCell("編集不可", "muted"));
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
    const place = `${LAYER_NAME[layerKey]} ${index + 1}行目`;
    const row = document.createElement("tr");
    const code = ruleCode(rule);
    code.id = `${id}-text`;
    const error = document.createElement("p");
    error.id = `${id}-error`;
    error.className = "rule-error";
    error.hidden = true;
    const edit = button("チェッカーで編集", "rule-edit", `${place}のルールをURLチェッカーで編集`);
    edit.setAttribute("aria-describedby", `${id}-error`);
    edit.addEventListener("click", () => startEdit(layerKey, index));
    const remove = button("削除", "rule-delete", `${place}のルールを削除`);
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
    $(`${id}-state`).textContent = !layer
      ? ""
      : `${layer.enabled ? "このグループは有効です" : "このグループは無効です（ルールと基本保護は判定に使われません）"}。ルール ${count}件。`;
    $(`${id}-state`).hidden = !layer;
    $(`${id}-table`).hidden = count === 0;
    const empty = $(`${id}-empty`);
    empty.textContent = layer ? "ルールはありません。" : loading ? "" : NOT_SHOWN;
    empty.hidden = count > 0 || empty.textContent === "";
    $(`${id}-hint`).hidden = !draft?.editable;
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
        controls.error.textContent = own.length ? `エラー: ${own.map((problem) => problemText(problem.code)).join(" ")}` : "";
        controls.error.hidden = own.length === 0;
        controls.code.dataset.invalid = own.length ? "true" : "";
      });
      for (const problem of problems.filter((problem) => problem.layer === key)) {
        if (problem.ruleIndex === null) layerMessages.push(problemText(problem.code));
        // A place this page does not have is not put on another row.
        else if (!rows[key][problem.ruleIndex]?.error) general.push(problemText(problem.code));
      }
      const layerError = $(`${LAYER_ID[key]}-error`);
      layerError.textContent = layerMessages.length ? `エラー: ${layerMessages.join(" ")}` : "";
      layerError.hidden = layerMessages.length === 0;
    }
    for (const problem of problems) if (problem.layer === null) general.push(problemText(problem.code));

    const status = $("validation-status");
    let text = validationNote;
    if (busy === "validating") text = "検証しています…";
    else if (current?.ok) text = "検証しました。この下書きは適用できます。";
    else if (current && DRAFT_CODES.has(current.code)) {
      const placed = problems.length - general.length;
      text = `検証の結果、下書きに問題があります（${problems.length}件）。${placed > 0 ? "該当する行とグループに表示しています。" : ""}${general.length ? ` ${general.join(" ")}` : ""}`;
    } else if (current) text = `検証できませんでした。${errorText(current.code)}`;
    status.textContent = text;
    status.hidden = text === "";
    status.dataset.result = current ? (current.ok ? "ok" : "error") : "";
  }

  function applyReason() {
    if (!draft) return "設定を読み込めていないため、適用できません。";
    if (!draft.editable) return "旧形式（v1）の設定は、この画面では適用できません。";
    if (busy === "applying" || busy === "checking") return "適用の処理中です。";
    if (busy === "validating") return "検証中です。";
    if (blocked === "conflict" || blocked === "changed") return "最新の設定を再読み込みするまで適用できません。";
    if (blocked) return "適用の結果を確認するまで、もう一度適用することはできません。";
    if (!draft.dirty()) return "未適用の変更はありません。";
    return "適用の前に、Native Hostで下書き全体を検証します。";
  }

  function renderFooter() {
    const dirty = draft?.dirty() ?? false;
    $("dirty-status").textContent = dirty ? "未適用の変更があります" : notice;
    $("discard").disabled = !dirty || locked();
    $("validate").disabled = !draft?.editable || busy !== null;
    $("apply").disabled = !canApply();
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
      showUrlResult("判定するURLを入力してください。", null);
      $("test-url").focus();
      return;
    }
    const epoch = testEpoch;
    const token = urlGate.next();
    showUrlResult("判定しています…", null);
    const answer = await testUrl({ send, policy: draft.policy, url });
    if (epoch !== testEpoch || !urlGate.isCurrent(token)) return;
    if (answer.ok) showUrlResult(`判定: ${DECISION_TEXT[answer.decision]}`, answer.decision);
    else if (DRAFT_CODES.has(answer.code)) showUrlResult("下書きに問題があるため判定できませんでした。「検証」で問題の箇所を確認してください。", null);
    else showUrlResult(`判定できませんでした。${errorText(answer.code)}`, null);
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
    if (!draft) return "設定を読み込めていないため、ルールを追加できません。";
    if (!draft.editable) return "旧形式（v1）の設定には、この画面からルールを追加できません。";
    if (locked()) return "適用の処理中は、ルールを追加・変更できません。";
    if (editing && !editTargetHolds()) return "編集対象のルールが変更または削除されたため、反映できません。「編集をやめる」を押してください。";
    const current = currentCheck();
    if (!current) {
      return complete(checkerInput())
        ? "「マッチングを確認」で一致を確認すると、追加できます。"
        : "ルールとチェック対象URLを入力し、「マッチングを確認」で一致を確認してください。";
    }
    if (current.state === "checking") return "確認中です。";
    if (current.state === "no_match") return "一致しなかったため、追加できません。ルールかURLを修正して、もう一度確認してください。";
    if (current.state !== "match") return "判定できなかったため、追加できません。";
    if (!editing) return "";
    if (sameRule(current, editing.rule)) return "ルールが変更されていません。";
    const others = draft.layer(editing.layer).rules.filter((_, index) => index !== editing.index);
    if (others.some((other) => sameRule(other, current))) return "同じルールがこのグループにすでにあります（登録済み）。";
    return "";
  }

  const canAddTo = (layerKey) => !editing && checkerReason() === "" && !draft.hasRule(layerKey, currentMatch());
  const canReflect = () => editing !== null && checkerReason() === "";

  const RESULT_TEXT = {
    checking: () => "確認中…",
    match: () => "一致しました",
    no_match: () => "一致しませんでした",
    rule_error: (code) => `ルールを確認してください。${problemText(code)}`,
    url_error: (code) => `URLを確認してください。${errorText(code)}`,
    failed: (code) => `判定できませんでした。${errorText(code ?? "invalid_response")}`,
  };

  function renderChecker() {
    const enabled = canCheck();
    for (const kind of RULE_KINDS) $(`checker-kind-${kind}`).disabled = !enabled;
    for (const id of ["checker-rule", "checker-url", "checker-run"]) $(id).disabled = !enabled;

    const current = currentCheck();
    const result = $("checker-result");
    result.textContent = current ? RESULT_TEXT[current.state](current.code) : "未チェック";
    result.dataset.result = current ? current.state : "none";

    const banner = $("checker-edit");
    banner.hidden = editing === null;
    banner.textContent = editing
      ? `編集中: 「${LAYER_NAME[editing.layer]}」の${editing.index + 1}行目のルール。一致を確認してから「変更を反映」を押すと、下書きのこのルールだけが置き換わります。`
      : "";
    $("checker-add").hidden = editing !== null;
    $("checker-edit-actions").hidden = editing === null;
    $("checker-apply-edit").disabled = !canReflect();
    $("checker-cancel-edit").disabled = locked();

    const reason = checkerReason();
    const match = currentMatch();
    for (const key of LAYER_KEYS) {
      $(`checker-add-${LAYER_ID[key]}`).disabled = !draft || !canAddTo(key);
      const notes = [];
      if (draft && match && draft.hasRule(key, match)) notes.push("登録済み: このグループに同じルールがあります。");
      if (draft?.editable && !draft.layerEnabled(key)) {
        notes.push("このグループは無効です。ルールは追加されますが、適用後もグループを有効にするまで判定に使われません。");
      }
      if (key === "saveButNoMcp" && draft && match && draft.hasRule("neverSave", match)) {
        notes.push("同じルールが「保存しないグループ」にもあります。保存しないグループの判定が優先されるため、一致したURLは保存されません。");
      }
      const note = $(`checker-${LAYER_ID[key]}-note`);
      note.textContent = notes.join(" ");
      note.hidden = notes.length === 0;
    }
    $("checker-gate").textContent =
      reason || (editing ? "一致しました。「変更を反映」で下書きのルールを置き換えられます。" : "一致しました。追加先のグループを選んでください。");
    const status = $("checker-status");
    status.textContent = checkerNote;
    status.hidden = checkerNote === "";
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
    const answer = await testRule({ send, rule: { kind: input.kind, pattern: input.pattern }, url: input.url });
    if (!checkGate.isCurrent(token) || check?.token !== token) return;
    check = { ...check, state: matchState(answer), code: answer.ok ? null : answer.code };
    renderChecker();
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
    checkerNote = `「${LAYER_NAME[layerKey]}」の下書きに追加しました（${index + 1}行目）。「適用」を押すまで反映されません。`;
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
    checkerNote = `「${LAYER_NAME[layer]}」の${index + 1}行目のルールを下書きで変更しました。「適用」を押すまで反映されません。`;
    renderChecker();
    showRow(layer, index);
  }

  function cancelEdit() {
    if (!editing || locked()) return;
    editing = null;
    checkerNote = "編集をやめました。下書きは変更していません。";
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
      validationNote = "検証中に下書きが変更されたため、結果は表示していません。もう一度「検証」を押してください。";
    } else {
      validation = { draft: target, version, ...result };
    }
    render();
  }

  function finish(note) {
    busy = null;
    applyNote = note;
    render();
    $("apply-status").focus();
  }

  /** Takes the policy just read as the active one, with a new draft of it. */
  function adopt(loaded, note) {
    view = loaded;
    draft = createDraft(loaded.active, { editable: loaded.editable });
    blocked = null;
    validation = null;
    validationNote = "";
    resetChecker();
    for (const key of LAYER_KEYS) buildRules(key);
    invalidateTests();
    finish(note);
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
    applyNote = "下書きを検証しています…";
    render();
    const checked = await validateDraft({ send, policy: submitted });
    if (!checked.ok) {
      validation = { draft: target, version, ...checked };
      finish(DRAFT_CODES.has(checked.code) ? "下書きに問題があるため、適用していません。" : `下書きを検証できなかったため、適用していません。${errorText(checked.code)}`);
      return;
    }
    applyNote = "適用しています…";
    render();
    const result = await applyDraft({ send, expectedEtag, policy: submitted });
    if (result.outcome === "applied") return confirmApplied({ etag: result.etag, revision: result.revision });
    if (result.outcome === "refused") {
      if (result.code === "policy_conflict") {
        blocked = "conflict";
        finish(
          "ほかの画面またはプロセスでプライバシー設定が変更されていたため、適用しませんでした。自動では上書きしません。下書きはそのまま残しています。内容を確認してから「再読み込み」で最新の設定を読み込んでください（再読み込みすると、この下書きは破棄されます）。",
        );
        return;
      }
      if (result.problems.length) validation = { draft: target, version, ok: false, code: "policy_invalid", problems: result.problems };
      finish(`適用しませんでした。${errorText(result.code)} 下書きはそのまま残しています。`);
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
    applyNote = `適用しました（Revision ${outcome.revision}）。最新の状態を読み込んでいます…`;
    render();
    const loaded = await loadActivePolicy({ send });
    if (loaded.state === "ready") {
      applied = null;
      adopt(
        loaded,
        loaded.active.etag === outcome.etag
          ? `適用しました（Revision ${outcome.revision}）。`
          : `適用しました（Revision ${outcome.revision}）。その後ほかの変更もあったため、最新の設定（Revision ${loaded.active.revision}）を表示しています。`,
      );
      return;
    }
    blocked = "applied";
    finish(
      `適用は成功しましたが（Revision ${outcome.revision}）、最新状態を取得できませんでした。${loadReason(loaded)} 下書きは残しています。「適用結果を再確認」を押してください。`,
    );
  }

  /** An apply whose outcome is unknown, compared with the policy read afterwards. */
  let pending = null;
  async function checkUnknown() {
    blocked = null;
    busy = "checking";
    applyNote = `適用の結果が不明です。${errorText(pending.code)} 現在の設定を読み込んで確認しています…`;
    render();
    const loaded = await loadActivePolicy({ send });
    if (loaded.state !== "ready") {
      blocked = "unknown";
      finish(
        `適用の結果が不明です。適用されたかどうかを確認できませんでした。${loadReason(loaded)} 自動ではもう一度適用しません。下書きは残しています。Native Hostの状態を確かめてから「適用結果を再確認」を押してください。`,
      );
      return;
    }
    const verdict = compareAfterApply({ expectedEtag: pending.expectedEtag, submitted: pending.submitted, loaded: loaded.active });
    const answered = pending.code;
    pending = null;
    if (verdict === "applied") {
      adopt(loaded, `適用されたことを確認しました（Revision ${loaded.active.revision}）。`);
    } else if (verdict === "not_applied") {
      finish(`確認した時点では、設定は適用前のままでした（${errorText(answered)}）。自動ではもう一度適用しません。下書きは残しています。必要であれば、もう一度「適用」を押してください。`);
    } else {
      blocked = "changed";
      finish(
        `適用の結果が不明なうちに、プライバシー設定がこの下書きとは異なる内容に変わっていました（Revision ${loaded.active.revision}）。自動では上書きしません。下書きは残しています。内容を確認してから「再読み込み」で最新の設定を読み込んでください。`,
      );
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
    draft = null;
    notice = "";
    validation = null;
    validationNote = "";
    applyNote = "";
    $("reload-confirm").hidden = true;
    confirming = null;
    $("relax-confirm").hidden = true;
    resetChecker();
    checkerNote = "";
    for (const key of LAYER_KEYS) buildRules(key);
    invalidateTests();
    render();
    const result = await loadActivePolicy({ send });
    if (!loadGate.isCurrent(token)) return;
    loading = false;
    view = result;
    draft = result.state === "ready" ? createDraft(result.active, { editable: result.editable }) : null;
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
    notice = "変更を破棄しました。";
    render();
  });
  $("apply").addEventListener("click", requestApply);
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
