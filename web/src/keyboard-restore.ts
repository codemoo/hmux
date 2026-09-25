// Keep one deferred redraw for the view that observed keyboard dismissal.
// Never carry it across a tab switch, reconnect, dialog or background transition.
export function createKeyboardRestore<T extends { generation: number }>() {
  let wasVisible = false;
  let keyboard: { tab: T; generation: number } | undefined;
  let pending: typeof keyboard;
  const matches = (owner: typeof keyboard, tab: T | undefined) =>
    owner && owner.tab === tab && owner.generation === tab?.generation;
  return {
    observe(visible: boolean, tab: T | undefined) {
      if (visible) {
        if (!wasVisible)
          keyboard = tab ? { tab, generation: tab.generation } : undefined;
        else if (!matches(keyboard, tab)) keyboard = undefined;
        pending = undefined;
      } else {
        if (keyboard) pending = keyboard;
        keyboard = undefined;
        if (!matches(pending, tab)) pending = undefined;
      }
      wasVisible = visible;
    },
    take(tab: T | undefined): T | undefined {
      const restore = matches(pending, tab) ? tab : undefined;
      pending = undefined;
      return restore;
    },
  };
}
