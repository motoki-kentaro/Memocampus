// The Privacy settings page (options.html). It reads the active policy from the host and lets the
// person edit a draft of the basic protection in memory. It applies nothing: there is no call that
// writes the policy, initializes it or migrates it. Rule text is shown with textContent only and
// never logged.

import { describeErrorCode, createLatestGate } from "./search-client.js";
import { CATEGORY_KEYS, LAYER_KEYS, LAYER_OF_CATEGORY, createDraft, loadActivePolicy } from "./options-state.js";

const LAYER_ID = { neverSave: "never-save", saveButNoMcp: "save-but-no-mcp" };
const LAYER_NAME = { neverSave: "保存しないグループ", saveButNoMcp: "MCPサーバに見せないグループ" };

// The label names the state in words, so it never depends on colour alone.
const STATE_LABEL = {
  loading: "確認中…",
  ready: "Ready（利用可能）",
  uninitialized: "Not Initialized（未初期化）",
  invalid: "Invalid（読み取れません）",
  unsupported: "Invalid（このバージョンでは使えません）",
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
  unsupported: `プライバシー設定に、このバージョンでは使えない内容が含まれているため、自動記録を停止しています。設定ファイルは変更していません。\n${NOT_SHOWN}`,
  busy: `プライバシー設定が使用中です。しばらくしてから再読み込みしてください。\n${NOT_SHOWN}`,
  unavailable: `プライバシー設定にアクセスできません。\n${NOT_SHOWN}`,
  invalid_response: `Native Hostから不正な応答を受け取りました。\n${NOT_SHOWN}`,
};
const READY_V1 =
  "この設定は旧形式（v1）のため、この画面では編集できません。編集するには明示的な移行（Migration）が必要です。移行機能は今後この画面に追加します。自動では移行しません。";
const READY_V2 = "基本保護を変更できます。変更は下書きとしてこの画面にだけ保持され、まだ適用されません。";

function ruleKind(rule) {
  if (typeof rule === "string") return "Pattern（v1）";
  return rule.kind === "regex" ? "Regex" : "Pattern";
}

export function startOptionsPage({ document, window, send }) {
  const $ = (id) => document.querySelector(`#${id}`);
  const gate = createLatestGate();
  let view = { state: "loading" };
  let draft = null;
  let loading = false;
  let notice = "";

  function explanation() {
    if (view.state === "ready") return view.editable ? READY_V2 : READY_V1;
    if (view.state === "host_unavailable") return `${describeErrorCode(view.code)}\n${NOT_SHOWN}`;
    return EXPLANATION[view.state] ?? NOT_SHOWN;
  }

  function renderStatus() {
    const ready = view.state === "ready";
    const state = $("policy-state");
    state.textContent = STATE_LABEL[view.state] ?? STATE_LABEL.unavailable;
    state.dataset.state = view.state;
    $("policy-version").textContent = ready ? `Schema ${view.active.schemaVersion} / Engine ${view.active.engineVersion}` : "—";
    $("policy-revision").textContent = ready ? String(view.active.revision) : "—";
    $("policy-explanation").textContent = explanation();
    $("reload").disabled = loading;
    $("options").setAttribute("aria-busy", String(loading));
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
      input.disabled = !draft?.editable;
      const layerNote = $(`category-${key}-note`);
      const layer = LAYER_OF_CATEGORY[key];
      layerNote.textContent = draft && !draft.layer(layer).enabled ? `「${LAYER_NAME[layer]}」が無効のため、現在この項目は判定に使われていません。` : "";
      layerNote.hidden = layerNote.textContent === "";
      // A warning only: the person's choice stands.
      $(`category-${key}-warning`).hidden = !weakened.has(key);
    }
  }

  function ruleRow(rule) {
    const row = document.createElement("tr");
    const kind = document.createElement("td");
    kind.textContent = ruleKind(rule);
    const text = document.createElement("td");
    const code = document.createElement("code");
    code.textContent = typeof rule === "string" ? rule : rule.pattern;
    text.append(code);
    const action = document.createElement("td");
    action.className = "muted";
    action.textContent = "編集不可";
    row.append(kind, text, action);
    return row;
  }

  function renderLayer(key) {
    const id = LAYER_ID[key];
    const layer = draft?.layer(key);
    const rules = layer?.rules ?? [];
    $(`${id}-state`).textContent = !layer
      ? ""
      : `${layer.enabled ? "このグループは有効です" : "このグループは無効です（ルールと基本保護は判定に使われません）"}。ルール ${rules.length}件。ルールの追加・変更・削除は次の開発段階で利用可能になります。`;
    $(`${id}-state`).hidden = !layer;
    const body = $(`${id}-rules`);
    const fragment = document.createDocumentFragment();
    for (const rule of rules) fragment.append(ruleRow(rule));
    body.replaceChildren(fragment);
    $(`${id}-table`).hidden = rules.length === 0;
    const empty = $(`${id}-empty`);
    empty.textContent = layer ? "ルールはありません。" : loading ? "" : NOT_SHOWN;
    empty.hidden = rules.length > 0 || empty.textContent === "";
  }

  function renderFooter() {
    const dirty = draft?.dirty() ?? false;
    $("dirty-status").textContent = dirty ? "未適用の変更があります" : notice;
    $("discard").disabled = !dirty;
    // Applying comes with the rule tables (Slice 2); until then nothing can apply a draft.
    $("apply").disabled = true;
  }

  function render() {
    renderStatus();
    renderCategories();
    for (const key of LAYER_KEYS) renderLayer(key);
    renderFooter();
  }

  async function load() {
    const token = gate.next();
    loading = true;
    view = { state: "loading" };
    draft = null;
    notice = "";
    $("reload-confirm").hidden = true;
    render();
    const result = await loadActivePolicy({ send });
    if (!gate.isCurrent(token)) return;
    loading = false;
    view = result;
    draft = result.state === "ready" ? createDraft(result.active, { editable: result.editable }) : null;
    render();
  }

  function showReloadConfirm(shown) {
    $("reload-confirm").hidden = !shown;
    (shown ? $("reload-confirm-yes") : $("reload")).focus();
  }

  for (const key of CATEGORY_KEYS) {
    $(`category-${key}`).addEventListener("change", (event) => {
      notice = "";
      draft?.setCategory(key, event.target.checked);
      render();
    });
  }
  $("discard").addEventListener("click", () => {
    if (!draft?.dirty()) return;
    draft.discard();
    notice = "変更を破棄しました。";
    render();
  });
  $("reload").addEventListener("click", () => {
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
  };
}
