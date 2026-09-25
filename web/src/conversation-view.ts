import { msg, type TextValue } from "./i18n.ts";
import { createTextFactory, iconButton } from "./dom.ts";
import { renderMarkdown } from "./markdown.ts";

export type Conversation = {
  status: string;
  provider?: string;
  messages: { role: string; text: string }[];
  truncated: boolean;
};

// The live catalog supplies the loading hint; the response owns final attribution.
export function renderConversationLoading(
  reader: HTMLElement,
  runtime?: string,
) {
  const text = createTextFactory(reader.ownerDocument);
  const provider =
    runtime === "codex" ? "Codex" : runtime === "claude" ? "Claude" : "";
  const card = text("div", "", "conversation-loading");
  const status = text("div", "", "conversation-loading-status");
  status.setAttribute("role", "status");
  const mark = text(
    "span",
    provider === "Claude" ? "✳" : provider === "Codex" ? "›_" : "···",
    "conversation-loading-mark",
  );
  mark.setAttribute("aria-hidden", "true");
  const copy = text("div", "", "conversation-loading-copy");
  copy.append(
    text(
      "span",
      provider || msg("Conversation", "대화"),
      "conversation-loading-provider",
    ),
    text("h2", msg("Loading conversation", "대화를 불러오는 중")),
    text(
      "p",
      msg("Preparing recent messages.", "최근 메시지를 정리하고 있어요."),
    ),
  );
  status.append(mark, copy);
  const preview = text("div", "", "conversation-loading-preview");
  preview.setAttribute("aria-hidden", "true");
  for (let i = 0; i < 5; i++) preview.append(text("span"));
  card.append(status, preview);
  reader.replaceChildren(card);
}

// Presentation only. The caller retains request, tab and epoch ownership.
export function renderConversation(
  reader: HTMLElement,
  data: Conversation,
  onReturn: () => void,
): boolean {
  const doc = reader.ownerDocument;
  const text = createTextFactory(doc);
  const button = (title: TextValue, name: string, action: () => void) =>
    iconButton(doc, title, name, action);
  reader.replaceChildren();
  if (data.status !== "ready") {
    reader.append(
      text("h2", msg("Conversation unavailable", "대화를 확인할 수 없습니다")),
      text(
        "p",
        msg(
          "No Codex or Claude conversation was found for the active pane of this tmux session.",
          "이 tmux 세션의 활성 pane에 연결된 Codex 또는 Claude 대화를 찾지 못했습니다.",
        ),
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
  const questions = text("label", msg("My messages ", "내 메시지 "));
  questions.prepend(include);
  const code = doc.createElement("input");
  code.type = "checkbox";
  const codeLabel = text("label", msg("Include code ", "코드 포함 "));
  codeLabel.prepend(code);
  controls.append(
    text("h2", msg("Conversation", "대화")),
    questions,
    codeLabel,
    button(msg("Latest message", "최신 메시지로"), "arrow", () => {
      reader.scrollTop = reader.scrollHeight;
    }),
    button(msg("Return to terminal", "터미널로 돌아가기"), "close", onReturn),
  );
  const content = doc.createElement("div");
  const render = () => {
    content.replaceChildren();
    for (const m of data.messages) {
      if (m.role !== "assistant" && !include.checked) continue;
      const article = doc.createElement("article");
      article.dataset.role = m.role;
      article.append(
        text(
          "small",
          m.role === "assistant"
            ? data.provider === "claude"
              ? "CLAUDE"
              : "CODEX"
            : msg("Me", "나"),
        ),
      );
      const message = text("div", "", "message-text");
      renderMarkdown(message, m.text, code.checked || m.role === "user");
      article.append(message);
      content.append(article);
    }
  };
  include.onchange = code.onchange = render;
  reader.append(controls, content);
  render();
  if (data.truncated)
    reader.append(
      text(
        "p",
        msg(
          "Only part of the recent conversation is shown.",
          "최근 대화 일부만 표시합니다.",
        ),
        "muted",
      ),
    );
  return true;
}
