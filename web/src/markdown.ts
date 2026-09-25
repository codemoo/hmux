import { msg, bindText, bindAttribute } from "./i18n.ts";
import { Lexer, type MarkedToken, type Token } from "marked";
import { decodeHTMLStrict } from "entities";
import { createTextFactory } from "./dom.ts";

// Parse Markdown into tokens, then create only known DOM elements. Transcript
// HTML is displayed as text; no transcript string enters an HTML parser/sink.
export function renderMarkdown(
  root: HTMLElement,
  source: string,
  includeCode = true,
) {
  const doc = root.ownerDocument;
  const text = createTextFactory(doc);
  const render = (parent: HTMLElement, tokens: Token[]) => {
    for (const entry of tokens) {
      // The lexer is local and has no extensions or custom token types.
      const token = entry as MarkedToken;
      switch (token.type) {
        case "space":
        case "def":
          break;
        case "heading": {
          const tag = `h${token.depth}` as
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6";
          const node = text(tag);
          render(node, token.tokens);
          parent.append(node);
          break;
        }
        case "paragraph":
        case "blockquote":
        case "strong":
        case "em":
        case "del": {
          const node = text(token.type === "paragraph" ? "p" : token.type);
          render(node, token.tokens);
          parent.append(node);
          break;
        }
        case "text":
          if (token.tokens) render(parent, token.tokens);
          else
            parent.append(
              text(
                "span",
                token.escaped ? token.text : decodeHTMLStrict(token.text),
              ),
            );
          break;
        case "escape":
        case "html":
          parent.append(text("span", token.text));
          break;
        case "codespan":
          parent.append(text("code", token.text));
          break;
        case "code": {
          if (!includeCode) {
            parent.append(
              text(
                "p",
                msg("[code hidden]", "[코드 숨김]"),
                "markdown-code-hidden",
              ),
            );
            break;
          }
          const pre = text("pre");
          pre.tabIndex = 0;
          pre.append(text("code", token.text));
          parent.append(pre);
          break;
        }
        case "br":
        case "hr":
          parent.append(text(token.type));
          break;
        case "checkbox": {
          const check = text("input");
          check.type = "checkbox";
          check.checked = token.checked;
          check.disabled = true;
          bindAttribute(
            check,
            "aria-label",
            token.checked
              ? msg("Complete", "완료")
              : msg("Incomplete", "미완료"),
          );
          parent.append(check);
          break;
        }
        case "list": {
          const list = text(token.ordered ? "ol" : "ul");
          if (token.ordered) list.setAttribute("start", String(token.start));
          for (const item of token.items) {
            const li = text("li");
            if (item.task) li.className = "markdown-task";
            render(li, item.tokens);
            list.append(li);
          }
          parent.append(list);
          break;
        }
        case "table": {
          const wrap = text("div", "", "markdown-table");
          wrap.tabIndex = 0;
          wrap.setAttribute("role", "region");
          bindAttribute(wrap, "aria-label", msg("Table", "표"));
          const table = text("table");
          const head = text("thead");
          const body = text("tbody");
          const rows = [token.header, ...token.rows];
          rows.forEach((cells, index) => {
            const row = text("tr");
            cells.forEach((cell, column) => {
              const node = text(index === 0 ? "th" : "td");
              if (index === 0) node.setAttribute("scope", "col");
              const align = token.align[column];
              if (align) node.style.textAlign = align;
              render(node, cell.tokens);
              row.append(node);
            });
            (index === 0 ? head : body).append(row);
          });
          table.append(head, body);
          wrap.append(table);
          parent.append(wrap);
          break;
        }
        case "link":
        case "image": {
          let href: string | undefined;
          try {
            const url = new URL(
              token.type === "link" && token.autolink
                ? token.href
                : decodeHTMLStrict(token.href),
            );
            if (
              ["https:", "http:"].includes(url.protocol) &&
              !url.username &&
              !url.password
            )
              href = url.href;
          } catch {
            // Local paths and unsupported schemes remain ordinary text.
          }
          const node = text(href ? "a" : "span");
          if (href) {
            node.setAttribute("href", href);
            node.setAttribute("target", "_blank");
            node.setAttribute("rel", "noopener noreferrer");
          }
          if (token.title) node.title = decodeHTMLStrict(token.title);
          // Images are explicit links, never automatic third-party requests.
          if (token.type === "image")
            node.textContent = decodeHTMLStrict(token.text);
          if (!node.textContent) bindText(node, msg("Image", "이미지"));
          else render(node, token.tokens);
          parent.append(node);
          break;
        }
        default:
          parent.append(text("span", entry.raw));
      }
    }
  };
  root.replaceChildren();
  try {
    render(root, new Lexer({ gfm: true, breaks: true }).lex(source));
  } catch {
    // A malformed/deep transcript must not take down the conversation reader.
    root.textContent = source;
  }
}
