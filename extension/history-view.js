// The parts of the full-page History that draw: the month calendar, the visit rows and the numbered pager.
// Only safe DOM calls (`createElement`, `textContent`, `setAttribute`): a title, a URL or a host name from the
// web is text, never markup. No state of its own beyond what is needed to update in place (focus survives).

import { WEEKDAYS, calendarWeeks, dayLabel, daysInMonth, moveDay } from "./visit-calendar.js";
import { isWebUrl } from "./search-client.js";
import { pageNumbers } from "./history-state.js";

export const NO_TITLE = "（タイトルなし）";

const make = (document, tag, className = "", text = undefined) => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
};
const setClass = (element, name, on) => {
  const names = element.className.split(" ").filter(Boolean).filter((n) => n !== name);
  if (on) names.push(name);
  element.className = names.join(" ");
};

/** `2026年11月17日（火）` */
export function longDate({ year, monthIndex, day }) {
  const weekday = WEEKDAYS[new Date(Date.UTC(year, monthIndex, day)).getUTCDay()];
  return `${year}年${monthIndex + 1}月${day}日（${weekday}）`;
}

/** `HH:MM:SS` in the person's time zone. */
export function localTime(iso) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (n) => String(n).padStart(2, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

// ---------------------------------------------------------------------------------------------------- calendar

/**
 * The month grid inside `table`. `show` builds it for a month, or only updates the marks of the month it already
 * shows (so a keyboard user's focus stays where it is while the counts arrive). `counts` is null while unknown:
 * no mark, and the label says it is loading.
 */
export function createCalendarView({ table, document, onSelect }) {
  let current = null; // { year, monthIndex, cells: Map<day, { button, dot }> }
  let focusDay = 1;

  function label(day, counts, today, selected) {
    return dayLabel(current.year, current.monthIndex, day, counts ? counts[day - 1] : null, { today: sameDay(today, day), selected: sameDay(selected, day) });
  }
  const sameDay = (date, day) => !!date && date.year === current.year && date.monthIndex === current.monthIndex && date.day === day;

  function paint({ counts, selected, today }) {
    for (const [day, cell] of current.cells) {
      const isSelected = sameDay(selected, day);
      const isToday = sameDay(today, day);
      const has = !!counts && counts[day - 1] > 0;
      cell.button.setAttribute("aria-label", label(day, counts, today, selected));
      cell.button.setAttribute("aria-pressed", isSelected ? "true" : "false");
      if (isToday) cell.button.setAttribute("aria-current", "date");
      else cell.button.removeAttribute("aria-current");
      cell.button.setAttribute("tabindex", day === focusDay ? "0" : "-1");
      setClass(cell.button, "is-selected", isSelected);
      setClass(cell.button, "is-today", isToday);
      setClass(cell.button, "has-visits", has);
      if (has && !cell.dot) {
        cell.dot = make(document, "span", "cal-dot");
        cell.dot.setAttribute("aria-hidden", "true");
        cell.button.append(cell.dot);
      } else if (!has && cell.dot) {
        cell.dot.remove();
        cell.dot = null;
      }
    }
  }

  function build(year, monthIndex) {
    const head = make(document, "thead");
    const headRow = make(document, "tr");
    for (const name of WEEKDAYS) {
      const th = make(document, "th", "", name);
      th.setAttribute("scope", "col");
      headRow.append(th);
    }
    head.append(headRow);
    const body = make(document, "tbody");
    const cells = new Map();
    for (const week of calendarWeeks(year, monthIndex)) {
      const row = make(document, "tr");
      for (const day of week) {
        const cell = make(document, "td");
        if (day !== null) {
          const button = make(document, "button", "cal-day", String(day));
          button.setAttribute("type", "button");
          button.dataset.day = String(day);
          button.addEventListener("click", () => onSelect({ year, monthIndex, day }));
          cell.append(button);
          cells.set(day, { button, dot: null });
        }
        row.append(cell);
      }
      body.append(row);
    }
    table.replaceChildren(head, body);
    current = { year, monthIndex, cells };
  }

  table.addEventListener("keydown", (event) => {
    if (!current || !event.target?.dataset?.day) return;
    const day = Number(event.target.dataset.day);
    const next = moveDay(current.year, current.monthIndex, day, event.key, () => true);
    if (next === null) return;
    event.preventDefault?.();
    focusDay = next;
    for (const [d, cell] of current.cells) cell.button.setAttribute("tabindex", d === next ? "0" : "-1");
    current.cells.get(next)?.button.focus();
  });

  return {
    /** `selected` and `today` are `{ year, monthIndex, day }` or null. */
    show({ year, monthIndex, counts, selected, today }) {
      if (!current || current.year !== year || current.monthIndex !== monthIndex) {
        build(year, monthIndex);
        const inMonth = (date) => date && date.year === year && date.monthIndex === monthIndex;
        focusDay = inMonth(selected) ? selected.day : inMonth(today) ? today.day : 1;
      }
      paint({ counts, selected, today });
    },
    get month() {
      return current && { year: current.year, monthIndex: current.monthIndex, days: daysInMonth(current.year, current.monthIndex) };
    },
    cell(day) {
      return current?.cells.get(day)?.button ?? null;
    },
  };
}

// ------------------------------------------------------------------------------------------------------- rows

/**
 * The visits of the page, one row each (a URL visited twice is two rows). Every row keeps its visit id
 * (`data-visit-id`) so that a later detail can be attached to it. A title opens the page in a new tab only when
 * the URL is a complete http(s) one; a cut URL is fetched in full first (`onOpenCut`), never opened as cut.
 */
export function renderRows({ list, document, items, onOpenCut, onToggle = null }) {
  const rows = items.map((item) => {
    const row = make(document, "li", "visit-row");
    row.dataset.visitId = item.id;
    const time = make(document, "time", "visit-time", localTime(item.visitedAt));
    time.setAttribute("datetime", item.visitedAt);
    const body = make(document, "div", "visit-body");
    const titleText = item.title === "" ? NO_TITLE : item.titleTruncated ? `${item.title}…` : item.title;
    let title;
    if (item.urlTruncated && isWebUrl(item.url)) {
      title = make(document, "button", "visit-link", titleText);
      title.setAttribute("type", "button");
      title.setAttribute("title", "完全なURLを取得して新しいタブで開きます");
      title.addEventListener("click", () => onOpenCut(item, title));
    } else if (!item.urlTruncated && isWebUrl(item.url)) {
      title = make(document, "a", "visit-link", titleText);
      title.href = item.url;
      title.setAttribute("href", item.url);
      title.setAttribute("target", "_blank");
      title.setAttribute("rel", "noopener noreferrer");
    } else {
      title = make(document, "span", "visit-link is-disabled", titleText);
      title.setAttribute("aria-disabled", "true");
    }
    const meta = make(document, "div", "visit-meta");
    meta.append(make(document, "span", "visit-host", item.host === "" ? "（ホスト名なし）" : item.host), make(document, "span", "visit-url", item.urlTruncated ? `${item.url}…` : item.url));
    body.append(title, meta);
    row.append(time, body);
    if (onToggle) {
      // The expander is its own button (never the title link), and the detail region it controls sits under the row.
      const regionId = `visit-detail-${item.id}`;
      const toggle = make(document, "button", "visit-toggle", "詳細を表示");
      toggle.setAttribute("type", "button");
      toggle.setAttribute("aria-expanded", "false");
      toggle.setAttribute("aria-controls", regionId);
      const region = make(document, "section", "visit-detail");
      region.setAttribute("id", regionId);
      region.setAttribute("aria-label", `${titleText}の詳細`);
      region.hidden = true;
      const ui = { id: item.id, row, toggle, region };
      toggle.addEventListener("click", () => onToggle(item, ui));
      body.append(toggle);
      row.append(region);
      row.ui = ui;
    }
    return row;
  });
  list.replaceChildren(...rows);
  return rows;
}

// ------------------------------------------------------------------------------------------------------ pager

/**
 * `« ‹ 1 … 6 7 [8] 9 10 … 20 › »`. Returns the button of the current page (to keep the focus on). `pages` 0: no pager.
 */
export function renderPager({ nav, document, page, pages, onPage }) {
  if (pages < 1) {
    nav.replaceChildren();
    nav.hidden = true;
    return null;
  }
  nav.hidden = false;
  const button = (text, label, target, { disabled = false, current = false } = {}) => {
    const element = make(document, "button", current ? "page-button is-current" : "page-button", text);
    element.setAttribute("type", "button");
    element.setAttribute("aria-label", label);
    if (current) element.setAttribute("aria-current", "page");
    if (disabled) element.disabled = true;
    else if (!current) element.addEventListener("click", () => onPage(target, element));
    return element;
  };
  const parts = [button("«", "最初のページ", 1, { disabled: page <= 1 }), button("‹", "前のページ", page - 1, { disabled: page <= 1 })];
  let currentButton = null;
  for (const number of pageNumbers(page, pages)) {
    if (number === null) {
      const gap = make(document, "span", "page-gap", "…");
      gap.setAttribute("aria-hidden", "true");
      parts.push(gap);
    } else {
      const isCurrent = number === page;
      const element = button(String(number), isCurrent ? `${number}ページ（現在）` : `${number}ページ`, number, { current: isCurrent });
      if (isCurrent) currentButton = element;
      parts.push(element);
    }
  }
  parts.push(button("›", "次のページ", page + 1, { disabled: page >= pages }), button("»", "最後のページ", pages, { disabled: page >= pages }));
  nav.replaceChildren(...parts);
  return currentButton;
}

// ---------------------------------------------------------------------------------------------- detail

export const SOURCE_CENSORED = "[CENSORED]";

/** The row's detail region: closed, loading, failed (with a retry) or showing a visit. */
export function setOpen(ui, open) {
  ui.toggle.setAttribute("aria-expanded", open ? "true" : "false");
  ui.toggle.textContent = open ? "詳細を閉じる" : "詳細を表示";
  ui.region.hidden = !open;
  if (!open) ui.region.replaceChildren();
  setClass(ui.row, "is-expanded", open);
}

export function showDetailLoading(ui, document) {
  const text = make(document, "p", "detail-note", "詳細を読み込み中…");
  text.setAttribute("role", "status");
  ui.region.replaceChildren(text);
}

export function showDetailError(ui, document, message, onRetry) {
  const text = make(document, "p", "detail-error", message);
  text.setAttribute("role", "alert");
  const parts = [text];
  if (onRetry) {
    const retry = make(document, "button", "link-button", "再読み込み");
    retry.setAttribute("type", "button");
    retry.addEventListener("click", onRetry);
    parts.push(retry);
  }
  ui.region.replaceChildren(...parts);
}

/** A web URL is a link that opens in a new tab; a cut one, or anything else, is only text (never opened as given). */
function urlNode(document, url, { cut = false, label = url, className = "detail-url" } = {}) {
  if (!cut && isWebUrl(url)) {
    const link = make(document, "a", className, label);
    link.href = url;
    link.setAttribute("href", url);
    link.setAttribute("target", "_blank");
    link.setAttribute("rel", "noopener noreferrer");
    return link;
  }
  return make(document, "span", className, cut ? `${label}…` : label);
}

const card = (document, heading, extraClass = "") => {
  const section = make(document, "div", extraClass ? `detail-card ${extraClass}` : "detail-card");
  section.append(make(document, "h4", "detail-heading", heading));
  return section;
};
const field = (document, name, value) => {
  const row = make(document, "div", "detail-field");
  row.append(make(document, "span", "detail-name", name));
  const content = make(document, "span", "detail-value");
  content.append(value);
  row.append(content);
  return row;
};
const when = (iso) => {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日 ${localTime(iso)}`;
};

function visitLine(document, item, extra = "") {
  const line = make(document, "div", "detail-visit");
  const titleText = item.title === "" ? NO_TITLE : item.titleTruncated ? `${item.title}…` : item.title;
  line.append(make(document, "span", "detail-visit-title", titleText));
  line.append(urlNode(document, item.url, { cut: item.urlTruncated, className: "detail-url" }));
  const meta = [item.host, when(item.visitedAt), extra].filter((part) => part !== "").join(" ・ ");
  line.append(make(document, "span", "detail-meta", meta));
  return line;
}

/**
 * The detail of one visit, from `historyVisit`'s model. The three states of the source are told apart and never
 * stand in for each other: `present` shows the visit, `[CENSORED]` shows nothing of it, `unknown` is not "direct access".
 */
export function showDetail(ui, document, detail) {
  const parts = [];
  const visit = card(document, "訪問情報", "detail-card-visit");
  visit.append(field(document, "訪問日時", make(document, "span", "", when(detail.visit.visitedAt))));
  visit.append(field(document, "タイトル", make(document, "span", "", detail.visit.title === "" ? NO_TITLE : detail.visit.title)));
  visit.append(field(document, "URL", urlNode(document, detail.visit.url)));
  if (detail.navigation && (detail.navigation.kind || detail.navigation.transitionType)) {
    visit.append(field(document, "記録された遷移の種類", make(document, "span", "", [detail.navigation.kind, detail.navigation.transitionType].filter(Boolean).join(" / "))));
  }

  const source = card(document, "遷移元", "detail-card-source");
  if (detail.source.state === "present") {
    source.append(visitLine(document, detail.source.visit));
  } else if (detail.source.state === "censored") {
    source.append(make(document, "strong", "detail-censored", SOURCE_CENSORED));
    source.append(make(document, "p", "detail-note", "遷移元の閲覧履歴は削除されています"));
  } else {
    source.append(make(document, "p", "detail-unknown", "遷移元：不明"));
    source.append(make(document, "p", "detail-note", "遷移元は記録されていないか、確定できませんでした。"));
  }
  parts.push(source, visit);

  const targets = card(document, "遷移先", "detail-card-targets");
  const shownCount = detail.targets.length;
  const summary = detail.targetsTruncated ? `遷移先 ${detail.targetCount.toLocaleString("ja-JP")}件（先頭${shownCount.toLocaleString("ja-JP")}件を表示）` : `遷移先 ${detail.targetCount.toLocaleString("ja-JP")}件`;
  if (detail.targetCount === 0) {
    targets.append(make(document, "p", "detail-note", "記録された遷移先はありません"));
  } else {
    targets.append(make(document, "p", "detail-note", summary));
    if (shownCount === 0) targets.append(make(document, "p", "detail-note", "表示できる遷移先の訪問がありません。"));
    const list = make(document, "ul", "detail-targets");
    for (const target of detail.targets) {
      const item = make(document, "li", "detail-target");
      item.dataset.visitId = target.id;
      item.append(visitLine(document, target, [target.kind, target.transitionType].filter(Boolean).join(" / ")));
      list.append(item);
    }
    if (shownCount > 0) targets.append(list);
  }
  parts.push(targets);

  if (detail.pageMetadata) {
    const meta = card(document, "取得したページ情報", "detail-card-meta");
    const note = detail.pageMetadata.capturedAt ? `この訪問に紐づく取得（${when(detail.pageMetadata.capturedAt)}）。訪問時点の内容とは限りません。` : "この訪問に紐づく取得。訪問時点の内容とは限りません。";
    meta.append(make(document, "p", "detail-note", note));
    const labels = [["title", "タイトル"], ["description", "説明"], ["ogTitle", "OGタイトル"], ["ogDescription", "OG説明"], ["ogSiteName", "サイト名"], ["ogImageUrl", "画像URL"], ["faviconUrl", "ファビコンURL"]];
    for (const [key, name] of labels) {
      const value = detail.pageMetadata[key];
      if (value) meta.append(field(document, name, make(document, "span", "", value)));
    }
    parts.push(meta);
  }
  ui.region.replaceChildren(...parts);
}
