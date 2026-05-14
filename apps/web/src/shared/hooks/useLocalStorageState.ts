import { useEffect, useState, type Dispatch, type SetStateAction } from "react";

type UseLocalStorageStateOptions<T> = {
  key: string;
  defaultValue: T;
  parse: (raw: unknown) => T;
};

function readLocalStorageState<T>({
  key,
  defaultValue,
  parse,
}: UseLocalStorageStateOptions<T>): T {
  if (typeof window === "undefined") return defaultValue;
  try {
    const raw = window.localStorage.getItem(key);
    return raw == null ? defaultValue : parse(JSON.parse(raw));
  } catch {
    return defaultValue;
  }
}

export function useLocalStorageState<T>(
  options: UseLocalStorageStateOptions<T>,
): [T, Dispatch<SetStateAction<T>>] {
  const { key, defaultValue, parse } = options;
  const [state, setState] = useState<T>(() =>
    readLocalStorageState({ key, defaultValue, parse }),
  );

  useEffect(() => {
    if (typeof window === "undefined") return;
    try {
      window.localStorage.setItem(key, JSON.stringify(state));
    } catch {
      // Persistence is optional; blocked or full storage must not break the UI.
    }
  }, [key, state]);

  return [state, setState];
}
