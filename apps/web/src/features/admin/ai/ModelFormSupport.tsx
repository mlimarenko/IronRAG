export function PriceField({
  label,
  value,
  onChange,
}: Readonly<{
  label: string
  value: string
  onChange: (value: string) => void
}>) {
  return (
    <label className="field">
      <span>{label}</span>
      <input
        className="input"
        required
        inputMode="decimal"
        pattern="\d+(\.\d+)?"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  )
}
