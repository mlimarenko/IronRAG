import type { TFunction } from "i18next";
import { FileText } from "lucide-react";

export function NoLibraryState({ t }: { t: TFunction }) {
  return (
    <div className="flex-1 flex flex-col">
      <div className="page-header">
        <h1 className="text-lg font-bold tracking-tight">
          {t("documents.title")}
        </h1>
      </div>
      <div className="empty-state flex-1">
        <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
          <FileText className="h-7 w-7 text-muted-foreground" />
        </div>
        <h2 className="text-base font-bold tracking-tight">
          {t("documents.noLibrary")}
        </h2>
        <p className="text-sm text-muted-foreground mt-2">
          {t("documents.noLibraryDesc")}
        </p>
      </div>
    </div>
  );
}
