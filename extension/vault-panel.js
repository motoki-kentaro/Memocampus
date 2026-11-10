// Vault settings section of the side panel. All file access happens in the native host; this
// only sends requests and renders the host's answers.

import { SearchError } from "./search-client.js";
import {
  BACKUP_FORMAT_NOTE,
  ENCRYPTION_NOTE,
  LOCATION_NOTE,
  BACKUP_SPELL_NOTE,
  PASSWORD_NOTE,
  PUBLICATION_NOTE,
  PUBLICATION_REVIEW,
  RECOVERY_TITLE,
  SPELL_CAUTION,
  SPELL_CHECK_TITLE,
  SPELL_LEAD,
  SPELL_NOTE,
  SPELL_TITLE,
  VERDICT_TEXT,
  backupFormat,
  checkBackupPassword,
  checkSpell,
  describeSpellProblem,
  describePolicyCopy,
  describePublication,
  describeReasons,
  describeRestorePending,
  describeRestoreSource,
  describeRestored,
  describeStatus,
  formatCounts,
  normalizeSpellInput,
  spellMatches,
  vaultRequest,
} from "./vault-client.js";

const PUBLICATION_TYPES = new Set(["vault_publication_confirm", "vault_publication_revoke"]);
/** Requests that may open a Windows dialog when no path is given. */
const DIALOG_TYPES = new Set(["vault_create", "vault_open", "vault_backup", "vault_create_encrypted", "vault_recover", "vault_restore_inspect", "vault_restore"]);
/** Issue #51 Slice C: the steps that end a restore. */
const RESTORE_END_TYPES = new Set(["vault_restore_activate", "vault_restore_discard"]);
/** Creation of an encrypted vault ends (either way) with these. */
const CREATION_END_TYPES = new Set(["vault_create_encrypted_commit", "vault_create_encrypted_cancel"]);

const $ = (selector) => document.querySelector(selector);

/** Puts `text` on the clipboard, only ever from the user's own click on "コピー". */
function writeClipboard(text) {
  return navigator.clipboard.writeText(text);
}

/**
 * `reveal` brings the settings view into sight (the side panel's settings tab); `onStatus` receives
 * every rendered status (null when the host could not be asked). `copyText` writes to the clipboard
 * (replaceable in tests).
 */
export function createVaultPanel({ send, onVaultChanged, reveal = () => {}, onStatus = () => {}, copyText = writeClipboard }) {
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
  // Issue #51: encryption choice, the Recovery Spell (ふっかつのじゅもん) shown once, and recovery.
  const encryptionBox = $("#vault-encryption");
  const encryptionNote = $("#vault-encryption-note");
  const encryptedChoice = $("#vault-encryption-encrypted");
  const plainChoice = $("#vault-encryption-plain");
  const onboardingBox = $("#vault-onboarding");
  const onboardingTitle = $("#vault-onboarding-title");
  const onboardingPath = $("#vault-onboarding-path");
  const spellShow = $("#vault-spell-show");
  const spellText = $("#vault-spell");
  const spellCopy = $("#vault-spell-copy");
  const spellWritten = $("#vault-spell-written");
  const spellCheck = $("#vault-spell-check");
  const spellConfirmInput = $("#vault-spell-confirm-input");
  const spellConfirm = $("#vault-spell-confirm");
  const spellBack = $("#vault-spell-back");
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
  // Issue #51 Slice C: backup protection and restore. Passwords and spells live only in these
  // inputs and in the one request that carries them; they are cleared when the step ends.
  const backupNormal = $("#vault-backup-normal");
  const backupPassword = $("#vault-backup-password");
  const backupNote = $("#vault-backup-note");
  const backupPasswordFields = $("#vault-backup-password-fields");
  const backupPasswordInput = $("#vault-backup-password-input");
  const backupPasswordConfirm = $("#vault-backup-password-confirm");
  // Review finding C-03: an encrypted vault's backup (B, D) needs the spell that recovers it.
  const backupSpellRow = $("#vault-backup-spell-row");
  const backupSpell = $("#vault-backup-spell");
  $("#vault-backup-spell-note").textContent = BACKUP_SPELL_NOTE;
  const restorePick = $("#vault-restore-pick");
  const restoreBox = $("#vault-restore");
  const restoreTitle = $("#vault-restore-title");
  const restorePasswordRow = $("#vault-restore-password-row");
  const restorePassword = $("#vault-restore-password");
  const restoreSpellRow = $("#vault-restore-spell-row");
  const restoreSpell = $("#vault-restore-spell");
  const restoreSubmit = $("#vault-restore-submit");
  const restoreCancel = $("#vault-restore-cancel");
  const restoredBox = $("#vault-restored");
  const restoredTitle = $("#vault-restored-title");
  const restoredPolicy = $("#vault-restored-policy");
  const restoredPolicyDetails = $("#vault-restored-policy-details");
  const restoredPolicyJson = $("#vault-restored-policy-json");
  const restoreActivate = $("#vault-restore-activate");
  const restoreDiscard = $("#vault-restore-discard");
  $("#vault-backup-password-note").textContent = PASSWORD_NOTE;
  encryptionNote.textContent = ENCRYPTION_NOTE;
  onboardingTitle.textContent = SPELL_TITLE;
  $("#vault-spell-lead").textContent = SPELL_LEAD;
  $("#vault-spell-caution").textContent = SPELL_CAUTION;
  $("#vault-spell-note").textContent = SPELL_NOTE;
  $("#vault-spell-check-title").textContent = SPELL_CHECK_TITLE;

  const actionButtons = [createDefault, createPick, openPick, backupPick, restorePick, publicationConfirm, publicationRevoke, publicationApprove, ...document.querySelectorAll("[data-manual]")];
  const creationButtons = [createDefault, createPick, openPick, document.querySelector('[data-manual="vault_create"]'), document.querySelector('[data-manual="vault_open"]')];
  // The spell lives only here, in this page's memory, until the creation ends; it is never stored
  // (no chrome.storage, no queue, no log) and no request asks the host for it again.
  let onboarding = null;
  let recoveryTarget = null;
  // The backup chosen for a restore (`vault_restore_inspect` answer), and the restored vault waiting
  // for the user's decision (`vault_restore` answer, or `status.restorePending`).
  let restoreSource = null;
  let restored = null;
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
    // No other vault is created or opened while a creation waits for its spell confirmation.
    const creationOpen = Boolean(onboarding) || Boolean(status?.creationPending);
    if (creationOpen) for (const button of creationButtons) button.disabled = true;
    const canEncrypt = status?.encryptionAvailable === true;
    encryptedChoice.disabled = busy || !canEncrypt;
    plainChoice.disabled = busy;
    if (!canEncrypt && status) plainChoice.checked = true;
    for (const button of [spellCopy, spellWritten, spellConfirm, spellBack, onboardingCancel, creationDiscard, recoveryStart, recoverySubmit, recoveryCancel, restoreSubmit, restoreCancel, restoreDiscard]) button.disabled = busy;
    // One restore at a time: none starts while a restored vault waits for a decision.
    if (status?.restorePending || restored) restorePick.disabled = true;
    restoreActivate.disabled = busy || !restored?.vaultToken;
    backupNormal.disabled = busy;
    backupPassword.disabled = busy;
  }

  function protection() {
    return backupPassword.checked ? "password" : "normal";
  }

  function clearBackupPassword() {
    backupPasswordInput.value = "";
    backupPasswordConfirm.value = "";
  }

  /** Forgets every backup secret typed (password and spell). */
  function clearBackupSecrets() {
    clearBackupPassword();
    backupSpell.value = "";
  }

  function encryptedShown() {
    return status?.mode === "encrypted";
  }

  /** The parameters of a backup as chosen, or null (with a message) when the password is not usable. */
  function backupParams(extra = {}) {
    const params = { ...extra, protection: protection() };
    if (params.protection === "password") {
      const problem = checkBackupPassword(backupPasswordInput.value, backupPasswordConfirm.value);
      if (problem) {
        setMessage(`${problem}\nバックアップは作成していません。`);
        backupPasswordInput.focus();
        return null;
      }
      params.password = backupPasswordInput.value;
    }
    if (encryptedShown()) {
      const problem = checkSpell(backupSpell.value);
      if (problem) {
        setMessage(`${problem}\nバックアップは作成していません。`);
        backupSpell.focus();
        return null;
      }
      params.recoverySpell = normalizeSpellInput(backupSpell.value);
    }
    return params;
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
    backupNote.textContent = BACKUP_FORMAT_NOTE[backupFormat(status?.mode, protection())];
    backupPasswordFields.hidden = protection() !== "password";
    backupSpellRow.hidden = !encryptedShown();
    if (backupSpellRow.hidden) backupSpell.value = "";
    // A restore recorded by the host, but not shown in this page (e.g. the panel was reopened).
    const pending = status?.restorePending ?? null;
    if (!pending && restored?.fromStatus) closeRestored();
    if (pending && !restored) showRestorePending(pending);
    const pendingPath = typeof status?.creationPending === "string" ? status.creationPending : null;
    creationPendingBox.hidden = pendingPath === null || Boolean(onboarding);
    if (!creationPendingBox.hidden) {
      creationPendingText.textContent = `作成途中の暗号化Vaultがあります（ふっかつのじゅもんの確認前に中断されました）。呪文は再表示できないため、破棄して作成し直してください。${pendingPath ? `\n保存先: ${pendingPath}` : ""}`;
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
    if (type === "vault_backup") {
      const format = typeof response.format === "string" ? `（形式${response.format}）` : "";
      const policy = response.policyCopy === true ? "\nプライバシー設定のコピーを含めました。" : "";
      return `バックアップを作成しました${format}。\nバックアップ元: ${response.vaultPath}\n保存先: ${response.backupPath}${policy}`;
    }
    if (type === "vault_restore_activate") return `復元したVaultを使い始めました:\n${response.path}\nMCPへの公開は未承認です。必要なら下で承認してください。`;
    if (type === "vault_restore_discard") return "復元したVault（または中断された復元）を破棄しました。Active Vaultは変更していません。";
    if (type === "vault_publication_confirm") return "このVaultをMCPに公開しました。プライバシー設定を通過した記録だけが読まれます。";
    if (type === "vault_publication_revoke") return "このVaultのMCPへの公開を停止しました。MCPクライアントには内容を返しません。";
    if (type === "vault_create") return `新しいVaultを作成し、使用を開始しました:\n${response.path}`;
    if (type === "vault_create_encrypted_commit") return `暗号化Vaultを作成し、使用を開始しました:\n${response.path}\nMCPへの公開は未承認です。必要なら下で承認してください。`;
    if (type === "vault_create_encrypted_cancel") return "作成途中の暗号化Vaultを破棄しました。Active Vaultは変更していません。";
    if (type === "vault_recover") return `ふっかつのじゅもんで復旧し、このWindowsユーザーで開けるようにしました:\n${response.path}\nMCPへの公開は未承認です。`;
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
        // Nothing is active yet: the creation ends only after the whole spell is entered again.
        startOnboarding(response);
        return;
      }
      if (type === "vault_restore_inspect") {
        startRestore(response);
        return;
      }
      if (type === "vault_restore") {
        // Restored, but not active: the user decides after seeing what was restored.
        closeRestore();
        showRestored(response);
        setMessage("復元が完了しました。内容を確認して、使い始めるか破棄してください。");
        return;
      }
      if (RESTORE_END_TYPES.has(type)) closeRestored();
      if (CREATION_END_TYPES.has(type)) clearOnboarding();
      if (type === "vault_recover") closeRecovery();
      setMessage(successMessage(type, response));
      if (response.status) showStatus(response.status);
      if (type !== "vault_backup" && !isPublication && type !== "vault_create_encrypted_cancel" && type !== "vault_restore_discard") onVaultChanged();
    } catch (error) {
      const response = error instanceof SearchError ? error.response : null;
      if (type === "vault_create_encrypted_commit" && (error.code === "vault_creation_not_pending" || error.code === "vault_creation_damaged")) clearOnboarding();
      if (type === "vault_restore_activate" && error.code === "vault_restore_not_pending") closeRestored();
      if (error.code === "recovery_spell_invalid") {
        setMessage(describeSpellProblem(response));
      } else if ((error.code === "key_unavailable" || error.code === "vault_locked" || error.code === "vault_decrypt_failed") && response?.recoveryAvailable === true && response.path) {
        setMessage(`${error.message}\nふっかつのじゅもんで復旧できます。`);
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
      // A backup password and spell are used for one request only.
      if (type === "vault_backup") clearBackupSecrets();
      busy = false;
      await refresh();
    }
  }

  function startRestore(info) {
    restoreSource = info;
    restoreTitle.textContent = describeRestoreSource(info);
    restorePasswordRow.hidden = info.needsPassword !== true;
    restoreSpellRow.hidden = info.needsSpell !== true;
    restorePassword.value = "";
    restoreSpell.value = "";
    restoreBox.hidden = false;
    setMessage("");
    (info.needsPassword ? restorePassword : info.needsSpell ? restoreSpell : restoreSubmit).focus();
  }

  /** Forgets the backup chosen and the secrets typed for it. */
  function closeRestore() {
    restoreSource = null;
    restorePassword.value = "";
    restoreSpell.value = "";
    restoreBox.hidden = true;
  }

  function showRestored(response) {
    restored = { path: response.path, vaultToken: response.vaultToken, fromStatus: false };
    restoredTitle.textContent = describeRestored(response);
    restoredPolicy.textContent = describePolicyCopy(response.policyCopy);
    const copy = response.policyCopy;
    restoredPolicyDetails.hidden = copy?.state !== "valid";
    restoredPolicyJson.textContent = copy?.state === "valid" ? JSON.stringify({ policy: copy.policy, approvedDomains: copy.approvedDomains }, null, 2) : "";
    restoreActivate.hidden = false;
    restoredBox.hidden = false;
    restoreActivate.focus();
  }

  function showRestorePending(pending) {
    const usable = pending.phase === "restored" || pending.phase === "verified";
    restored = { path: pending.path, vaultToken: usable ? pending.vaultToken : null, fromStatus: true };
    restoredTitle.textContent = describeRestorePending(pending);
    restoredPolicy.textContent = "";
    restoredPolicyDetails.hidden = true;
    restoredPolicyJson.textContent = "";
    restoreActivate.hidden = !usable;
    restoredBox.hidden = false;
  }

  function closeRestored() {
    restored = null;
    restoredPolicyJson.textContent = "";
    restoredBox.hidden = true;
  }

  function showSpell() {
    spellText.textContent = onboarding.spell;
    spellShow.hidden = false;
    spellCheck.hidden = true;
    spellConfirmInput.value = "";
  }

  function startOnboarding(response) {
    const spell = typeof response?.recoverySpell === "string" ? response.recoverySpell : "";
    if (checkSpell(spell) !== null || spell !== normalizeSpellInput(spell)) {
      setMessage("ふっかつのじゅもんを受け取れませんでした。作成途中のVaultを破棄して、作成し直してください。");
      return;
    }
    onboarding = { spell, path: response.path };
    onboardingPath.textContent = `保存先: ${response.path}`;
    showSpell();
    onboardingBox.hidden = false;
    setMessage("");
    spellWritten.focus();
  }

  /** Forgets the spell: memory, DOM and the confirmation input. */
  function clearOnboarding() {
    onboarding = null;
    spellText.textContent = "";
    spellConfirmInput.value = "";
    onboardingPath.textContent = "";
    spellShow.hidden = false;
    spellCheck.hidden = true;
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

  spellCopy.addEventListener("click", async () => {
    if (!onboarding) return;
    try {
      await copyText(onboarding.spell);
      setMessage("コピーしました。安全な場所に保管したら、クリップボード（Windowsのクリップボード履歴を含む）から消去してください。");
    } catch {
      setMessage("コピーできませんでした。表示された呪文を書き写してください。");
    }
  });
  spellWritten.addEventListener("click", () => {
    if (!onboarding) return;
    // The spell leaves the page while the user enters it again from where they kept it.
    spellText.textContent = "";
    spellShow.hidden = true;
    spellConfirmInput.value = "";
    spellCheck.hidden = false;
    spellConfirmInput.focus();
  });
  spellBack.addEventListener("click", () => {
    if (onboarding) showSpell();
  });
  spellConfirm.addEventListener("click", () => {
    if (!onboarding || busy) return;
    const typed = spellConfirmInput.value;
    if (!spellMatches(onboarding.spell, typed)) {
      const problem = checkSpell(typed);
      setMessage(`${problem ?? "入力した呪文が、表示したふっかつのじゅもんと一致しません。"}\n記録した内容を確認して、もう一度入力してください（Vaultはまだ作成していません）。`);
      spellConfirmInput.focus();
      return;
    }
    void run("vault_create_encrypted_commit", { recoverySpell: onboarding.spell });
  });
  onboardingCancel.addEventListener("click", () => void run("vault_create_encrypted_cancel"));
  creationDiscard.addEventListener("click", () => void run("vault_create_encrypted_cancel"));
  recoveryStart.addEventListener("click", () => {
    if (status?.vaultPath) startRecovery(status.vaultPath);
  });
  recoverySubmit.addEventListener("click", () => {
    if (!recoveryTarget || busy) return;
    const problem = checkSpell(recoveryInput.value);
    if (problem) {
      setMessage(`${problem}\nVaultも鍵も変更していません。`);
      recoveryInput.focus();
      return;
    }
    void run("vault_recover", { path: recoveryTarget, recoverySpell: normalizeSpellInput(recoveryInput.value) });
  });
  recoveryCancel.addEventListener("click", () => {
    closeRecovery();
    setMessage("キャンセルしました。Vaultも鍵も変更していません。");
  });

  createDefault.addEventListener("click", () => void run(createType(), { useDefault: true }));
  createPick.addEventListener("click", () => void run(createType()));
  openPick.addEventListener("click", () => void run("vault_open"));
  backupPick.addEventListener("click", () => {
    const params = backupParams();
    if (params) void run("vault_backup", params);
  });
  for (const choice of [backupNormal, backupPassword]) {
    choice.addEventListener("change", () => {
      if (!backupPassword.checked) clearBackupPassword();
      render();
    });
  }
  restorePick.addEventListener("click", () => void run("vault_restore_inspect"));
  restoreSubmit.addEventListener("click", () => {
    if (!restoreSource || busy) return;
    const params = { backupPath: restoreSource.backupPath };
    if (restoreSource.needsPassword) {
      if (!restorePassword.value) {
        setMessage("バックアップのパスワードを入力してください。");
        restorePassword.focus();
        return;
      }
      params.password = restorePassword.value;
    }
    if (restoreSource.needsSpell) {
      const problem = checkSpell(restoreSpell.value);
      if (problem) {
        setMessage(`${problem}\n何も復元していません。`);
        restoreSpell.focus();
        return;
      }
      params.recoverySpell = normalizeSpellInput(restoreSpell.value);
    }
    void run("vault_restore", params);
  });
  restoreCancel.addEventListener("click", () => {
    closeRestore();
    setMessage("キャンセルしました。何も復元していません。");
  });
  restoreActivate.addEventListener("click", () => {
    if (restored?.vaultToken) void run("vault_restore_activate", { expectedVaultToken: restored.vaultToken });
  });
  restoreDiscard.addEventListener("click", () => void run("vault_restore_discard"));
  for (const button of document.querySelectorAll("[data-manual]")) {
    button.addEventListener("click", () => {
      const value = manualInput.value.trim();
      if (!value) {
        setMessage("パスを入力してください。");
        manualInput.focus();
        return;
      }
      const type = button.dataset.manual === "vault_create" ? createType() : button.dataset.manual;
      if (type === "vault_backup") {
        const params = backupParams({ directory: value });
        if (params) void run(type, params);
        return;
      }
      void run(type, { path: value });
    });
  }
  approveButton.addEventListener("click", () => {
    if (pendingApproval) void run(pendingApproval.type, pendingApproval.params);
  });
  approveCancel.addEventListener("click", () => {
    approvalBox.hidden = true;
    // A recovery waiting for this approval carries the spell: forget it with the request.
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
