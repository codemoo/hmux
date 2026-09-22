import { createTextFactory } from "./dom.ts";

// Keep panels mounted: account requests and their disposal stay owned by the
// originating settings dialog even while another category is selected.
export function installSettingsNavigation(
  parent: HTMLElement,
  groups: { id: string; label: string; sections: HTMLElement[] }[],
  initialId?: string,
) {
  const text = createTextFactory(parent.ownerDocument);
  const nav = text("div", "", "settings-nav");
  nav.setAttribute("role", "tablist");
  nav.setAttribute("aria-label", "설정 항목");
  const content = text("div", "", "settings-content");
  const buttons: HTMLButtonElement[] = [];
  const panels: HTMLElement[] = [];
  function select(index: number, focus = false) {
    buttons.forEach((button, i) => {
      button.setAttribute("aria-selected", String(i === index));
      button.tabIndex = i === index ? 0 : -1;
      panels[i].hidden = i !== index;
    });
    content.scrollTop = 0;
    if (focus) buttons[index].focus({ preventScroll: false });
  }
  groups.forEach((group, index) => {
    const button = text("button", group.label);
    button.type = "button";
    button.id = `settings-tab-${group.id}`;
    button.setAttribute("role", "tab");
    button.setAttribute("aria-controls", `settings-panel-${group.id}`);
    button.onclick = () => select(index);
    button.onkeydown = (event) => {
      let next = index;
      if (event.key === "ArrowRight") next = (index + 1) % groups.length;
      else if (event.key === "ArrowLeft")
        next = (index + groups.length - 1) % groups.length;
      else if (event.key === "Home") next = 0;
      else if (event.key === "End") next = groups.length - 1;
      else return;
      event.preventDefault();
      select(next, true);
    };
    const panel = text("div", "", "settings-panel");
    panel.id = `settings-panel-${group.id}`;
    panel.setAttribute("role", "tabpanel");
    panel.setAttribute("aria-labelledby", button.id);
    panel.tabIndex = 0;
    panel.append(...group.sections);
    buttons.push(button);
    panels.push(panel);
    nav.append(button);
    content.append(panel);
  });
  parent.append(nav, content);
  select(
    Math.max(
      0,
      groups.findIndex((group) => group.id === initialId),
    ),
  );
}
