import { createTextFactory, iconButton } from "./dom.ts";

export type Conversation = {
  status: string;
  messages: { role: string; text: string }[];
  truncated: boolean;
};

// Presentation only. The caller retains request, tab and epoch ownership.
export function renderConversation(
  reader: HTMLElement,
  data: Conversation,
  onReturn: () => void,
): boolean {
  const doc = reader.ownerDocument;
  const text = createTextFactory(doc);
  const button = (title: string, name: string, action: () => void) =>
    iconButton(doc, title, name, action);
  reader.replaceChildren();
  if (data.status !== "ready") {
    reader.append(
      text("h2", "대화를 확인할 수 없습니다"),
      text(
        "p",
        "이 tmux 세션의 활성 pane에 연결된 Codex 대화를 찾지 못했습니다.",
        "muted",
      ),
    );
    return false;
  }
  const controls = doc.createElement("div");
  controls.className = "reader-controls";
  const include = doc.createElement("input");
  include.type = "checkbox";
  include.checked = true;
  const questions = text("label", "내 메시지 ");
  questions.prepend(include);
  const code = doc.createElement("input");
  code.type = "checkbox";
  const codeLabel = text("label", "코드 포함 ");
  codeLabel.prepend(code);
  controls.append(
    text("h2", "대화"),
    questions,
    codeLabel,
    button("최신 메시지로", "arrow", () => {
      reader.scrollTop = reader.scrollHeight;
    }),
    button("터미널로 돌아가기", "close", onReturn),
  );
  const content = doc.createElement("div");
  const render = () => {
    content.replaceChildren();
    for (const m of data.messages) {
      if (m.role !== "assistant" && !include.checked) continue;
      const article = doc.createElement("article");
      article.dataset.role = m.role;
      article.append(text("small", m.role === "assistant" ? "CODEX" : "나"));
      const value =
        code.checked || m.role === "user"
          ? m.text
          : m.text.replace(/```[^\n]*\n[\s\S]*?```/g, "[코드 숨김]");
      article.append(text("div", value, "message-text"));
      content.append(article);
    }
  };
  include.onchange = code.onchange = render;
  reader.append(controls, content);
  render();
  if (data.truncated)
    reader.append(text("p", "최근 대화 일부만 표시합니다.", "muted"));
  return true;
}
