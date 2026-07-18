import { z } from 'zod'

export function nonEmptyString(message: string) {
  return z.string().trim().min(1, message)
}

export function optionalNumberString(message: string) {
  return z
    .string()
    .trim()
    .refine((value) => value === '' || Number.isFinite(Number(value)), message)
    .transform((value) => (value === '' ? null : Number(value)))
}

export function optionalIntegerString(message: string) {
  return z
    .string()
    .trim()
    .refine((value) => {
      if (value === '') {
        return true
      }
      const parsed = Number(value)
      return Number.isInteger(parsed)
    }, message)
    .transform((value) => (value === '' ? null : Number(value)))
}
