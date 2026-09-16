import type { Identity } from "./types";
export type SharedWorkspace = {
  conflict?: "workspace_conflict";
  version: number;
  initialized: boolean;
  revision: number;
  tabs: Identity[];
  selected?: Identity;
};
export type WorkspaceChange = {
  operation_id: string;
  revision: number;
  base: Identity[];
  tabs: Identity[];
  selected?: Identity;
};
export function validateWorkspace(value: SharedWorkspace): SharedWorkspace {
  if (
    (value.conflict !== undefined && value.conflict !== "workspace_conflict") ||
    typeof value.initialized !== "boolean" ||
    value.version !== 1 ||
    !Number.isSafeInteger(value.revision) ||
    value.revision < 0 ||
    !Array.isArray(value.tabs) ||
    value.tabs.length > 32
  )
    throw new Error("공용 탭 응답이 올바르지 않습니다.");
  const ids = new Set<string>();
  for (const id of value.tabs) {
    if (
      !id ||
      !/^\$[0-9]+$/.test(id.id) ||
      !Number.isSafeInteger(id.created_at) ||
      id.created_at < 1 ||
      ids.has(id.id)
    )
      throw new Error("공용 탭 식별자가 올바르지 않습니다.");
    ids.add(id.id);
  }
  if (
    value.selected &&
    !value.tabs.some((id) => sameIdentity(id, value.selected!))
  )
    throw new Error("공용 선택 탭이 없습니다.");
  return value;
}
export function sameIdentity(a: Identity, b: Identity) {
  return a.id === b.id && a.created_at === b.created_at;
}
