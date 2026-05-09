import type {
  CreateProviderCredentialRequest,
  UpdateProviderCredentialRequest,
  CreateBindingAssignmentRequest,
  UpdateBindingAssignmentRequest,
  CreateModelPresetRequest as GeneratedCreateModelPresetRequest,
  UpdateModelPresetRequest as GeneratedUpdateModelPresetRequest,
  CreateWorkspacePriceOverrideRequest,
} from "@/shared/api/generated";

type OptionalKeys<T> = {
  [K in keyof T]-?: Record<string, never> extends Pick<T, K> ? K : never;
}[keyof T];

type RequestInput<T> = Omit<T, OptionalKeys<T>> & {
  [K in OptionalKeys<T>]?: T[K] | undefined;
};

export type CreateCredentialRequest = RequestInput<CreateProviderCredentialRequest>;
export type UpdateCredentialRequest = RequestInput<UpdateProviderCredentialRequest>;
export type CreateBindingRequest = RequestInput<CreateBindingAssignmentRequest>;
export type UpdateBindingRequest = RequestInput<UpdateBindingAssignmentRequest>;
export type CreateModelPresetRequest = RequestInput<GeneratedCreateModelPresetRequest>;
export type UpdateModelPresetRequest = RequestInput<GeneratedUpdateModelPresetRequest>;
export type CreatePriceOverrideRequest = RequestInput<CreateWorkspacePriceOverrideRequest>;
