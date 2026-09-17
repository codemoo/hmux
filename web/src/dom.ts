import { icon } from "./icons.ts";

// Use the component's document and textContent for external text. The only HTML
// below comes from the fixed local icon table, never from a server response.
export function createTextFactory(doc: Document) {
  return <K extends keyof HTMLElementTagNameMap>(
    tag: K,
    value = "",
    className = "",
  ) => {
    const node = doc.createElement(tag);
    node.textContent = value;
    if (className) node.className = className;
    return node;
  };
}

export function iconButton(
  doc: Document,
  title: string,
  name: string,
  action: () => void,
) {
  const button = doc.createElement("button");
  button.type = "button";
  button.className = "icon-button";
  button.title = title;
  button.setAttribute("aria-label", title);
  button.innerHTML = icon(name);
  button.onclick = action;
  return button;
}
