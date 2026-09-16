import type { Identity } from "./types.ts";

export const uploadLimits = {
  files: 16,
  fileBytes: 32 << 20,
  totalBytes: 128 << 20,
  chunkBytes: 256 << 10,
};
export type FileMetadata = { size: number; extension: string };
export type UploadStage = {
  protocol_version: number;
  request_id: string;
  stage_id: string;
  session: Identity;
  expires_at_unix: number;
  files: { index: number; path: string; size: number; sha256: string }[];
};
export function uploadMetadata(
  files: Pick<File, "name" | "size">[],
): FileMetadata[] {
  if (!files.length || files.length > uploadLimits.files)
    throw Error("파일은 한 번에 1–16개까지 첨부할 수 있습니다.");
  let total = 0;
  return files.map((file) => {
    if (
      !Number.isSafeInteger(file.size) ||
      file.size < 1 ||
      file.size > uploadLimits.fileBytes
    )
      throw Error(
        "빈 파일은 첨부할 수 없으며, 파일당 최대 크기는 32MiB입니다.",
      );
    total += file.size;
    if (total > uploadLimits.totalBytes)
      throw Error("한 번에 첨부하는 파일의 합계는 128MiB 이하여야 합니다.");
    const extension =
      /\.([a-z0-9]{1,16})$/i.exec(file.name)?.[1].toLowerCase() ?? "";
    return { size: file.size, extension };
  });
}
export function stagedPaths(
  value: unknown,
  session: Identity,
  files: FileMetadata[],
  now = Date.now(),
): { stage: UploadStage; text: string } {
  const stage = value as UploadStage;
  if (
    !stage ||
    stage.protocol_version !== 1 ||
    !/^[a-f0-9]{32}$/.test(stage.request_id) ||
    !/^[a-f0-9]{32}$/.test(stage.stage_id) ||
    stage.session?.id !== session.id ||
    stage.session?.created_at !== session.created_at ||
    !Number.isSafeInteger(stage.expires_at_unix) ||
    stage.expires_at_unix * 1000 <= now ||
    stage.expires_at_unix * 1000 > now + 3 * 3600_000 + 60_000 ||
    !Array.isArray(stage.files) ||
    stage.files.length !== files.length
  )
    throw Error("첨부 결과를 확인하지 못했습니다.");
  const paths = stage.files.map((file, index) => {
    const metadata = files[index];
    const suffix = `/hmux/staged-files-v1/${stage.expires_at_unix}-${stage.stage_id}/file-${String(index + 1).padStart(4, "0")}${metadata.extension ? "." + metadata.extension : ""}`;
    if (
      !file ||
      file.index !== index ||
      file.size !== metadata.size ||
      !/^[a-f0-9]{64}$/.test(file.sha256) ||
      typeof file.path !== "string" ||
      file.path.length > 4096 ||
      !file.path.startsWith("/") ||
      /[\x00-\x1f\x7f]/.test(file.path) ||
      file.path
        .split("/")
        .slice(1)
        .some((p) => !p || p === "." || p === "..") ||
      !file.path.endsWith(suffix)
    )
      throw Error("안전한 첨부 경로를 확인하지 못했습니다.");
    return "'" + file.path.replaceAll("'", "'\\''") + "'";
  });
  return { stage, text: paths.join(" ") + " " };
}

export function uploadFiles(
  files: File[],
  session: Identity,
  csrf: string,
  signal: AbortSignal,
  progress: (received: number, total: number) => void,
): Promise<{ stage: UploadStage; text: string }> {
  const metadata = uploadMetadata(files);
  const total = metadata.reduce((sum, f) => sum + f.size, 0);
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("취소했습니다.", "AbortError"));
      return;
    }
    const url = new URL("/api/upload", location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(url);
    let settled = false,
      received = 0,
      expected = 0,
      fileIndex = 0,
      fileOffset = 0;
    let phase: "connect" | "ready" | "reading" | "ack" | "complete" = "connect";
    let idle: ReturnType<typeof setTimeout>;
    const deadline = setTimeout(
      () => fail(Error("파일 전송 시간이 초과되었습니다. 다시 첨부해주세요.")),
      5 * 60_000,
    );
    const cleanup = () => {
      clearTimeout(idle);
      clearTimeout(deadline);
      signal.removeEventListener("abort", abort);
      ws.close();
    };
    const fail = (error: Error) => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(error);
    };
    const abort = () => fail(new DOMException("취소했습니다.", "AbortError"));
    const touch = () => {
      clearTimeout(idle);
      idle = setTimeout(
        () => fail(Error("파일 전송 응답이 없습니다. 연결을 확인해주세요.")),
        30_000,
      );
    };
    const next = async () => {
      try {
        if (received === total) {
          phase = "complete";
          ws.send(JSON.stringify({ type: "finish" }));
          touch();
          return;
        }
        phase = "reading";
        const file = files[fileIndex];
        const bytes = await file
          .slice(fileOffset, fileOffset + uploadLimits.chunkBytes)
          .arrayBuffer();
        if (settled) return;
        if (!bytes.byteLength)
          throw Error("파일을 읽지 못했습니다. 다시 선택해주세요.");
        expected = received + bytes.byteLength;
        fileOffset += bytes.byteLength;
        if (fileOffset === file.size) {
          fileIndex++;
          fileOffset = 0;
        }
        phase = "ack";
        ws.send(bytes);
        touch();
      } catch (error) {
        fail(error as Error);
      }
    };
    signal.addEventListener("abort", abort, { once: true });
    touch();
    ws.onopen = () => {
      if (settled) return;
      phase = "ready";
      ws.send(
        JSON.stringify({ type: "start", csrf, session, files: metadata }),
      );
      touch();
    };
    ws.onmessage = (event) => {
      if (settled) return;
      try {
        if (typeof event.data !== "string" || event.data.length > 65536)
          throw Error("잘못된 첨부 응답입니다.");
        const message = JSON.parse(event.data);
        if (message.type === "error")
          throw Error(
            "파일을 전송하지 못했습니다. Home 연결과 로그인 상태를 확인한 뒤 다시 시도해주세요.",
          );
        if (phase === "ready" && message.type === "ready") {
          progress(0, total);
          void next();
        } else if (
          phase === "ack" &&
          message.type === "ack" &&
          message.received === expected
        ) {
          received = expected;
          progress(received, total);
          void next();
        } else if (phase === "complete" && message.type === "complete") {
          const result = stagedPaths(message.stage, session, metadata);
          settled = true;
          cleanup();
          resolve(result);
        } else throw Error("첨부 전송 순서를 확인하지 못했습니다.");
      } catch (error) {
        fail(error as Error);
      }
    };
    ws.onerror = () =>
      fail(
        Error(
          "첨부 연결을 열지 못했습니다. Home 연결과 로그인 상태를 확인해주세요.",
        ),
      );
    ws.onclose = () =>
      fail(Error("파일 전송 중 연결이 끊겼습니다. 다시 첨부해주세요."));
  });
}
