import { useTranslation } from "react-i18next";

import { FeatureErrorBoundary } from "@/shared/components/FeatureErrorBoundary";

import { DocumentsPage as DocumentsPageShell } from "./components/documents-page/DocumentsPage";

export default function DocumentsPage() {
  const { t } = useTranslation();

  return (
    <FeatureErrorBoundary feature={t("documents.title")}>
      <DocumentsPageShell />
    </FeatureErrorBoundary>
  );
}
