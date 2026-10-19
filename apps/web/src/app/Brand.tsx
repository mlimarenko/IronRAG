import { getBranding } from './branding'

export function BrandMark({
  compact = false,
  className = 'brand-mark',
  size = 28,
}: Readonly<{ compact?: boolean; className?: string; size?: number }>) {
  const branding = getBranding()
  return (
    <img
      className={className}
      src={compact ? branding.iconUrl : branding.logoUrl}
      alt=""
      aria-hidden="true"
      width={size}
      height={size}
      style={{ objectFit: 'contain', width: size, height: size }}
    />
  )
}

export function Brand({ compact = false }: Readonly<{ compact?: boolean }>) {
  const branding = getBranding()
  return (
    <div className={`brand ${compact ? 'brand--compact' : ''}`} aria-label={branding.displayName}>
      <BrandMark compact={compact} />
      {!compact && (
        <span
          className="brand-type"
          title={branding.displayName}
          style={{ minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}
        >
          <strong>{branding.displayName}</strong>
        </span>
      )}
    </div>
  )
}
