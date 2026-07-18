import type {
  CreateAiAccountRequest,
  UpdateAiAccountRequest,
  CreateAiBindingRequest,
  UpdateAiBindingRequest,
  CreateProviderCatalogRequest as GeneratedCreateProviderCatalogRequest,
  UpdateProviderCatalogRequest as GeneratedUpdateProviderCatalogRequest,
  CreateModelCatalogRequest as GeneratedCreateModelCatalogRequest,
  UpdateModelCatalogRequest as GeneratedUpdateModelCatalogRequest,
  CreateWorkspacePriceOverrideRequest,
  UpdateWorkspacePriceOverrideRequest,
} from '@/shared/api/generated'

type OptionalKeys<T> = {
  [K in keyof T]-?: Record<string, never> extends Pick<T, K> ? K : never
}[keyof T]

type RequestInput<T> = Omit<T, OptionalKeys<T>> & {
  [K in OptionalKeys<T>]?: T[K] | undefined
}

export type CreateAccountRequest = RequestInput<CreateAiAccountRequest>
export type UpdateAccountRequest = RequestInput<UpdateAiAccountRequest>
export type CreateBindingRequest = RequestInput<CreateAiBindingRequest>
export type UpdateBindingRequest = RequestInput<UpdateAiBindingRequest>
export type CreateProviderRequest = RequestInput<GeneratedCreateProviderCatalogRequest>
export type UpdateProviderRequest = RequestInput<GeneratedUpdateProviderCatalogRequest>
export type CreateModelRequest = RequestInput<GeneratedCreateModelCatalogRequest>
export type UpdateModelRequest = RequestInput<GeneratedUpdateModelCatalogRequest>
export type CreatePriceOverrideRequest = RequestInput<CreateWorkspacePriceOverrideRequest>
export type UpdatePriceOverrideRequest = RequestInput<UpdateWorkspacePriceOverrideRequest>
