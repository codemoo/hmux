import test from "node:test";
import assert from "node:assert/strict";
import {
  uploadMetadata,
  stagedPaths,
  uploadFiles,
} from "../src/file-upload.ts";
const session = { id: "$7", created_at: 1700000000 };
const now = 1700000000000;
const metadata = [{ size: 3, extension: "png" }];
function response() {
  const expiry = Math.floor(now / 1000) + 10800;
  return {
    protocol_version: 1,
    request_id: "a".repeat(32),
    stage_id: "b".repeat(32),
    session,
    expires_at_unix: expiry,
    files: [
      {
        index: 0,
        size: 3,
        sha256: "c".repeat(64),
        path: `/Users/o'neil/Library/Caches/hmux/staged-files-v1/${expiry}-${"b".repeat(32)}/file-0001.png`,
      },
    ],
  };
}
test("only bounded file sizes and sanitized extensions become upload metadata", () => {
  assert.deepEqual(
    uploadMetadata([{ name: "private-image.PNG", size: 3 }]),
    metadata,
  );
  assert.deepEqual(uploadMetadata([{ name: "secret.$(cmd)", size: 3 }]), [
    { size: 3, extension: "" },
  ]);
  for (const files of [
    [],
    Array(17).fill({ name: "a", size: 1 }),
    [{ name: "a", size: 0 }],
    [{ name: "a", size: 32 * 1024 * 1024 + 1 }],
    Array(5).fill({ name: "a", size: 32 * 1024 * 1024 }),
  ])
    assert.throws(() => uploadMetadata(files));
});
test("staged paths require exact session, safe shape, size and unexpired identity", () => {
  const valid = response();
  assert.equal(
    stagedPaths(valid, session, metadata, now).text,
    "'" + valid.files[0].path.replaceAll("'", "'\\''") + "' ",
  );
  for (const mutate of [
    (r) => (r.session = { ...session, created_at: 1 }),
    (r) => (r.expires_at_unix = 1),
    (r) => (r.files[0].size = 4),
    (r) => (r.files[0].sha256 = "invalid"),
    (r) => (r.files[0].path = "/tmp/a\nrm -rf"),
    (r) => (r.files[0].path = r.files[0].path.replace("/Library/", "/../")),
    (r) =>
      (r.files[0].path = r.files[0].path.replace(
        "file-0001.png",
        "file-0002.png",
      )),
  ]) {
    const r = response();
    mutate(r);
    assert.throws(() => stagedPaths(r, session, metadata, now));
  }
});
test("chunk protocol waits for ack, never transmits names, and abort closes upload", async () => {
  const previousWS = globalThis.WebSocket,
    previousLocation = globalThis.location;
  const sent = [];
  let socket;
  globalThis.location = { href: "https://example.com/" };
  globalThis.WebSocket = class {
    constructor(url) {
      assert.equal(url.protocol, "wss:");
      socket = this;
      queueMicrotask(() => this.onopen());
    }
    send(data) {
      sent.push(data);
    }
    close() {
      this.closed = true;
    }
  };
  try {
    const abort = new AbortController();
    const file = new File([new Uint8Array(300000)], "private-name.png");
    const progress = [];
    const promise = uploadFiles([file], session, "csrf", abort.signal, (n) =>
      progress.push(n),
    );
    await new Promise((r) => setTimeout(r, 0));
    assert.equal(sent.length, 1);
    assert.equal(sent[0].includes("private-name"), false);
    socket.onmessage({ data: '{"type":"ready"}' });
    await new Promise((r) => setTimeout(r, 0));
    assert.equal(sent.length, 2);
    assert.equal(sent[1].byteLength, 262144);
    socket.onmessage({ data: '{"type":"ack","received":262144}' });
    await new Promise((r) => setTimeout(r, 0));
    assert.equal(sent[2].byteLength, 37856);
    socket.onmessage({ data: '{"type":"ack","received":300000}' });
    assert.deepEqual(JSON.parse(sent[3]), { type: "finish" });
    assert.deepEqual(progress, [0, 262144, 300000]);
    const rejected = assert.rejects(promise, { name: "AbortError" });
    abort.abort();
    await rejected;
    assert.equal(socket.closed, true);
  } finally {
    globalThis.WebSocket = previousWS;
    globalThis.location = previousLocation;
  }
});
