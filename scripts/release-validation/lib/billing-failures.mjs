function hasNon200Status(billing) {
  return (
    billing?.costStatus !== 200 || billing?.callsStatus !== 200 || billing?.chargesStatus !== 200
  );
}

export function classifyBillingFailure(billing) {
  const executionKind = String(billing?.executionKind ?? "");
  if (executionKind && executionKind !== "ingest_attempt") {
    return {
      category: "unsupported_kind",
      message: `Unsupported execution kind: ${executionKind}`,
    };
  }

  if (hasNon200Status(billing)) {
    return {
      category: "missing_rollup",
      message: `Billing endpoint status mismatch (cost=${billing?.costStatus ?? "n/a"}, calls=${billing?.callsStatus ?? "n/a"}, charges=${billing?.chargesStatus ?? "n/a"})`,
    };
  }

  if (billing?.chargesStatus === 200 && Number(billing?.chargeCount ?? 0) <= 0) {
    return {
      category: "empty_charges",
      message: "Charges endpoint succeeded but returned no charge rows.",
    };
  }

  const currencies = Array.isArray(billing?.chargeCurrencies)
    ? billing.chargeCurrencies.filter(Boolean).map((item) => String(item).toUpperCase())
    : [];
  if (currencies.length > 1) {
    return {
      category: "currency_conflict",
      message: `Multiple currencies observed: ${currencies.join(", ")}`,
    };
  }
  if (
    currencies.length === 1 &&
    billing?.currencyCode &&
    currencies[0] !== String(billing.currencyCode).toUpperCase()
  ) {
    return {
      category: "currency_conflict",
      message: `Cost currency ${billing.currencyCode} does not match charge currency ${currencies[0]}`,
    };
  }

  return {
    category: "ok",
    message: "Billing responses are consistent.",
  };
}
