// Request body shapes for the RustRAG HTTP API.
//
// Write endpoints accept JSON objects validated by the backend.
// We surface them as opaque records to keep call-sites flexible while
// ensuring no `any` leaks through the api layer.

export type CreateCredentialRequest = Record<string, unknown>;
export type UpdateCredentialRequest = Record<string, unknown>;
export type CreateBindingRequest = Record<string, unknown>;
export type UpdateBindingRequest = Record<string, unknown>;
export type CreateModelPresetRequest = Record<string, unknown>;
export type UpdateModelPresetRequest = Record<string, unknown>;
export type CreatePriceOverrideRequest = Record<string, unknown>;
