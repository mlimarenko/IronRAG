import { useEffect, type ChangeEvent, type RefObject } from "react";
import type { TFunction } from "i18next";
import {
  AlertTriangle,
  ChevronDown,
  FolderOpen,
  Link as LinkIcon,
  Loader2,
  RotateCw,
  Settings,
  Upload,
} from "lucide-react";

import { Button } from "@/shared/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/shared/components/ui/dropdown-menu";
import type { WebBoundaryPolicy, WebIngestMode } from "@/shared/api";

import { DOCUMENT_FILE_INPUT_ACCEPT } from "../model/uploadAccept";
import type { DocumentsPageTab } from "./documents-page/documentsPageState";

type DocumentsPageHeaderProps = {
  activeLibraryName: string;
  activeTab: DocumentsPageTab;
  canUpload: boolean;
  documentsCount: number;
  fileInputRef: RefObject<HTMLInputElement | null>;
  folderInputRef: RefObject<HTMLInputElement | null>;
  handleFileSelect: (event: ChangeEvent<HTMLInputElement>) => void;
  handleFolderSelect: (event: ChangeEvent<HTMLInputElement>) => void;
  hasActiveWebRun: boolean;
  setActiveTab: (tab: DocumentsPageTab) => void;
  setAddLinkOpen: (open: boolean) => void;
  setBoundaryPolicy: (value: WebBoundaryPolicy) => void;
  setCrawlMode: (value: WebIngestMode) => void;
  setMaxDepth: (value: string) => void;
  setMaxPages: (value: string) => void;
  setSeedUrl: (value: string) => void;
  onRefreshWebRuns: () => void;
  t: TFunction;
  webRunsRefreshing: boolean;
  webRunsCount: number;
  ingestionReady: boolean;
  onOpenAiSettings: () => void;
};

export function DocumentsPageHeader({
  activeLibraryName,
  activeTab,
  canUpload,
  documentsCount,
  fileInputRef,
  folderInputRef,
  handleFileSelect,
  handleFolderSelect,
  hasActiveWebRun,
  setActiveTab,
  setAddLinkOpen,
  setBoundaryPolicy,
  setCrawlMode,
  setMaxDepth,
  setMaxPages,
  setSeedUrl,
  onRefreshWebRuns,
  t,
  webRunsRefreshing,
  webRunsCount,
  ingestionReady,
  onOpenAiSettings,
}: DocumentsPageHeaderProps) {
  useEffect(() => {
    const folderInput = folderInputRef.current;
    if (!folderInput) {
      return;
    }
    folderInput.setAttribute("webkitdirectory", "");
    folderInput.setAttribute("directory", "");
  }, [folderInputRef]);

  return (
    <div className="page-header">
      {!ingestionReady && (
        <div className="mb-4 rounded-lg border border-status-warning/40 bg-status-warning/8 p-3 flex items-start gap-3">
          <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-status-warning" />
          <div className="min-w-0 flex-1">
            <p className="text-sm font-bold text-status-warning">
              {t("documents.ingestionNotReady")}
            </p>
            <p className="mt-1 text-sm text-muted-foreground">
              {t("documents.ingestionNotReadyDetail")}
            </p>
          </div>
          <Button size="sm" variant="outline" onClick={onOpenAiSettings} className="shrink-0">
            <Settings className="h-3.5 w-3.5 mr-1.5" />
            {t("documents.ingestionNotReadyAction")}
          </Button>
        </div>
      )}
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-lg font-bold tracking-tight">
            {t("documents.title")}
          </h1>
          <p className="text-sm text-muted-foreground">
            {activeLibraryName} - {t("documents.subtitle")}
          </p>
        </div>

        <div className="flex gap-0.5 p-1 bg-muted rounded-xl border border-border/50">
          <button
            className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${
              activeTab === "documents"
                ? "bg-primary text-primary-foreground font-semibold"
                : "text-muted-foreground hover:text-foreground"
            }`}
            onClick={() => setActiveTab("documents")}
          >
            {t("documents.tabs.documents")}
            <span
              className={`text-[10px] tabular-nums px-1.5 py-0.5 rounded-md ${activeTab === "documents" ? "bg-primary-foreground/20" : "bg-background/60"}`}
            >
              {documentsCount}
            </span>
          </button>
          <button
            className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${
              activeTab === "web"
                ? "bg-primary text-primary-foreground font-semibold"
                : "text-muted-foreground hover:text-foreground"
            }`}
            onClick={() => setActiveTab("web")}
          >
            {t("documents.tabs.webIngest")}
            <span
              className={`text-[10px] tabular-nums px-1.5 py-0.5 rounded-md ${activeTab === "web" ? "bg-primary-foreground/20" : "bg-background/60"}`}
            >
              {webRunsCount}
            </span>
            {hasActiveWebRun && (
              <span
                className="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse"
                aria-label={t("documents.activeWebRun")}
              />
            )}
          </button>
        </div>

        <div className="flex gap-2">
          {activeTab === "documents" && canUpload && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button size="sm" disabled={!ingestionReady}>
                  <Upload className="h-3.5 w-3.5 mr-1.5" />
                  {t("documents.addContent")}
                  <ChevronDown className="h-3 w-3 ml-1.5 opacity-70" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-[160px]">
                <DropdownMenuItem
                  onSelect={() => fileInputRef.current?.click()}
                >
                  <Upload className="h-3.5 w-3.5 mr-2 text-muted-foreground" />
                  {t("documents.addContentFiles")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => folderInputRef.current?.click()}
                >
                  <FolderOpen className="h-3.5 w-3.5 mr-2 text-muted-foreground" />
                  {t("documents.addContentFolder")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
          {activeTab === "web" && (
            <>
              <Button
                size="sm"
                variant="outline"
                disabled={webRunsRefreshing}
                onClick={onRefreshWebRuns}
              >
                {webRunsRefreshing ? (
                  <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
                ) : (
                  <RotateCw className="h-3.5 w-3.5 mr-1.5" />
                )}{" "}
                {t("documents.refreshRuns")}
              </Button>
              {canUpload && (
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => {
                    setSeedUrl("");
                    setCrawlMode("recursive_crawl");
                    setBoundaryPolicy("same_host");
                    setMaxDepth("3");
                    setMaxPages("30");
                    setAddLinkOpen(true);
                  }}
                >
                  <LinkIcon className="h-3.5 w-3.5 mr-1.5" />{" "}
                  {t("documents.addLink")}
                </Button>
              )}
            </>
          )}
          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept={DOCUMENT_FILE_INPUT_ACCEPT}
            className="hidden"
            onChange={handleFileSelect}
          />
          <input
            ref={folderInputRef}
            type="file"
            multiple
            className="hidden"
            onChange={handleFolderSelect}
          />
        </div>
      </div>
    </div>
  );
}
