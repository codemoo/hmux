import test from "node:test";
import assert from "node:assert/strict";
import {
  jobMessage,
  parseProviderResult,
  providerSummary,
} from "../src/provider-settings.ts";

test("provider results keep only known providers and bounded fields", () => {
  const result = parseProviderResult({
    providers: [
      {
        id: "codex",
        label: "Codex",
        installed: true,
        version: "0.155.1",
        auth: "api-key",
        key_hint: "…0001",
        profile: true,
        profile_id: "codex",
        key: "sk-should-never-be-read",
      },
      {
        id: "gemini",
        installed: "yes",
        auth: "root",
        key_hint: "x".repeat(40),
        profile_id: "Bad Profile",
      },
      { id: "unknown", installed: true },
      null,
    ],
  });
  assert.deepEqual(result.providers, [
    {
      id: "codex",
      label: "Codex",
      installed: true,
      version: "0.155.1",
      auth: "api-key",
      key_hint: "…0001",
      profile: true,
      profile_id: "codex",
    },
    {
      id: "gemini",
      label: "gemini",
      installed: false,
      version: "",
      auth: "none",
      key_hint: "",
      profile: false,
      profile_id: "",
    },
  ]);
});

test("provider jobs keep only safe login URLs and device codes", () => {
  const job = parseProviderResult({
    job: {
      state: "login",
      url: "https://claude.com/cai/oauth/authorize?code=true",
      code: "ABCD-12345",
      needs_input: true,
      log: ["line", 7, "x".repeat(400)],
    },
  }).job;
  assert.deepEqual(job, {
    state: "login",
    url: "https://claude.com/cai/oauth/authorize?code=true",
    code: "ABCD-12345",
    needs_input: true,
    log: ["line", "x".repeat(320)],
  });
  for (const url of [
    "http://auth.openai.com/x",
    "https://evil.example/x",
    "https://accounts.google.com.evil.example/x",
    "https://user:pw@auth.openai.com/x",
    "javascript:alert(1)",
  ])
    assert.equal(
      parseProviderResult({ job: { state: "login", url } }).job.url,
      "",
    );
  assert.equal(
    parseProviderResult({ job: { state: "login", code: "<b>1</b>" } }).job.code,
    "",
  );
  assert.equal(parseProviderResult({ job: { state: "rm" } }).job, undefined);
  assert.equal(
    parseProviderResult({ error: "Codex를 먼저 설치하세요" }).error,
    "Codex를 먼저 설치하세요",
  );
  assert.throws(() => parseProviderResult(null));
});

test("job messages guide each step", () => {
  const job = (state, extra = {}) => ({
    state,
    url: "",
    code: "",
    needs_input: false,
    log: [],
    ...extra,
  });
  assert.match(jobMessage(job("installing"), "connect"), /설치/);
  assert.match(
    jobMessage(
      job("login", {
        url: "https://auth.openai.com/codex/device",
        code: "ABCD-1234",
      }),
      "connect",
    ),
    /코드를 입력/,
  );
  assert.match(
    jobMessage(job("login", { needs_input: true }), "connect"),
    /붙여넣/,
  );
  assert.equal(jobMessage(job("connected"), "connect"), "연결되었습니다.");
  assert.equal(jobMessage(job("done"), "update"), "업데이트했습니다.");
  assert.match(jobMessage(job("failed"), "connect"), /로그/);
});

test("provider summary describes install and connection state", () => {
  const base = {
    id: "claude",
    label: "Claude Code",
    installed: false,
    version: "",
    auth: "none",
    key_hint: "",
    profile: false,
  };
  assert.equal(providerSummary(base), "설치되지 않음");
  assert.equal(
    providerSummary({ ...base, installed: true, version: "2.1.278" }),
    "v2.1.278 · 연결 필요",
  );
  assert.equal(
    providerSummary({ ...base, installed: true, auth: "account" }),
    "계정 연결됨",
  );
  assert.equal(
    providerSummary({ ...base, auth: "api-key", key_hint: "…abcd" }),
    "설치되지 않음 · API 키 …abcd",
  );
});
