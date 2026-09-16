// Browser storage is optional. Shared tabs remain authoritative on Home even
// when a browser blocks localStorage or its quota is exhausted.
export function createPreferences(
  storage: () => Pick<Storage, "getItem" | "setItem" | "removeItem">,
) {
  const memory = new Map<string, string>();
  return {
    get(key: string): string | null {
      try {
        const value = storage().getItem(key);
        if (value !== null) memory.set(key, value);
        return value ?? memory.get(key) ?? null;
      } catch {
        return memory.get(key) ?? null;
      }
    },
    set(key: string, value: string) {
      memory.set(key, value);
      try {
        storage().setItem(key, value);
      } catch {
        // Keep this device's preferences in memory for the current visit.
      }
    },
    remove(key: string) {
      memory.delete(key);
      try {
        storage().removeItem(key);
      } catch {
        // Storage restrictions must not interrupt logout.
      }
    },
  };
}
