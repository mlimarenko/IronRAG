import { ArrowUpRight } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { versionApi } from "@/api/version";
import { BUILD_VERSION_LABEL, formatVersionTag } from "@/lib/build-version";

const RELEASE_UPDATE_REFETCH_INTERVAL_MS = 12 * 60 * 60 * 1000;

export function ShellFooter() {
  const { t } = useTranslation();
  const releaseUpdate = useQuery({
    queryKey: ["release-update"],
    queryFn: versionApi.getReleaseUpdate,
    staleTime: RELEASE_UPDATE_REFETCH_INTERVAL_MS,
    gcTime: RELEASE_UPDATE_REFETCH_INTERVAL_MS * 2,
    refetchInterval: RELEASE_UPDATE_REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: true,
    refetchOnWindowFocus: false,
    retry: 1,
  });

  const updateVersion = releaseUpdate.data?.latestVersion
    ? formatVersionTag(releaseUpdate.data.latestVersion)
    : null;
  const updateUrl = releaseUpdate.data?.releaseUrl ?? releaseUpdate.data?.repositoryUrl;
  const showUpdateLink =
    releaseUpdate.data?.status === "update_available" && updateVersion && updateUrl;

  return (
    <footer
      className="min-h-8 flex flex-wrap items-center justify-center px-4 py-1 gap-x-2 gap-y-1 shrink-0 text-[10px] sm:gap-x-4 sm:text-[11px] text-muted-foreground border-t"
      style={{
        background: "linear-gradient(180deg, hsl(var(--background)), hsl(var(--muted) / 0.3))",
      }}
    >
      <span className="font-medium">{BUILD_VERSION_LABEL}</span>
      {showUpdateLink ? (
        <a
          href={updateUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 rounded-full border border-amber-500/25 bg-amber-500/8 px-2 py-0.5 font-semibold text-amber-700 transition-colors hover:bg-amber-500/14 dark:text-amber-300"
        >
          <ArrowUpRight className="h-3 w-3" />
          <span>{t("common.updateAvailableFooter", { version: updateVersion })}</span>
        </a>
      ) : null}
      <span className="hidden sm:inline">
        {t("common.copyright", { year: new Date().getFullYear() })}
      </span>
      <a
        href="https://github.com/mlimarenko/IronRAG"
        target="_blank"
        rel="noopener noreferrer"
        className="hover:text-foreground transition-colors flex items-center gap-1"
      >
        <svg
          className="w-3.5 h-3.5"
          fill="currentColor"
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
          <path d="M12 .5C5.65.5.5 5.65.5 12a11.5 11.5 0 0 0 7.86 10.92c.58.11.79-.25.79-.56v-2.01c-3.2.7-3.88-1.37-3.88-1.37-.52-1.34-1.28-1.69-1.28-1.69-1.04-.71.08-.7.08-.7 1.15.08 1.75 1.18 1.75 1.18 1.02 1.75 2.67 1.24 3.32.95.1-.74.4-1.24.72-1.52-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.29 1.18-3.09-.12-.29-.51-1.46.11-3.04 0 0 .97-.31 3.18 1.18a11 11 0 0 1 5.79 0c2.21-1.49 3.18-1.18 3.18-1.18.62 1.58.23 2.75.11 3.04.73.8 1.18 1.83 1.18 3.09 0 4.42-2.69 5.4-5.26 5.69.41.35.78 1.05.78 2.12v3.15c0 .31.21.68.8.56A11.5 11.5 0 0 0 23.5 12C23.5 5.65 18.35.5 12 .5Z" />
        </svg>
        <span className="hidden sm:inline">{t("shell.github")}</span>
      </a>
    </footer>
  );
}
