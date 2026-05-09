import {
  type ComponentProps,
  type ReactNode,
  useId,
} from "react";
import {
  useController,
  type Control,
  type FieldPath,
  type FieldValues,
  type FormState,
  type UseFormRegisterReturn,
} from "react-hook-form";

import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";
import {
  Select,
  SelectContent,
  SelectTrigger,
  SelectValue,
} from "@/shared/components/ui/select";
import { Textarea } from "@/shared/components/ui/textarea";
import { cn } from "@/shared/lib/utils";

import { fieldErrorMessage } from "./fieldError";

type FieldRenderState = {
  describedBy: string | undefined;
  errorMessage: string | undefined;
  id: string;
  invalid: boolean;
};

type FormFieldProps<TValues extends FieldValues> = {
  children: (field: FieldRenderState) => ReactNode;
  className?: string;
  description?: ReactNode;
  formState: Pick<FormState<TValues>, "errors">;
  id?: string;
  label: ReactNode;
  name: FieldPath<TValues>;
};

export function FormField<TValues extends FieldValues>({
  children,
  className,
  description,
  formState,
  id: explicitId,
  label,
  name,
}: FormFieldProps<TValues>) {
  const generatedId = useId();
  const id = explicitId ?? generatedId;
  const descriptionId = description ? `${id}-description` : undefined;
  const errorId = `${id}-error`;
  const errorMessage = fieldErrorMessage(formState.errors, name);
  const describedBy = [descriptionId, errorMessage ? errorId : undefined]
    .filter(Boolean)
    .join(" ") || undefined;

  return (
    <div className={cn("space-y-2", className)}>
      <Label htmlFor={id}>{label}</Label>
      {children({
        describedBy,
        errorMessage,
        id,
        invalid: Boolean(errorMessage),
      })}
      {description && (
        <p id={descriptionId} className="text-xs text-muted-foreground">
          {description}
        </p>
      )}
      {errorMessage && (
        <p id={errorId} role="alert" className="text-xs text-destructive">
          {errorMessage}
        </p>
      )}
    </div>
  );
}

type FormInputFieldProps<TValues extends FieldValues> =
  Omit<ComponentProps<typeof Input>, "className" | "id" | "name"> &
  Omit<FormFieldProps<TValues>, "children"> & {
    inputClassName?: string;
    onValueChange?: (value: string) => void;
    registration: UseFormRegisterReturn<FieldPath<TValues>>;
  };

export function FormInputField<TValues extends FieldValues>({
  className,
  description,
  formState,
  id,
  inputClassName,
  label,
  name,
  onValueChange,
  registration,
  ...inputProps
}: FormInputFieldProps<TValues>) {
  const { onChange, ...registeredInput } = registration;
  return (
    <FormField
      className={className}
      description={description}
      formState={formState}
      id={id}
      label={label}
      name={name}
    >
      {({ describedBy, id, invalid }) => (
        <Input
          id={id}
          aria-describedby={describedBy}
          aria-invalid={invalid || undefined}
          className={inputClassName}
          {...registeredInput}
          {...inputProps}
          onChange={(event) => {
            void onChange(event);
            inputProps.onChange?.(event);
            onValueChange?.(event.target.value);
          }}
        />
      )}
    </FormField>
  );
}

type FormTextareaFieldProps<TValues extends FieldValues> =
  Omit<ComponentProps<typeof Textarea>, "className" | "id" | "name"> &
  Omit<FormFieldProps<TValues>, "children"> & {
    onValueChange?: (value: string) => void;
    registration: UseFormRegisterReturn<FieldPath<TValues>>;
    textareaClassName?: string;
  };

export function FormTextareaField<TValues extends FieldValues>({
  className,
  description,
  formState,
  id,
  label,
  name,
  onValueChange,
  registration,
  textareaClassName,
  ...textareaProps
}: FormTextareaFieldProps<TValues>) {
  const { onChange, ...registeredTextarea } = registration;
  return (
    <FormField
      className={className}
      description={description}
      formState={formState}
      id={id}
      label={label}
      name={name}
    >
      {({ describedBy, id, invalid }) => (
        <Textarea
          id={id}
          aria-describedby={describedBy}
          aria-invalid={invalid || undefined}
          className={textareaClassName}
          {...registeredTextarea}
          {...textareaProps}
          onChange={(event) => {
            void onChange(event);
            textareaProps.onChange?.(event);
            onValueChange?.(event.target.value);
          }}
        />
      )}
    </FormField>
  );
}

type FormSelectFieldProps<TValues extends FieldValues> =
  Omit<FormFieldProps<TValues>, "children"> & {
    children: ReactNode;
    control: Control<TValues>;
    disabled?: boolean;
    onValueChange?: (value: string) => void;
    placeholder?: string;
    triggerClassName?: string;
  };

export function FormSelectField<TValues extends FieldValues>({
  children,
  className,
  control,
  description,
  disabled,
  formState,
  id,
  label,
  name,
  onValueChange,
  placeholder,
  triggerClassName,
}: FormSelectFieldProps<TValues>) {
  const { field } = useController({ control, name });

  return (
    <FormField
      className={className}
      description={description}
      formState={formState}
      id={id}
      label={label}
      name={name}
    >
      {({ describedBy, id, invalid }) => (
        <Select
          disabled={disabled}
          value={typeof field.value === "string" ? field.value : ""}
          onValueChange={(value) => {
            field.onChange(value);
            onValueChange?.(value);
          }}
        >
          <SelectTrigger
            id={id}
            aria-describedby={describedBy}
            aria-invalid={invalid || undefined}
            className={triggerClassName}
          >
            <SelectValue placeholder={placeholder} />
          </SelectTrigger>
          <SelectContent>{children}</SelectContent>
        </Select>
      )}
    </FormField>
  );
}
