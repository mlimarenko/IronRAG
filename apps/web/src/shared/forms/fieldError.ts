import type {
  FieldErrors,
  FieldPath,
  FieldValues,
} from "react-hook-form";

export function fieldErrorMessage<TValues extends FieldValues>(
  errors: FieldErrors<TValues>,
  name: FieldPath<TValues>,
) {
  let current: unknown = errors;
  for (const segment of String(name).split(".")) {
    if (!current || typeof current !== "object") {
      return undefined;
    }
    current = (current as Record<string, unknown>)[segment];
  }
  if (!current || typeof current !== "object") {
    return undefined;
  }
  const message = (current as { message?: unknown }).message;
  return typeof message === "string" ? message : undefined;
}
