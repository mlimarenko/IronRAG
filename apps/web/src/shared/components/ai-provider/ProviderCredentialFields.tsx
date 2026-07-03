import { Input } from '@/shared/components/ui/input';
import { Label } from '@/shared/components/ui/label';
import {
  canEditProviderBaseUrl,
  normalizeProviderBaseUrl,
  resolveProviderCredentialPolicy,
  shouldRenderBaseUrlInput,
} from '@/shared/lib/ai-provider';

export type ProviderCredentialFieldSource = {
  credentialSource?: string;
  defaultBaseUrl?: string | null;
  credentialPolicy: unknown;
  baseUrlPolicy: unknown;
  uiHints?: unknown;
};

type ProviderCredentialFieldLabels = {
  apiKeyRequired: string;
  apiKeyOptional: string;
  apiKeyPlaceholder: string;
  apiKeyRequiredHint: string;
  baseUrlRequired: string;
  baseUrlOptional: string;
  baseUrlRequiredHint: string;
  fixedBaseUrlHint: string;
  keepSecretPlaceholder?: string;
};

type ProviderCredentialFieldsProps = {
  provider: ProviderCredentialFieldSource | null;
  idPrefix: string;
  apiKey: string;
  baseUrl: string;
  labels: ProviderCredentialFieldLabels;
  onApiKeyChange: (value: string) => void;
  onBaseUrlChange: (value: string) => void;
  apiKeyError?: string;
  baseUrlError?: string;
  preserveExistingSecret?: boolean;
  allowBaseUrlOverride?: boolean;
};

function providerHint(provider: ProviderCredentialFieldSource | null, key: string): string {
  const hints = provider?.uiHints;
  const value = hints && typeof hints === 'object' && !Array.isArray(hints)
    ? (hints as Record<string, unknown>)[key]
    : undefined;
  return typeof value === 'string' ? value : '';
}

export function ProviderCredentialFields({
  provider,
  idPrefix,
  apiKey,
  baseUrl,
  labels,
  onApiKeyChange,
  onBaseUrlChange,
  apiKeyError,
  baseUrlError,
  preserveExistingSecret = false,
  allowBaseUrlOverride = false,
}: ProviderCredentialFieldsProps) {
  const credentialPolicy = provider ? resolveProviderCredentialPolicy(provider) : null;
  const baseUrlEditable =
    canEditProviderBaseUrl(provider)
    || (allowBaseUrlOverride && Boolean(provider) && provider?.credentialSource !== 'env');
  const baseUrlVisible = shouldRenderBaseUrlInput(provider);
  const defaultBaseUrl = provider ? normalizeProviderBaseUrl(provider, provider.defaultBaseUrl) : '';
  const apiKeyDisabled = !provider || provider.credentialSource === 'env';
  const baseUrlFieldId = `${idPrefix}-base-url`;
  const baseUrlHintId = `${baseUrlFieldId}-hint`;
  const baseUrlErrorId = `${baseUrlFieldId}-error`;
  const apiKeyFieldId = `${idPrefix}-api-key`;
  const apiKeyHintId = `${apiKeyFieldId}-hint`;
  const apiKeyErrorId = `${apiKeyFieldId}-error`;
  const baseUrlHint = providerHint(provider, 'baseUrlHint');
  const apiKeyHint = providerHint(provider, 'apiKeyHint');
  const baseUrlDescriptionIds = Array.from(new Set([
    baseUrlError ? baseUrlErrorId : '',
    (baseUrlHint || !baseUrlEditable) ? baseUrlHintId : '',
  ].filter(Boolean))).join(' ') || undefined;
  const apiKeyDescriptionIds = [
    apiKeyError ? apiKeyErrorId : '',
    apiKeyHint ? apiKeyHintId : '',
  ].filter(Boolean).join(' ') || undefined;

  return (
    <div className="space-y-4">
      {baseUrlVisible && (
        <div>
          <Label htmlFor={baseUrlFieldId}>
            {credentialPolicy?.baseUrlRequired ? labels.baseUrlRequired : labels.baseUrlOptional}
          </Label>
          {baseUrlEditable ? (
            <Input
              id={baseUrlFieldId}
              className="mt-2 font-mono text-xs"
              type="text"
              placeholder={defaultBaseUrl}
              value={baseUrl}
              onChange={event => onBaseUrlChange(event.target.value)}
              aria-describedby={baseUrlDescriptionIds}
              aria-invalid={Boolean(baseUrlError) || undefined}
              aria-required={credentialPolicy?.baseUrlRequired || undefined}
            />
          ) : (
            <div
              id={baseUrlFieldId}
              className="mt-2 select-text rounded-lg border border-border bg-surface-sunken px-3 py-2 font-mono text-xs leading-relaxed text-foreground [overflow-wrap:anywhere]"
              aria-describedby={baseUrlDescriptionIds}
              aria-label={defaultBaseUrl || labels.baseUrlRequiredHint}
              aria-invalid={Boolean(baseUrlError) || undefined}
              aria-readonly="true"
              role="textbox"
              tabIndex={0}
              title={defaultBaseUrl}
            >
              {defaultBaseUrl || labels.baseUrlRequiredHint}
            </div>
          )}
          {baseUrlError && (
            <p id={baseUrlErrorId} role="alert" className="mt-2 text-xs text-destructive">
              {baseUrlError}
            </p>
          )}
          {(baseUrlHint || !baseUrlEditable) && (
            <p id={baseUrlHintId} className="mt-2 text-xs text-muted-foreground">
              {baseUrlHint || labels.fixedBaseUrlHint}
            </p>
          )}
        </div>
      )}

      <div>
        <Label htmlFor={apiKeyFieldId}>
          {credentialPolicy?.apiKeyRequired ? labels.apiKeyRequired : labels.apiKeyOptional}
        </Label>
        <Input
          id={apiKeyFieldId}
          type="password"
          className="mt-2"
          value={apiKey}
          onChange={event => onApiKeyChange(event.target.value)}
          placeholder={preserveExistingSecret ? labels.keepSecretPlaceholder ?? labels.apiKeyPlaceholder : labels.apiKeyPlaceholder}
          disabled={apiKeyDisabled}
          aria-describedby={apiKeyDescriptionIds}
          aria-invalid={Boolean(apiKeyError) || undefined}
          aria-required={(credentialPolicy?.apiKeyRequired && !apiKeyDisabled && !preserveExistingSecret) || undefined}
        />
        {apiKeyError && (
          <p id={apiKeyErrorId} role="alert" className="mt-2 text-xs text-destructive">
            {apiKeyError}
          </p>
        )}
        {apiKeyHint && (
          <p id={apiKeyHintId} className="mt-2 text-xs text-muted-foreground">{apiKeyHint}</p>
        )}
      </div>
    </div>
  );
}
