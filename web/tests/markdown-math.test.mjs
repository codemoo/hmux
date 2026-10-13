import test from "node:test";
import assert from "node:assert/strict";
import { Marked } from "marked";
import {
  mathExtensions,
  renderEquation,
  MAX_MESSAGE_EQUATIONS,
} from "../src/markdown-math.ts";
import { renderMarkdown } from "../src/markdown.ts";

const markdown = new Marked({
  gfm: true,
  breaks: true,
  extensions: mathExtensions,
});
function collect(value, predicate) {
  if (!value || typeof value !== "object") return [];
  if (Array.isArray(value)) return value.flatMap((v) => collect(v, predicate));
  return [
    ...(predicate(value) ? [value] : []),
    ...Object.values(value).flatMap((v) => collect(v, predicate)),
  ];
}
const math = (source) =>
  collect(markdown.lexer(source), (v) => v.type?.startsWith("math_"));

test("inline/display math preserves TeX before Markdown emphasis, lists and tables", () => {
  const tokens = math(String.raw`Inline $x_1^2$ and \(\frac{a}{b}\).

$$
\sum_{i=1}^n x_i
$$

\[
\begin{pmatrix}a & b \\ c & d\end{pmatrix}
\]

| Formula | Label |
|---|---|
| $a_b$ | **bold** |

- $y_2$`);
  assert.equal(tokens.length, 6);
  assert.deepEqual(
    tokens.map((t) => t.display),
    [false, false, true, true, false, false],
  );
  assert.equal(tokens[0].text, "x_1^2");
  assert.equal(tokens[1].text, String.raw`\frac{a}{b}`);
  assert.ok(tokens[3].text.includes(String.raw`a & b \\ c & d`));
});

test("currency, escaped delimiters, fenced/indented/inline code and destinations stay literal", () => {
  const source = String.raw`Prices $5, $10 and $20. Price $5 and math $x$.

\$20 and \$30; \(a + \text{\$5}\).

${"`$code$`"} [Docs](https://example.com/$path$)

~~~tex
$$code$$
~~~

    $$indented$$

<script>$html$</script>`;
  const tokens = math(source);
  assert.equal(tokens.length, 2);
  assert.equal(tokens[0].text, "x");
  assert.equal(tokens[1].text, String.raw`a + \text{\$5}`);
  assert.equal(math("missing $close or \\(unclosed").length, 0);
  assert.equal(math("$" + "x".repeat(8193) + "$").length, 0);
  const mixed = markdown.lexer("Prices $5, $10 and $20. Inline code `$x$`.");
  assert.equal(collect(mixed, (v) => v.type?.startsWith("math_")).length, 0);
  assert.equal(collect(mixed, (v) => v.type === "codespan")[0].text, "$x$");
});

// Minimal DOM with real MathML namespaces/text nodes. No HTML parser or sinks;
// actual KaTeX DOM rendering is exercised rather than substituting an SVG image.
function documentFixture() {
  const doc = { compatMode: "CSS1Compat" };
  function element(tagName, namespaceURI) {
    const node = {
      tagName,
      namespaceURI,
      ownerDocument: doc,
      isConnected: true,
      children: [],
      attrs: {},
      style: {},
      className: "",
      setAttribute(k, v) {
        this.attrs[k] = v;
      },
      getAttributeNames() {
        return Object.keys(this.attrs);
      },
      getAttribute(name) {
        return this.attrs[name];
      },
      get childNodes() {
        return this.children;
      },
      querySelectorAll() {
        return descendants(this)
          .slice(1)
          .filter((n) => n.tagName !== "#text");
      },
      appendChild(child) {
        this.children.push(child);
        return child;
      },
      append(...children) {
        this.children.push(...children);
      },
      replaceChildren(...children) {
        this.children = children;
        this._text = "";
      },
      contains(other) {
        return this === other || this.children.some((c) => c.contains?.(other));
      },
      closest() {
        return undefined;
      },
      get textContent() {
        return (
          (this._text || "") + this.children.map((c) => c.textContent).join("")
        );
      },
      set textContent(value) {
        this._text = value;
        this.children = [];
      },
    };
    node.classList = {
      remove(name) {
        node.className = node.className
          .split(" ")
          .filter((v) => v !== name)
          .join(" ");
      },
    };
    return node;
  }
  doc.createElement = (tag) => element(tag);
  doc.createElementNS = (ns, tag) => element(tag, ns);
  doc.createTextNode = (text) => ({ tagName: "#text", textContent: text });
  globalThis.document = doc;
  return doc;
}
const descendants = (n) => [n, ...(n.children || []).flatMap(descendants)];
const equation = (text) => ({
  type: "math_inline",
  raw: `$${text}$`,
  text,
  display: false,
});

test("real renderer creates MathML fractions and leaves invalid/recursive TeX readable", async () => {
  const doc = documentFixture();
  const node = doc.createElement("span");
  node.className = "math-source";
  await renderEquation(node, equation(String.raw`\frac{a_1}{b^2}`), () => true);
  assert.ok(
    descendants(node).some(
      (n) =>
        n.tagName === "mfrac" &&
        n.namespaceURI === "http://www.w3.org/1998/Math/MathML",
    ),
  );
  for (const latex of [
    String.raw`\unknowncommand`,
    String.raw`\def\x{\x}\x`,
    String.raw`\rule[-100000em]{1em}{1em}`,
    String.raw`\kern-100000em x`,
  ]) {
    await renderEquation(node, equation(latex), () => true);
    assert.equal(node.textContent, `$${latex}$`);
  }
});

test("untrusted links/images/HTML cannot create active nodes and macros do not leak", async () => {
  const doc = documentFixture();
  const node = doc.createElement("span");
  for (const latex of [
    String.raw`\href{javascript:alert(1)}{x}`,
    String.raw`\includegraphics{https://example.com/tracker.png}`,
    String.raw`\htmlClass{bad}{x}`,
    String.raw`\text{<img src=x onerror=alert(1)>}`,
  ]) {
    await renderEquation(node, equation(latex), () => true);
    assert.ok(
      !descendants(node).some(
        (n) =>
          ["a", "img", "script", "iframe"].includes(n.tagName) ||
          n.attrs?.href ||
          n.attrs?.src,
      ),
    );
  }
  await renderEquation(
    node,
    equation(String.raw`\gdef\privateMacro{B}\privateMacro`),
    () => true,
  );
  await renderEquation(node, equation(String.raw`\privateMacro`), () => true);
  assert.equal(node.textContent, String.raw`$\privateMacro$`);
});

test("stale lazy render cannot replace a newer message and per-message work is bounded", async () => {
  const doc = documentFixture();
  const node = doc.createElement("span");
  node.textContent = "newer";
  await renderEquation(node, equation("x"), () => false);
  assert.equal(node.textContent, "newer");
  const root = doc.createElement("section");
  renderMarkdown(root, Array(70).fill("$x$").join(" "));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    descendants(root).filter((n) => n.tagName === "math").length,
    MAX_MESSAGE_EQUATIONS,
  );
  renderMarkdown(root, "$old$");
  renderMarkdown(root, "new message");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(root.textContent, "new message");
  const discarded = doc.createElement("section");
  renderMarkdown(discarded, "$x$");
  discarded.isConnected = false;
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    descendants(discarded).filter((n) => n.tagName === "math").length,
    0,
  );
});
