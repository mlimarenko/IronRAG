import { useEffect, type ChangeEvent, type RefObject } from "react";
import type { TFunction } from "i18next";
import { FolderOpen, Link as LinkIcon, Upload } from "lucide-react";

import { Button } from "@/components/ui/button";

import { DOCUMENT_FILE_INPUT_ACCEPT } from "./uploadAccept";

type DocumentsPageHeaderProps = {
  activeLibraryName: string;
  activeTab: "documents" | "web";
  documentsCount: number;
  fileInputRef: RefObject<HTMLInputElement | null>;
  folderInputRef: RefObject<HTMLInputElement | null>;
  handleFileSelect: (event: ChangeEvent<HTMLInputElement>) => void;
  handleFolderSelect: (event: ChangeEvent<HTMLInputElement>) => void;
  setActiveTab: (tab: "documents" | "web") => void;
  setAddLinkOpen: (open: boolean) => void;
  setBoundaryPolicy: (value: string) => void;
  setCrawlMode: (value: string) => void;
  setMaxDepth: (value: string) => void;
  setMaxPages: (value: string) => void;
  setSeedUrl: (value: string) => void;
  t: TFunction;
  webRunsCount: number;
};

export function DocumentsPageHeader({
  activeLibraryName,
  activeTab,
  documentsCount,
  fileInputRef,
  folderInputRef,
  handleFileSelect,
  handleFolderSelect,
  setActiveTab,
  setAddLinkOpen,
  setBoundaryPolicy,
  setCrawlMode,
  setMaxDepth,
  setMaxPages,
  setSeedUrl,
  t,
  webRunsCount,
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
          </button>
        </div>

        <div className="flex gap-2">
          {activeTab === "documents" && (
            <>
              <Button size="sm" onClick={() => fileInputRef.current?.click()}>
                <Upload className="h-3.5 w-3.5 mr-1.5" />{" "}
                {t("documents.uploadFiles")}
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => folderInputRef.current?.click()}
              >
                <FolderOpen className="h-3.5 w-3.5 mr-1.5" />{" "}
                {t("documents.uploadFolder")}
              </Button>
            </>
          )}
          {activeTab === "web" && (
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
