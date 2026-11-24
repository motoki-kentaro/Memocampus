// Context menu items of the service worker (image and page saving). Chrome keeps created items across
// extension updates and browser restarts, so registration runs on both onInstalled and onStartup:
// an item that does not exist yet is created; an existing one only gets its title set to the current
// UI language. The id and contexts of an existing item are never changed.
//
// Chrome reports both failures only as text in runtime.lastError, with no error code, so the text is
// never parsed to tell a duplicate id from anything else. Instead the outcome is checked: when create
// fails, update must succeed (the item exists and now has the current title). Only if both fail is the
// item missing or stale, and that is reported with both of Chrome's messages. A create that failed for
// another reason but whose update then succeeded is indistinguishable from a duplicate id, and is
// treated as one: the item is there with the current title, which is what registration is for.

/** Both Chrome calls failed: the menu item may be missing or keep a title in another language. */
export class MenuRegistrationError extends Error {
  constructor(id, createError, updateError) {
    super(`Context menu item "${id}" was not registered (create: ${createError}; update: ${updateError})`);
    this.name = "MenuRegistrationError";
    this.id = id;
    this.createError = createError;
    this.updateError = updateError;
  }
}

/** runtime.lastError's text, read inside the callback that Chrome set it for (null when none). */
const lastErrorText = (runtime) => {
  const error = runtime?.lastError;
  return error ? String(error.message ?? error) : null;
};

/** A call that may also throw synchronously (Chrome validates arguments before calling back). */
function call(method, args, runtime) {
  return new Promise((resolve) => {
    try {
      method(...args, () => resolve(lastErrorText(runtime)));
    } catch (error) {
      resolve(String(error?.message ?? error));
    }
  });
}

/**
 * Creates `item`, or re-titles the existing item with its id. Resolves to "created" or "retitled";
 * rejects with MenuRegistrationError when neither worked.
 */
export async function registerMenuItem(contextMenus, runtime, item) {
  const createError = await call(contextMenus.create.bind(contextMenus), [item], runtime);
  if (createError === null) return "created";
  const updateError = await call(contextMenus.update.bind(contextMenus), [item.id, { title: item.title }], runtime);
  if (updateError === null) return "retitled";
  throw new MenuRegistrationError(item.id, createError, updateError);
}
