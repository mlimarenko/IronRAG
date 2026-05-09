import { z } from "zod";

export function nonEmptyString(message: string) {
  return z.string().trim().min(1, message);
}

export function slugIdentifier(message: string) {
  return z
    .string()
    .trim()
    .regex(/^[a-z][a-z0-9_-]*$/i, message);
}

export function urlPattern(message: string) {
  return z.string().trim().refine((value) => {
    if (!value) {
      return false;
    }
    try {
      new URL(value);
      return true;
    } catch {
      return false;
    }
  }, message);
}

export function optionalTrimmedString() {
  return z.string().transform((value) => value.trim());
}

export function optionalNumberString(message: string) {
  return z
    .string()
    .trim()
    .refine((value) => value === "" || Number.isFinite(Number(value)), message)
    .transform((value) => (value === "" ? null : Number(value)));
}

export function optionalIntegerString(message: string) {
  return z
    .string()
    .trim()
    .refine((value) => {
      if (value === "") {
        return true;
      }
      const parsed = Number(value);
      return Number.isInteger(parsed);
    }, message)
    .transform((value) => (value === "" ? null : Number(value)));
}
