import { zodResolver } from "@hookform/resolvers/zod";
import { useCallback } from "react";
import {
  useForm,
  type BaseSyntheticEvent,
  type DefaultValues,
  type FieldValues,
  type Resolver,
  type UseFormProps,
  type UseFormReturn,
} from "react-hook-form";
import { toast } from "sonner";
import type { z } from "zod";

import { errorMessage as formatErrorMessage } from "@/shared/lib/errorMessage";

type FormInput<TSchema extends z.ZodType> =
  z.input<TSchema> extends FieldValues ? z.input<TSchema> : FieldValues;

type FormValues<TSchema extends z.ZodType> =
  z.output<TSchema> extends FieldValues ? z.output<TSchema> : FieldValues;

export type TypedFormMutation<TValues extends FieldValues, TResult = unknown> = {
  mutateAsync: (values: TValues) => Promise<TResult>;
};

export type SubmitWithMutationOptions<
  TValues extends FieldValues,
  TResult = unknown,
> = {
  errorMessage?: string | ((error: unknown) => string);
  onError?: (error: unknown, values: TValues) => void;
  onSuccess?: (result: TResult, values: TValues) => void;
};

type UseTypedFormOptions<TSchema extends z.ZodType> =
  Omit<UseFormProps<FormInput<TSchema>, unknown, FormValues<TSchema>>, "resolver"> & {
    defaultValues: DefaultValues<FormInput<TSchema>>;
    schema: TSchema;
  };

export type TypedFormReturn<TSchema extends z.ZodType> =
  UseFormReturn<FormInput<TSchema>, unknown, FormValues<TSchema>> & {
    submitWithMutation: <TResult = unknown>(
      mutation: TypedFormMutation<FormValues<TSchema>, TResult>,
      options?: SubmitWithMutationOptions<FormValues<TSchema>, TResult>,
    ) => (event?: BaseSyntheticEvent) => Promise<void>;
  };

function resolveSubmitError(
  error: unknown,
  message: SubmitWithMutationOptions<FieldValues>["errorMessage"] | undefined,
) {
  if (typeof message === "function") {
    return message(error);
  }
  return formatErrorMessage(error, message ?? "Form submission failed");
}

export function useTypedForm<TSchema extends z.ZodType>({
  schema,
  ...options
}: UseTypedFormOptions<TSchema>): TypedFormReturn<TSchema> {
  const form = useForm<FormInput<TSchema>, unknown, FormValues<TSchema>>({
    ...options,
    resolver: zodResolver(schema) as Resolver<
      FormInput<TSchema>,
      unknown,
      FormValues<TSchema>
    >,
  });
  const { handleSubmit } = form;

  const submitWithMutation = useCallback<TypedFormReturn<TSchema>["submitWithMutation"]>(
    (mutation, submitOptions) =>
      handleSubmit(async (values) => {
        try {
          const result = await mutation.mutateAsync(values);
          submitOptions?.onSuccess?.(result, values);
        } catch (error) {
          toast.error(resolveSubmitError(error, submitOptions?.errorMessage));
          submitOptions?.onError?.(error, values);
        }
      }),
    [handleSubmit],
  );

  return {
    ...form,
    submitWithMutation,
  };
}

export type TypedSubmitHandler<TSchema extends z.ZodType> = (
  values: FormValues<TSchema>,
) => void | Promise<void>;
