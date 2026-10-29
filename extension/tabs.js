// Side panel tabs following the WAI-ARIA tabs pattern (automatic activation, roving tabindex).
// Panels stay in the DOM and are only hidden, so inputs, results and in-flight state survive a
// switch. Each panel's scroll offset is saved when it is hidden and restored when it is shown again.

/** Target index for a key press on the horizontal tab list, or null for other keys. */
export function nextTabIndex(key, current, count) {
  switch (key) {
    case "ArrowRight":
      return (current + 1) % count;
    case "ArrowLeft":
      return (current - 1 + count) % count;
    case "Home":
      return 0;
    case "End":
      return count - 1;
    default:
      return null;
  }
}

/**
 * entries: [{ id, tab, panel, scroller? }] where `scroller` (default: the panel) is the element
 * whose scroll offset belongs to that tab.
 */
export function createTabs(entries, { onChange = () => {} } = {}) {
  const savedScroll = new Map();
  let current = null;
  const scrollerOf = (entry) => entry.scroller ?? entry.panel;

  function select(id, { focus = false } = {}) {
    const next = entries.find((entry) => entry.id === id);
    if (!next) return;
    const previous = current;
    if (previous && previous !== next) savedScroll.set(previous.id, scrollerOf(previous).scrollTop);
    for (const entry of entries) {
      const selected = entry === next;
      entry.tab.setAttribute("aria-selected", String(selected));
      entry.tab.tabIndex = selected ? 0 : -1;
      entry.panel.hidden = !selected;
    }
    current = next;
    // Restore only after the panel is visible again; hidden elements cannot hold an offset.
    if (previous !== next && savedScroll.has(id)) scrollerOf(next).scrollTop = savedScroll.get(id);
    if (focus) next.tab.focus();
    if (previous !== next) onChange(id, previous?.id ?? null);
  }

  /** Scroll offset of a tab, including one that is currently hidden. */
  function getScroll(id) {
    const entry = entries.find((candidate) => candidate.id === id);
    if (!entry) return 0;
    return entry === current ? scrollerOf(entry).scrollTop : savedScroll.get(id) ?? 0;
  }

  /** Sets a tab's scroll offset now if visible, otherwise when it is shown next. */
  function setScroll(id, top) {
    const entry = entries.find((candidate) => candidate.id === id);
    if (!entry) return;
    if (entry === current) scrollerOf(entry).scrollTop = top;
    else savedScroll.set(id, top);
  }

  entries.forEach((entry, index) => {
    entry.tab.addEventListener("click", () => select(entry.id));
    entry.tab.addEventListener("keydown", (event) => {
      const target = nextTabIndex(event.key, index, entries.length);
      if (target === null) return;
      event.preventDefault();
      select(entries[target].id, { focus: true });
    });
  });

  return {
    select,
    getScroll,
    setScroll,
    get current() {
      return current?.id ?? null;
    },
  };
}
