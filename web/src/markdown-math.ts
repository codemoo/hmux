import type { TokenizerExtension } from "marked";

export type MathToken = {
  type: "math_inline" | "math_block";
  raw: string;
  text: string;
  display: boolean;
};
export const MAX_EQUATION_LENGTH = 8192;
export const MAX_MESSAGE_EQUATIONS = 64;
export const MAX_MESSAGE_MATH_LENGTH = 32768;

function escaped(source: string, index: number): boolean {
  let slashes = 0;
  while (index > 0 && source[--index] === "\\") slashes++;
  return slashes % 2 === 1;
}
function equation(source: string): Omit<MathToken, "type"> | undefined {
  const open = source.startsWith("$$")
    ? "$$"
    : source.startsWith("\\[")
      ? "\\["
      : source.startsWith("\\(")
        ? "\\("
        : source.startsWith("$")
          ? "$"
          : undefined;
  if (!open || (open === "$" && /\s/.test(source[1] || " "))) return;
  const close = open === "\\[" ? "\\]" : open === "\\(" ? "\\)" : open;
  const scan = source.slice(
    0,
    MAX_EQUATION_LENGTH + open.length + close.length,
  );
  let end = scan.indexOf(close, open.length);
  // Limit delimiter scanning even when a malformed transcript has no closer.
  while (end !== -1 && end <= MAX_EQUATION_LENGTH + open.length) {
    if (!escaped(source, end)) {
      if (
        open === "$" &&
        (/\s/.test(source[end - 1]) ||
          /[0-9$]/.test(source[end + 1] || "") ||
          source[end - 1] === "$")
      )
        return;
      const text = source.slice(open.length, end);
      // A currency opener must not consume a later code span or another line.
      if (open === "$" && /[`\r\n]/.test(text)) return;
      if (!text.trim()) return;
      return {
        raw: source.slice(0, end + close.length),
        text,
        display: open === "$$" || open === "\\[",
      };
    }
    end = scan.indexOf(close, end + close.length);
  }
}

// Local Marked extensions run before Markdown escapes/emphasis. They never
// inspect code tokens, raw HTML tokens, or link destinations.
export const mathExtensions: TokenizerExtension[] = [
  {
    name: "math_block",
    level: "block",
    start(source) {
      return /(?:^|\n) {0,3}(?:\$\$|\\\[)/.exec(source)?.index;
    },
    tokenizer(source) {
      const indent = /^ {0,3}/.exec(source)![0];
      const result = equation(source.slice(indent.length));
      if (!result?.display) return;
      const remaining = source.slice(indent.length + result.raw.length);
      if (!/^(?:[ \t]*(?:\n|$))/.test(remaining)) return;
      const trailing = /^[ \t]*(?:\n|$)/.exec(remaining)![0];
      return {
        ...result,
        type: "math_block",
        raw: indent + result.raw + trailing,
      };
    },
  },
  {
    name: "math_inline",
    level: "inline",
    start(source) {
      return /\$|\\[([]/.exec(source)?.index;
    },
    tokenizer(source) {
      const result = equation(source);
      if (result) return { ...result, type: "math_inline" };
    },
  },
];

let renderer: Promise<typeof import("katex")> | undefined;
function loadRenderer() {
  return (renderer ??= import("katex").catch((error) => {
    renderer = undefined;
    throw error;
  }));
}
// KaTeX clamps positive sizes, but negative rule shifts/kerns can bypass it.
// Inspect detached output before insertion, bounding signed dimensions/nodes.
function boundedMath(node: HTMLElement): boolean {
  const nodes = node.querySelectorAll("*");
  if (nodes.length > 4096) return false;
  const sizes = new Set([
    "width",
    "height",
    "depth",
    "voffset",
    "lspace",
    "rspace",
    "mathsize",
    "columnspacing",
    "rowspacing",
    "columnwidth",
    "minsize",
    "maxsize",
  ]);
  const units: Record<string, number> = {
    em: 1,
    ex: 0.5,
    px: 1 / 16,
    pt: 1 / 12,
    pc: 1,
    cm: 96 / 2.54 / 16,
    mm: 96 / 25.4 / 16,
    in: 6,
    mu: 1 / 18,
    "%": 0.01,
  };
  for (const element of nodes) {
    for (const name of element.getAttributeNames()) {
      if (!sizes.has(name)) continue;
      const value = element.getAttribute(name)!;
      for (const match of value.matchAll(
        /([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)\s*(em|ex|px|pt|pc|cm|mm|in|mu|%)/gi,
      )) {
        const dimension = Number(match[1]) * units[match[2].toLowerCase()];
        if (!Number.isFinite(dimension) || Math.abs(dimension) > 20)
          return false;
      }
      if (/infinity|nan/i.test(value)) return false;
    }
  }
  return true;
}
// Use KaTeX's DOM builder with native MathML: no transcript HTML sinks, remote
// renderer requests, bundled math fonts or shared user-defined macro state.
export async function renderEquation(
  node: HTMLElement,
  token: MathToken,
  current: () => boolean,
): Promise<void> {
  try {
    const { default: katex } = await loadRenderer();
    if (!current()) return;
    const reader = node.closest<HTMLElement>("#reader");
    const atBottom =
      reader &&
      reader.scrollHeight - reader.clientHeight - reader.scrollTop < 32;
    const prepared = node.ownerDocument.createElement("span");
    katex.render(token.text, prepared, {
      output: "mathml",
      displayMode: token.display,
      trust: false,
      throwOnError: true,
      strict: "ignore",
      maxExpand: 256,
      maxSize: 20,
      macros: {},
    });
    if (!boundedMath(prepared)) throw new Error("Equation size limit");
    if (!current()) return;
    node.replaceChildren(...Array.from(prepared.childNodes));
    node.classList.remove("math-source");
    if (atBottom) reader.scrollTop = reader.scrollHeight;
  } catch {
    // Invalid/unsupported/over-budget expressions remain readable LaTeX.
    if (current()) node.textContent = token.raw;
  }
}
