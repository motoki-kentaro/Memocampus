// Vault settings section of the side panel. All file access happens in the native host; this
// only sends requests and renders the host's answers.

import { SearchError } from "./search-client.js";
import {
  ENCRYPTION_NOTE,
  LOCATION_NOTE,
  PHRASE_CHECK_TITLE,
  PHRASE_NOTE,
  PHRASE_TITLE,
  PUBLICATION_NOTE,
  PUBLICATION_REVIEW,
  RECOVERY_TITLE,
  VERDICT_TEXT,
  checkAnswers,
  describePhraseProblem,
  describePublication,
  describeReasons,
  describeStatus,
  formatCounts,
  pickPositions,
  vaultRequest,
} from "./vault-client.js";

const PUBLICATION_TYPES = new Set(["vault_publication_confirm", "vault_publication_revoke"]);
/** Requests that may open a Windows dialog when no path is given. */
const DIALOG_TYPES = new Set(["vault_create", "vault_open", "vault_backup", "vault_create_encrypted", "vault_recover"]);
/** Creation of an encrypted vault ends (either way) with these. */
const CREATION_END_TYPES = new Set(["vault_create_encrypted_commit", "vault_create_encrypted_cancel"]);

const $ = (selector) => document.querySelector(selector);

/**
 * `reveal` brings the settings view into sight (the side panel's settings tab); `onStatus` receives
 * every rendered status (null when the host could not be asked).
 */
export function createVaultPanel({ send, onVaultChanged, reveal = () => {}, onStatus = () => {} }) {
  const stateText = $("#vault-state");
  const pathText = $("#vault-path");
  const countsText = $("#vault-counts");
  const locationBox = $("#vault-location");
  const verdictText = $("#vault-verdict");
  const reasonsList = $("#vault-reasons");
  const createDefault = $("#vault-create-default");
  const defaultPathText = $("#vault-default-path");
  const createPick = $("#vault-create-pick");
  const openPick = $("#vault-open-pick");
  const backupPick = $("#vault-backup-pick");
  const manualInput = $("#vault-path-input");
  const approvalBox = $("#vault-approval");
  const approvalTitle = $("#vault-approval-title");
  const approvalReasons = $("#vault-approval-reasons");
  const approveButton = $("#vault-approve");
  const approveCancel = $("#vault-approve-cancel");
  const messageText = $("#vault-message");
  // MCP publication of the vault shown (Issue #51): decided here, never through MCP.
  const publicationBox = $("#vault-publication");
  const publicationState = $("#vault-publication-state");
  const publicationNote = $("#vault-publication-note");
  const publicationConfirm = $("#vault-publication-confirm");
  const publicationRevoke = $("#vault-publication-revoke");
  const publicationReview = $("#vault-publication-review");
  const publicationReviewTitle = $("#vault-publication-review-title");
  const publicationApprove = $("#vault-publication-approve");
  const publicationCancel = $("#vault-publication-cancel");
  $("#vault-note").textContent = LOCATION_NOTE;
  // Issue #51 Slice B: encryption choice, the recovery phrase shown once, and recovery.
  const encryptionBox = $("#vault-encryption");
  const encryptionNote = $("#vault-encryption-note");
  const encryptedChoice = $("#vault-encryption-encrypted");
  const plainChoice = $("#vault-encryption-plain");
  const onboardingBox = $("#vault-onboarding");
  const onboardingTitle = $("#vault-onboarding-title");
  const phraseShow = $("#vault-phrase-show");
  const phraseList = $("#vault-phrase");
  const phraseNote = $("#vault-phrase-note");
  const phraseWritten = $("#vault-phrase-written");
  const phraseCheck = $("#vault-phrase-check");
  const phraseCheckTitle = $("#vault-phrase-check-title");
  const phraseInputs = $("#vault-phrase-inputs");
  const phraseConfirm = $("#vault-phrase-confirm");
  const phraseBack = $("#vault-phrase-back");
  const onboardingCancel = $("#vault-onboarding-cancel");
  const creationPendingBox = $("#vault-creation-pending");
  const creationPendingText = $("#vault-creation-pending-text");
  const creationDiscard = $("#vault-creation-discard");
  const recoveryOffer = $("#vault-recovery-offer");
  const recoveryStart = $("#vault-recovery-start");
  const recoveryBox = $("#vault-recovery");
  const recoveryTitle = $("#vault-recovery-title");
  const recoveryInput = $("#vault-recovery-input");
  const recoverySubmit = $("#vault-recovery-submit");
  const recoveryCancel = $("#vault-recovery-cancel");
  encryptionNote.textContent = ENCRYPTION_NOTE;
  phraseNote.textContent = PHRASE_NOTE;
  phraseCheckTitle.textContent = PHRASE_CHECK_TITLE;

  const actionButtons = [createDefault, createPick, openPick, backupPick, publicationConfirm, publicationRevoke, publicationApprove, ...document.querySelectorAll("[data-manual]")];
  const creationButtons = [createDefault, createPick, openPick, document.querySelector('[data-manual="vault_create"]'), document.querySelector('[data-manual="vault_open"]')];
  // The phrase lives only here, in this page's memory, until the creation ends; it is never stored.
  let onboarding = null;
  let answerInputs = [];
  let recoveryTarget = null;
  let status = null;
  // Each status asked for; only the latest answer is shown (an older one arriving late never
  // replaces it, so the vault shown and the token a backup sends always come from one answer).
  let statusRequest = 0;
  // Resolves with the status shown once the latest request is answered: the caller of a superseded
  // request gets the newer status, never the older one it asked for.
  let latestStatus = Promise.resolve(null);
  let busy = false;
  let pendingApproval = null;

  function fillList(list, items) {
    list.replaceChildren(...items.map((text) => Object.assign(document.createElement("li"), { textContent: text })));
  }

  function updateButtons() {
    const manageable = status?.managementEnabled === true;
    for (const button of actionButtons) button.disabled = busy || !manageable;
    if (!busy && manageable) {
      createDefault.disabled = status.defaultExists === true;
      backupPick.disabled = !backupSource();
      document.querySelector('[data-manual="vault_backup"]').disabled = !backupSource();
      publicationConfirm.disabled = !backupSource() || status.publication !== "pending";
      publicationApprove.disabled = publicationConfirm.disabled;
      publicationRevoke.disabled = !backupSource() || status.publication !== "confirmed";
    }
    // No other vault is created or opened while a creation waits for its phrase confirmation.
    const creationOpen = Boolean(onboarding) || Boolean(status?.creationPending);
    if (creationOpen) for (const button of creationButtons) button.disabled = true;
    const canEncrypt = status?.encryptionAvailable === true;
    encryptedChoice.disabled = busy || !canEncrypt;
    plainChoice.disabled = busy;
    if (!canEncrypt && status) plainChoice.checked = true;
    for (const button of [phraseWritten, phraseConfirm, phraseBack, onboardingCancel, creationDiscard, recoveryStart, recoverySubmit, recoveryCancel]) button.disabled = busy;
  }

  function encryptedChosen() {
    return encryptedChoice.checked && status?.encryptionAvailable === true;
  }

  function createType() {
    return encryptedChosen() ? "vault_create_encrypted" : "vault_create";
  }

  /** The vault a backup or a publication decision is about: the one shown, if it is ready and named by a token. */
  function backupSource() {
    if (status?.state !== "ready" || typeof status.vaultToken !== "string" || !/^[0-9a-f]{64}$/.test(status.vaultToken)) return null;
    return { vaultPath: status.vaultPath, vaultToken: status.vaultToken };
  }

  function render() {
    stateText.textContent = describeStatus(status);
    pathText.hidden = !status?.vaultPath;
    pathText.textContent = status?.vaultPath || "";
    const counts = status?.state === "ready" ? formatCounts(status.counts) : "";
    countsText.hidden = !counts;
    countsText.textContent = counts;

    const location = status?.location;
    locationBox.hidden = !location;
    if (location) {
      locationBox.dataset.verdict = location.verdict;
      const approvedNote = location.verdict === "needs_approval" && status.locationApproved ? "（承認済み）" : "";
      verdictText.textContent = `${VERDICT_TEXT[location.verdict] || location.verdict}${approvedNote}`;
      fillList(reasonsList, describeReasons(location.reasons));
    }

    const publication = status?.publication;
    // Hidden for an older host (no field) and wherever MCP cannot read anything (null).
    publicationBox.hidden = publication !== "confirmed" && publication !== "pending";
    if (!publicationBox.hidden) {
      publicationBox.dataset.state = publication;
      publicationState.textContent = describePublication(status);
      const usable = status.state === "ready";
      publicationNote.textContent = publication === "pending" && !usable ? `${PUBLICATION_NOTE.pending}\n${PUBLICATION_NOTE.unusable}` : PUBLICATION_NOTE[publication];
      publicationConfirm.hidden = publication !== "pending" || !usable;
      publicationRevoke.hidden = publication !== "confirmed";
    }
    if (publication !== "pending") publicationReview.hidden = true;

    encryptionBox.hidden = !status?.managementEnabled;
    const pendingPath = typeof status?.creationPending === "string" ? status.creationPending : null;
    creationPendingBox.hidden = pendingPath === null || Boolean(onboarding);
    if (!creationPendingBox.hidden) {
      creationPendingText.textContent = `作成途中の暗号化Vaultがあります（リカバリーフレーズの確認前に中断されました）。フレーズは再表示できないため、破棄して作成し直してください。${pendingPath ? `\n保存先: ${pendingPath}` : ""}`;
    }
    recoveryOffer.hidden = !(status?.mode === "encrypted" && status.recoveryAvailable === true && status.state !== "ready");

    if (status?.defaultPath) {
      defaultPathText.textContent = status.defaultExists
        ? `既定の場所に既にファイルがあります: ${status.defaultPath}\n開く場合は「既存のVaultを開く」を使ってください。`
        : status.defaultPath;
    } else {
      defaultPathText.textContent = "";
    }
    updateButtons();
    onStatus(status);
  }

  function setMessage(text) {
    messageText.textContent = text;
  }

  /** Shows `value` as the status now; anything asked for before is older than it. */
  function showStatus(value) {
    statusRequest += 1;
    latestStatus = Promise.resolve(value);
    status = value;
    render();
  }

  function refresh() {
    const request = ++statusRequest;
    const answered = (async () => {
      try {
        const response = await vaultRequest("vault_status", {}, { send });
        if (request !== statusRequest) return latestStatus;
        status = response.status || null;
      } catch (error) {
        if (request !== statusRequest) return latestStatus;
        status = null;
        setMessage(error instanceof SearchError ? error.message : "Vaultの状態を取得できませんでした。");
      }
      render();
      return status;
    })();
    latestStatus = answered;
    return answered;
  }

  function showApproval(type, params, response) {
    pendingApproval = { type, params: { ...params, path: response.path, approveWarnings: true } };
    approvalTitle.textContent = `この場所をActive Vaultにすると、同期による破損やデータ消失の恐れがあります:\n${response.path}`;
    fillList(approvalReasons, describeReasons(response.location?.reasons));
    approvalBox.hidden = false;
    approveButton.focus();
  }

  function successMessage(type, response) {
    if (type === "vault_backup") return `バックアップを作成しました。\nバックアップ元: ${response.vaultPath}\n保存先: ${response.backupPath}`;
    if (type === "vault_publication_confirm") return "このVaultをMCPに公開しました。プライバシー設定を通過した記録だけが読まれます。";
    if (type === "vault_publication_revoke") return "このVaultのMCPへの公開を停止しました。MCPクライアントには内容を返しません。";
    if (type === "vault_create") return `新しいVaultを作成し、使用を開始しました:\n${response.path}`;
    if (type === "vault_create_encrypted_commit") return `暗号化Vaultを作成し、使用を開始しました:\n${response.path}\nMCPへの公開は未承認です。必要なら下で承認してください。`;
    if (type === "vault_create_encrypted_cancel") return "作成途中の暗号化Vaultを破棄しました。Active Vaultは変更していません。";
    if (type === "vault_recover") return `リカバリーフレーズで復旧し、このWindowsユーザーで開けるようにしました:\n${response.path}\nMCPへの公開は未承認です。`;
    return `Vaultを開きました:\n${response.path}`;
  }

  async function run(type, params = {}) {
    if (busy) return;
    // A backup (Issue #48) and a publication decision (Issue #51) name the vault shown: the host
    // refuses them if another vault is active.
    const isPublication = PUBLICATION_TYPES.has(type);
    const namesShown = type === "vault_backup" || isPublication;
    const source = namesShown ? backupSource() : null;
    if (namesShown && !source) {
      setMessage(isPublication ? "対象のVaultを確認できないため、MCPへの公開の設定は変更していません。表示を更新します。" : "バックアップするVaultを確認できないため、開始しませんでした。表示を更新します。");
      await refresh();
      return;
    }
    if (source) params = { ...params, expectedVaultToken: source.vaultToken };
    busy = true;
    approvalBox.hidden = true;
    publicationReview.hidden = true;
    pendingApproval = null;
    updateButtons();
    const usesDialog = DIALOG_TYPES.has(type) && !params.path && !params.directory && !params.useDefault;
    setMessage(usesDialog ? "Windowsのダイアログで選択してください。\n表示されない場合はタスクバーを確認してください。" : "処理中…");
    try {
      const response = await vaultRequest(type, params, { send });
      if (type === "vault_backup" && response.vaultToken !== source.vaultToken) {
        // The host only backs up the vault named; an answer naming another is reported, not trusted.
        setMessage(`バックアップ元が表示中のVaultと一致しません。作成されたファイルを確認してください。\nバックアップ元: ${response.vaultPath}\n保存先: ${response.backupPath}`);
        return;
      }
      if (type === "vault_create_encrypted") {
        // Nothing is active yet: the creation ends only after the phrase is confirmed.
        startOnboarding(response);
        return;
      }
      if (CREATION_END_TYPES.has(type)) clearOnboarding();
      if (type === "vault_recover") closeRecovery();
      setMessage(successMessage(type, response));
      if (response.status) showStatus(response.status);
      if (type !== "vault_backup" && !isPublication && type !== "vault_create_encrypted_cancel") onVaultChanged();
    } catch (error) {
      const response = error instanceof SearchError ? error.response : null;
      if (type === "vault_create_encrypted_commit" && (error.code === "vault_creation_not_pending" || error.code === "vault_creation_damaged")) clearOnboarding();
      if (error.code === "recovery_phrase_invalid") {
        setMessage(describePhraseProblem(response));
      } else if ((error.code === "key_unavailable" || error.code === "vault_locked" || error.code === "vault_decrypt_failed") && response?.recoveryAvailable === true && response.path) {
        setMessage(`${error.message}\nリカバリーフレーズで復旧できます。`);
        startRecovery(response.path);
      } else if (source && error.code === "vault_switched") {
        const unchanged = isPublication ? "MCPへの公開の設定は変更していません。" : "バックアップは作成していません。";
        setMessage(`表示していたVault（${source.vaultPath}）は、もうActiveではありません。${unchanged}\n表示を最新の状態に更新しました。確認してから、もう一度実行してください。`);
      } else if (isPublication && typeof error.code === "string" && error.code.startsWith("policy_")) {
        setMessage("プライバシー設定を読み取れない（未初期化・破損・使用中など）ため、MCPへの公開を承認していません。下の「プライバシー設定」を確認してください。");
      } else if (error.code === "location_needs_approval" && response?.path) {
        setMessage("");
        showApproval(type, params, response);
      } else {
        const reasons = describeReasons(response?.location?.reasons);
        const lines = [error instanceof SearchError ? error.message : "操作に失敗しました。"];
        if (response?.created) lines.push("Vaultファイルは作成されましたが、使用開始に失敗しました。「既存のVaultを開く」で開けます。");
        if (response?.path) lines.push(response.path);
        if (reasons.length) lines.push(...reasons.map((reason) => `・${reason}`));
        setMessage(lines.join("\n"));
      }
    } finally {
      busy = false;
      await refresh();
    }
  }

  function wordItem(word) {
    return Object.assign(document.createElement("li"), { textContent: word });
  }

  function showPhrase() {
    phraseList.replaceChildren(...onboarding.words.map(wordItem));
    phraseShow.hidden = false;
    phraseCheck.hidden = true;
    phraseInputs.replaceChildren();
    answerInputs = [];
  }

  function startOnboarding(response) {
    const words = Array.isArray(response?.recoveryPhrase) ? response.recoveryPhrase.filter((word) => typeof word === "string") : [];
    if (words.length !== 12) {
      setMessage("リカバリーフレーズを受け取れませんでした。作成途中のVaultを破棄して、作成し直してください。");
      return;
    }
    onboarding = { words, path: response.path };
    onboardingTitle.textContent = `${PHRASE_TITLE}\n保存先: ${response.path}`;
    showPhrase();
    onboardingBox.hidden = false;
    setMessage("");
    phraseWritten.focus();
  }

  /** Forgets the phrase: memory and DOM. */
  function clearOnboarding() {
    if (onboarding) onboarding.words.fill("");
    onboarding = null;
    phraseList.replaceChildren();
    phraseInputs.replaceChildren();
    answerInputs = [];
    onboardingBox.hidden = true;
  }

  function startRecovery(path) {
    recoveryTarget = path;
    recoveryTitle.textContent = `${RECOVERY_TITLE}\n対象: ${path}`;
    recoveryInput.value = "";
    recoveryBox.hidden = false;
    recoveryInput.focus();
  }

  function closeRecovery() {
    recoveryTarget = null;
    recoveryInput.value = "";
    recoveryBox.hidden = true;
  }

  phraseWritten.addEventListener("click", () => {
    if (!onboarding) return;
    // The words leave the page while the user answers from what they wrote down.
    phraseList.replaceChildren();
    phraseShow.hidden = true;
    answerInputs = pickPositions(3).map((position) => {
      const input = Object.assign(document.createElement("input"), { type: "text", spellcheck: false, autocomplete: "off" });
      input.setAttribute("autocapitalize", "off");
      input.setAttribute("aria-label", `${position}番目の単語`);
      input.dataset.position = String(position);
      return input;
    });
    phraseInputs.replaceChildren(
      ...answerInputs.map((input) => {
        const label = document.createElement("label");
        label.append(Object.assign(document.createElement("span"), { textContent: `${input.dataset.position}番目の単語 ` }), input);
        return label;
      }),
    );
    phraseCheck.hidden = false;
    answerInputs[0]?.focus();
  });
  phraseBack.addEventListener("click", () => {
    if (onboarding) showPhrase();
  });
  phraseConfirm.addEventListener("click", () => {
    if (!onboarding || busy) return;
    const answers = answerInputs.map((input) => ({ position: Number(input.dataset.position), word: input.value }));
    if (!checkAnswers(onboarding.words, answers)) {
      setMessage("入力した単語が、表示したフレーズと一致しません。書き留めた内容を確認してください（Vaultはまだ作成していません）。");
      return;
    }
    void run("vault_create_encrypted_commit", { recoveryPhrase: onboarding.words.join(" ") });
  });
  onboardingCancel.addEventListener("click", () => void run("vault_create_encrypted_cancel"));
  creationDiscard.addEventListener("click", () => void run("vault_create_encrypted_cancel"));
  recoveryStart.addEventListener("click", () => {
    if (status?.vaultPath) startRecovery(status.vaultPath);
  });
  recoverySubmit.addEventListener("click", () => {
    if (!recoveryTarget || busy) return;
    void run("vault_recover", { path: recoveryTarget, recoveryPhrase: recoveryInput.value });
  });
  recoveryCancel.addEventListener("click", () => {
    closeRecovery();
    setMessage("キャンセルしました。Vaultも鍵も変更していません。");
  });

  createDefault.addEventListener("click", () => void run(createType(), { useDefault: true }));
  createPick.addEventListener("click", () => void run(createType()));
  openPick.addEventListener("click", () => void run("vault_open"));
  backupPick.addEventListener("click", () => void run("vault_backup"));
  for (const button of document.querySelectorAll("[data-manual]")) {
    button.addEventListener("click", () => {
      const value = manualInput.value.trim();
      if (!value) {
        setMessage("パスを入力してください。");
        manualInput.focus();
        return;
      }
      const type = button.dataset.manual === "vault_create" ? createType() : button.dataset.manual;
      void run(type, type === "vault_backup" ? { directory: value } : { path: value });
    });
  }
  approveButton.addEventListener("click", () => {
    if (pendingApproval) void run(pendingApproval.type, pendingApproval.params);
  });
  approveCancel.addEventListener("click", () => {
    approvalBox.hidden = true;
    // A recovery waiting for this approval carries the phrase: forget it with the request.
    pendingApproval = null;
    setMessage("キャンセルしました。");
  });
  // Confirming takes two steps: what publishing means and which vault, then the approval itself.
  publicationConfirm.addEventListener("click", () => {
    const source = backupSource();
    if (busy || !source || status?.publication !== "pending") return;
    publicationReviewTitle.textContent = `${PUBLICATION_REVIEW}\n対象: ${source.vaultPath}`;
    publicationReview.hidden = false;
    publicationApprove.focus();
  });
  publicationApprove.addEventListener("click", () => void run("vault_publication_confirm"));
  publicationCancel.addEventListener("click", () => {
    publicationReview.hidden = true;
    setMessage("キャンセルしました。MCPへの公開は承認していません。");
  });
  publicationRevoke.addEventListener("click", () => void run("vault_publication_revoke"));

  return {
    refresh,
    open: reveal,
    get status() {
      return status;
    },
  };
}
