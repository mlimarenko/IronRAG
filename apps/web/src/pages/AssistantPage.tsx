import { useState, useRef, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useApp } from "@/contexts/AppContext";
import { useNavigate } from "react-router-dom";
import ReactMarkdown from "react-markdown";
import { queryApi } from "@/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { mapSourceAccess } from "@/lib/source-access";
import { mapAssistantVerificationState } from "@/pages/assistant/verification";
import {
  Send,
  Plus,
  Search,
  Loader2,
  FileText,
  Share2,
  AlertTriangle,
  CheckCircle2,
  MessageSquare,
  Brain,
  Target,
  Zap,
  XCircle,
} from "lucide-react";
import type {
  AssistantSession,
  AssistantMessage,
  VerificationState,
  EvidenceBundle,
} from "@/types";
import type {
  RawAssistantTurnResponse,
  RawAssistantSession,
  RawAssistantMessage,
} from "@/types/api-responses";

const STARTER_PROMPT_KEYS = [
  "assistant.starterPrompts.technologies",
  "assistant.starterPrompts.deployment",
  "assistant.starterPrompts.security",
  "assistant.starterPrompts.storage",
] as const;

/* ── API response → UI mapping ─────────────────────────────────── */

function errorMessage(err: unknown, fallback: string): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "object" && err !== null && "message" in err) {
    const msg = (err as { message?: unknown }).message;
    if (typeof msg === "string") return msg;
  }
  return fallback;
}

function mapTurnResponseToEvidence(
  resp: RawAssistantTurnResponse,
): EvidenceBundle {
  return {
    segmentRefs: (resp.preparedSegmentReferences ?? []).map((r) => {
      const trail = Array.isArray(r.headingTrail)
        ? r.headingTrail.filter((h): h is string => typeof h === "string")
        : [];
      const path = Array.isArray(r.sectionPath)
        ? r.sectionPath.filter((p): p is string => typeof p === "string")
        : [];
      return {
        documentId: r.documentId ?? r.segmentId ?? "",
        documentName:
          trail.length > 0
            ? trail[trail.length - 1]
            : path.join(" / ") || r.blockKind || "Segment",
        documentTitle: r.documentTitle ?? null,
        sourceUri: r.sourceUri ?? null,
        sourceAccess: mapSourceAccess(r.sourceAccess) ?? null,
        segmentOrdinal: r.rank ?? 0,
        excerpt: trail.join(" > ") || path.join(" > ") || "",
        relevance: r.score ?? 0,
      };
    }),
    factRefs: (resp.technicalFactReferences ?? []).map((r) => ({
      factKind: r.factKind,
      value:
        typeof r.displayValue === "string"
          ? r.displayValue
          : typeof r.canonicalValue === "string"
            ? r.canonicalValue
            : String(r.displayValue ?? r.canonicalValue ?? ""),
      confidence: r.score ?? 0,
      documentName: "",
    })),
    entityRefs: (resp.entityReferences ?? []).map((r) => ({
      entityId: r.nodeId,
      label: typeof r.label === "string" ? r.label : "Entity",
      type: r.entityType || "unknown",
      relevance: r.score ?? 0,
    })),
    relationRefs: (resp.relationReferences ?? []).map((r) => ({
      sourceLabel: r.predicate || "",
      targetLabel: r.normalizedAssertion || "",
      relation: r.predicate || "",
      weight: r.score ?? 0,
    })),
    verificationState: mapAssistantVerificationState(resp.verificationState),
    verificationWarnings: (resp.verificationWarnings ?? []).map(
      (w) => w.message ?? w.code ?? "",
    ),
    runtimeSummary: {
      totalSegments: (resp.preparedSegmentReferences ?? []).length,
      totalFacts: (resp.technicalFactReferences ?? []).length,
      totalEntities: (resp.entityReferences ?? []).length,
      totalRelations: (resp.relationReferences ?? []).length,
      stages: (resp.runtimeStageSummaries ?? []).map((s) => ({
        stage: s.stageKind,
        durationMs: 0,
        itemCount: 0,
      })),
      policyInterventions: [],
    },
  };
}

function mapApiSession(s: RawAssistantSession): AssistantSession {
  return {
    id: s.id,
    libraryId: s.libraryId,
    title: s.title || "",
    updatedAt: s.updatedAt,
    turnCount: s.turnCount ?? 0,
  };
}

function mapApiMessage(m: RawAssistantMessage): AssistantMessage {
  return {
    id: m.id,
    role: m.role === "user" ? "user" : "assistant",
    content: m.content ?? "",
    timestamp: m.timestamp,
  };
}

const verificationConfig: Record<
  VerificationState,
  { icon: typeof CheckCircle2; labelKey: string; cls: string }
> = {
  passed: {
    icon: CheckCircle2,
    labelKey: "assistant.verified",
    cls: "text-status-ready",
  },
  partially_supported: {
    icon: AlertTriangle,
    labelKey: "assistant.partiallySupported",
    cls: "text-status-warning",
  },
  conflicting: {
    icon: XCircle,
    labelKey: "assistant.conflictingEvidence",
    cls: "text-status-failed",
  },
  insufficient_evidence: {
    icon: AlertTriangle,
    labelKey: "assistant.insufficientEvidence",
    cls: "text-status-sparse",
  },
  failed: {
    icon: XCircle,
    labelKey: "assistant.verificationFailed",
    cls: "text-status-failed",
  },
  not_run: {
    icon: Brain,
    labelKey: "assistant.verificationNotRun",
    cls: "text-muted-foreground",
  },
};

export default function AssistantPage() {
  const { t } = useTranslation();
  const { activeLibrary, activeWorkspace, locale } = useApp();
  const navigate = useNavigate();
  const [sessions, setSessions] = useState<AssistantSession[]>([]);
  const [activeSession, setActiveSession] = useState<string | null>(null);
  const [messages, setMessages] = useState<AssistantMessage[]>([]);
  const [inputText, setInputText] = useState("");
  const [isExecuting, setIsExecuting] = useState(false);
  const [executionStage, setExecutionStage] = useState<string | null>(null);
  const [sessionSearch, setSessionSearch] = useState("");
  const [showEvidence, setShowEvidence] = useState(true);
  const [showSessionRail, setShowSessionRail] = useState(true);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const workspaceId = activeWorkspace?.id ?? activeLibrary?.workspaceId;
  const libraryId = activeLibrary?.id;

  /* ── Load sessions when library changes ──────────────────────── */
  const loadSessions = useCallback(async () => {
    if (!workspaceId || !libraryId) return;
    try {
      const data = await queryApi.listSessions({ workspaceId, libraryId });
      setSessions(data.map(mapApiSession));
    } catch (err: unknown) {
      console.error("Failed to load sessions:", err);
      toast.error(errorMessage(err, t("assistant.loadSessionsFailed")));
    }
  }, [libraryId, t, workspaceId]);

  useEffect(() => {
    loadSessions();
  }, [loadSessions]);

  /* ── Load conversation messages when session changes ──────── */
  const loadSessionMessages = useCallback(async (sessionId: string) => {
    try {
      const data = await queryApi.getSession(sessionId);
      setMessages((data.messages ?? []).map(mapApiMessage));
    } catch {
      setMessages([]);
    }
  }, []);

  const handleSelectSession = useCallback(
    (sessionId: string) => {
      setActiveSession(sessionId);
      loadSessionMessages(sessionId);
    },
    [loadSessionMessages],
  );

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  /* ── Create new session ──────────────────────────────────────── */
  const handleNewSession = useCallback(async () => {
    setActiveSession(null);
    setMessages([]);
  }, []);

  /* ── Send a question ─────────────────────────────────────────── */
  const handleSend = async () => {
    if (!inputText.trim()) return;
    if (!workspaceId || !libraryId) return;

    const questionText = inputText.trim();
    const userMsg: AssistantMessage = {
      id: `m-${Date.now()}`,
      role: "user",
      content: questionText,
      timestamp: new Date().toISOString(),
    };
    setMessages((prev) => [...prev, userMsg]);
    setInputText("");
    setIsExecuting(true);
    setExecutionStage("planning");

    try {
      // Ensure we have a session
      let sessionId = activeSession;
      if (!sessionId) {
        const session = await queryApi.createSession(workspaceId, libraryId);
        sessionId = session.id;
        setActiveSession(sessionId);
      }

      setExecutionStage("grounding");

      // Submit the turn (synchronous — returns the full result)
      const result = await queryApi.createTurn(sessionId, questionText);

      setExecutionStage("response");

      const answerText =
        result.responseTurn?.contentText ?? t("assistant.noResponseGenerated");
      const evidence = mapTurnResponseToEvidence(result);

      setMessages((prev) => [
        ...prev,
        {
          id: result.responseTurn?.id ?? `m-${Date.now() + 1}`,
          role: "assistant",
          content: answerText,
          timestamp: result.responseTurn?.createdAt ?? new Date().toISOString(),
          evidence,
        },
      ]);

      // Refresh session list to pick up new/updated session
      loadSessions();
    } catch (err: unknown) {
      setMessages((prev) => [
        ...prev,
        {
          id: `m-err-${Date.now()}`,
          role: "assistant",
          content: t("assistant.sendError", {
            error: errorMessage(err, t("assistant.unknownError")),
          }),
          timestamp: new Date().toISOString(),
        },
      ]);
    } finally {
      setIsExecuting(false);
      setExecutionStage(null);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const latestEvidence = [...messages]
    .reverse()
    .find((m) => m.role === "assistant" && m.evidence)?.evidence;

  if (!activeLibrary) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">
            {t("assistant.title")}
          </h1>
        </div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
            <MessageSquare className="h-7 w-7 text-muted-foreground" />
          </div>
          <h2 className="text-base font-bold tracking-tight">
            {t("assistant.noLibrary")}
          </h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm">
            {t("assistant.noLibraryDesc")}
          </p>
          <Button
            variant="outline"
            size="sm"
            className="mt-4"
            onClick={() => navigate("/documents")}
          >
            <FileText className="h-3.5 w-3.5 mr-1.5" />{" "}
            {t("assistant.goToDocuments")}
          </Button>
        </div>
      </div>
    );
  }

  if (activeLibrary.missingBindingPurposes.includes("query_answer")) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">
            {t("assistant.title")}
          </h1>
        </div>
        <div className="empty-state flex-1">
          <div
            className="w-14 h-14 rounded-2xl flex items-center justify-center mb-4"
            style={{
              background: "hsl(var(--status-warning-bg))",
              boxShadow:
                "inset 0 0 0 1px hsl(var(--status-warning-ring) / 0.3)",
            }}
          >
            <AlertTriangle className="h-7 w-7 text-status-warning" />
          </div>
          <h2 className="text-base font-bold tracking-tight">
            {t("assistant.queryNotConfigured")}
          </h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm">
            {t("assistant.queryNotConfiguredDesc")}
          </p>
          <Button
            variant="outline"
            size="sm"
            className="mt-4"
            onClick={() => navigate("/admin")}
          >
            {t("assistant.goToAdmin")}
          </Button>
        </div>
      </div>
    );
  }

  const sessionTitle = (title: string) =>
    title || t("assistant.untitledSession");
  const filteredSessions = sessions.filter(
    (s) =>
      !sessionSearch ||
      sessionTitle(s.title).toLowerCase().includes(sessionSearch.toLowerCase()),
  );

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="page-header flex items-center justify-between">
        <h1 className="text-lg font-bold tracking-tight">
          {t("assistant.title")}
        </h1>
        <div className="flex gap-2">
          <Button
            variant="ghost"
            size="sm"
            className="md:hidden"
            onClick={() => setShowSessionRail(!showSessionRail)}
          >
            {t("assistant.sessions")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setShowEvidence(!showEvidence)}
          >
            {showEvidence
              ? t("assistant.evidenceOn")
              : t("assistant.evidenceOff")}
          </Button>
        </div>
      </div>

      <div className="flex-1 flex overflow-hidden">
        {/* Session rail */}
        <div
          className={`${showSessionRail ? "w-64" : "w-0 overflow-hidden"} shrink-0 border-r bg-surface-sunken/30 transition-all duration-250 md:w-64`}
        >
          <div className="p-3 space-y-2">
            <Button size="sm" className="w-full" onClick={handleNewSession}>
              <Plus className="h-3.5 w-3.5 mr-1.5" />{" "}
              {t("assistant.newSession")}
            </Button>
            <div className="relative">
              <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3 w-3 text-muted-foreground" />
              <Input
                className="h-8 pl-8 text-xs"
                placeholder={t("assistant.searchSessions")}
                value={sessionSearch}
                onChange={(e) => setSessionSearch(e.target.value)}
              />
            </div>
          </div>
          <div className="px-2 space-y-0.5">
            {filteredSessions.map((s) => (
              <button
                key={s.id}
                onClick={() => handleSelectSession(s.id)}
                className={`w-full text-left px-3 py-2.5 rounded-xl text-sm transition-all duration-200 ${activeSession === s.id ? "bg-card shadow-soft font-semibold border border-border/50" : "hover:bg-accent/50"}`}
              >
                <div className="truncate">{sessionTitle(s.title)}</div>
                <div className="text-[11px] text-muted-foreground mt-0.5">
                  {new Intl.DateTimeFormat(locale).format(
                    new Date(s.updatedAt),
                  )}
                </div>
              </button>
            ))}
          </div>
        </div>

        {/* Conversation thread */}
        <div className="flex-1 flex flex-col overflow-hidden">
          <div className="flex-1 overflow-y-auto p-4 space-y-4">
            {messages.length === 0 ? (
              <div className="flex-1 flex flex-col items-center justify-center py-16 animate-fade-in">
                <div
                  className="w-16 h-16 rounded-2xl flex items-center justify-center mb-5"
                  style={{
                    background:
                      "linear-gradient(135deg, hsl(var(--primary) / 0.15), hsl(var(--primary) / 0.05))",
                    boxShadow: "0 0 0 1px hsl(var(--primary) / 0.1)",
                  }}
                >
                  <Brain className="h-8 w-8 text-primary" />
                </div>
                <h2 className="text-base font-bold tracking-tight">
                  {t("assistant.askQuestion")}
                </h2>
                <p className="text-sm text-muted-foreground mt-1.5 mb-6">
                  {t("assistant.askQuestionDesc")}
                </p>
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5 max-w-md w-full">
                  {STARTER_PROMPT_KEYS.map((key) => {
                    const prompt = t(key);
                    return (
                      <button
                        key={key}
                        className="text-left p-4 rounded-xl border hover:bg-accent/50 hover:shadow-soft transition-all duration-200 text-sm font-medium"
                        onClick={() => setInputText(prompt)}
                      >
                        {prompt}
                      </button>
                    );
                  })}
                </div>
              </div>
            ) : (
              messages.map((msg) => (
                <div
                  key={msg.id}
                  className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"} animate-fade-in`}
                >
                  <div
                    className={`max-w-[80%] ${msg.role === "user" ? "text-primary-foreground rounded-2xl rounded-br-sm px-4 py-3" : "space-y-2"}`}
                    style={
                      msg.role === "user"
                        ? {
                            background:
                              "linear-gradient(135deg, hsl(var(--primary)), hsl(224 76% 42%))",
                            boxShadow:
                              "0 2px 8px -2px hsl(var(--primary) / 0.4)",
                          }
                        : undefined
                    }
                  >
                    {msg.role === "assistant" && msg.evidence && (
                      <div className="flex items-center gap-2 text-xs">
                        {(() => {
                          const vc =
                            verificationConfig[msg.evidence.verificationState];
                          return (
                            <>
                              <vc.icon className={`h-3 w-3 ${vc.cls}`} />
                              <span className={`font-semibold ${vc.cls}`}>
                                {t(vc.labelKey)}
                              </span>
                            </>
                          );
                        })()}
                      </div>
                    )}
                    <div
                      className={`text-sm leading-relaxed ${msg.role === "assistant" ? "bg-card border rounded-2xl rounded-bl-sm px-4 py-3 shadow-soft" : ""}`}
                    >
                      {msg.role === "assistant" ? (
                        <div className="prose prose-sm dark:prose-invert max-w-none">
                          <ReactMarkdown
                            components={{
                              code: ({ className, children, ...props }) => {
                                const isInline = !className;
                                return isInline ? (
                                  <code
                                    className="bg-muted px-1 py-0.5 rounded text-xs"
                                    {...props}
                                  >
                                    {children}
                                  </code>
                                ) : (
                                  <pre className="bg-muted rounded-md p-3 overflow-x-auto text-xs">
                                    <code className={className} {...props}>
                                      {children}
                                    </code>
                                  </pre>
                                );
                              },
                              table: ({ children }) => (
                                <div className="overflow-x-auto">
                                  <table className="min-w-full text-xs border-collapse">
                                    {children}
                                  </table>
                                </div>
                              ),
                              th: ({ children }) => (
                                <th className="border border-border px-2 py-1 bg-muted font-medium text-left">
                                  {children}
                                </th>
                              ),
                              td: ({ children }) => (
                                <td className="border border-border px-2 py-1">
                                  {children}
                                </td>
                              ),
                            }}
                          >
                            {msg.content}
                          </ReactMarkdown>
                        </div>
                      ) : (
                        msg.content.split("\n").map((line, i) => (
                          <p key={i} className={i > 0 ? "mt-2" : ""}>
                            {line}
                          </p>
                        ))
                      )}
                    </div>
                  </div>
                </div>
              ))
            )}

            {isExecuting && (
              <div className="flex justify-start animate-fade-in">
                <div className="bg-card border rounded-2xl rounded-bl-sm px-4 py-3 flex items-center gap-2.5 shadow-soft">
                  <Loader2 className="h-4 w-4 animate-spin text-primary" />
                  <div className="flex items-center gap-2 text-xs text-muted-foreground font-medium">
                    {executionStage === "planning" && (
                      <>
                        <Target className="h-3 w-3" /> {t("assistant.planning")}
                      </>
                    )}
                    {executionStage === "grounding" && (
                      <>
                        <Zap className="h-3 w-3" /> {t("assistant.grounding")}
                      </>
                    )}
                    {executionStage === "response" && (
                      <>
                        <Brain className="h-3 w-3" />{" "}
                        {t("assistant.generating")}
                      </>
                    )}
                  </div>
                </div>
              </div>
            )}

            <div ref={messagesEndRef} />
          </div>

          {/* Composer */}
          <div
            className="border-t p-3"
            style={{
              background:
                "linear-gradient(180deg, hsl(var(--card)), hsl(var(--card)))",
            }}
          >
            <div className="flex items-end gap-2">
              <Textarea
                value={inputText}
                onChange={(e) => setInputText(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={t("assistant.askPlaceholder")}
                className="min-h-[44px] max-h-[120px] resize-none text-sm rounded-xl"
                rows={1}
              />
              <Button
                size="icon"
                className="shrink-0 rounded-xl h-10 w-10"
                onClick={handleSend}
                disabled={isExecuting || !inputText.trim()}
              >
                <Send className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </div>

        {/* Evidence panel */}
        {showEvidence && latestEvidence && (
          <div className="inspector-panel w-72 lg:w-80 shrink-0 hidden lg:block overflow-y-auto animate-slide-in-right">
            <div className="p-3 border-b">
              <h3 className="text-sm font-bold tracking-tight">
                {t("assistant.evidence")}
              </h3>
            </div>
            <div className="p-3 space-y-4">
              {(() => {
                const vc = verificationConfig[latestEvidence.verificationState];
                return (
                  <div
                    className="flex items-center gap-2.5 p-3.5 rounded-xl"
                    style={{
                      background:
                        latestEvidence.verificationState === "passed"
                          ? "hsl(var(--status-ready-bg))"
                          : "hsl(var(--status-warning-bg))",
                      boxShadow: `inset 0 0 0 1px ${latestEvidence.verificationState === "passed" ? "hsl(var(--status-ready-ring) / 0.3)" : "hsl(var(--status-warning-ring) / 0.3)"}`,
                    }}
                  >
                    <vc.icon className={`h-4 w-4 ${vc.cls}`} />
                    <span className="text-sm font-bold">{t(vc.labelKey)}</span>
                  </div>
                );
              })()}

              {latestEvidence.runtimeSummary && (
                <div>
                  <div className="section-label mb-2">
                    {t("assistant.runtime")}
                  </div>
                  <div className="grid grid-cols-2 gap-2 text-xs">
                    {[
                      {
                        label: t("assistant.segmentRefs"),
                        value: latestEvidence.runtimeSummary.totalSegments,
                      },
                      {
                        label: t("assistant.factRefs"),
                        value: latestEvidence.runtimeSummary.totalFacts,
                      },
                      {
                        label: t("assistant.entityRefs"),
                        value: latestEvidence.runtimeSummary.totalEntities,
                      },
                      {
                        label: t("assistant.relationRefs"),
                        value: latestEvidence.runtimeSummary.totalRelations,
                      },
                    ].map((m) => (
                      <div
                        key={m.label}
                        className="p-3 bg-surface-sunken rounded-xl"
                      >
                        <div className="text-muted-foreground text-[10px] font-bold uppercase tracking-wider">
                          {m.label}
                        </div>
                        <div className="font-bold text-base mt-1 tabular-nums">
                          {m.value}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {latestEvidence.segmentRefs.length > 0 && (
                <div>
                  <div className="section-label mb-2">
                    {t("assistant.segmentRefs")}
                  </div>
                  <div className="space-y-2">
                    {latestEvidence.segmentRefs.map((ref, i) => (
                      <div
                        key={i}
                        className="p-3.5 border rounded-xl text-xs bg-card shadow-soft"
                      >
                        <div className="flex items-center gap-1.5 font-bold">
                          <FileText className="h-3 w-3" />{" "}
                          {ref.documentTitle || ref.documentName}
                        </div>
                        {(ref.sourceAccess?.href || ref.sourceUri) && (
                          <a
                            href={
                              ref.sourceAccess?.href ?? ref.sourceUri ?? "#"
                            }
                            target="_blank"
                            rel="noopener noreferrer"
                            className="text-primary text-[10px] hover:underline truncate block mt-0.5"
                          >
                            {ref.sourceAccess?.kind === "stored_document"
                              ? t("assistant.openSourceDocument")
                              : (ref.sourceUri ??
                                t("assistant.openSourceLink"))}
                          </a>
                        )}
                        <p className="mt-1.5 text-muted-foreground line-clamp-2 leading-relaxed">
                          {ref.excerpt}
                        </p>
                        <div className="mt-1.5 text-muted-foreground">
                          {t("assistant.relevance")}:{" "}
                          <span className="font-bold text-foreground">
                            {ref.relevance > 100
                              ? Math.round(ref.relevance).toLocaleString()
                              : (ref.relevance * 100).toFixed(0) + "%"}
                          </span>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {latestEvidence.factRefs.length > 0 && (
                <div>
                  <div className="section-label mb-2">
                    {t("assistant.factRefs")}
                  </div>
                  <div className="space-y-2">
                    {latestEvidence.factRefs.map((ref, i) => (
                      <div
                        key={i}
                        className="p-3.5 border rounded-xl text-xs bg-card shadow-soft"
                      >
                        <div className="font-bold">{ref.value}</div>
                        <div className="text-muted-foreground mt-1">
                          {ref.factKind}
                          {ref.confidence > 0
                            ? ` · ${ref.confidence > 100 ? Math.round(ref.confidence).toLocaleString() : (ref.confidence * 100).toFixed(0) + "%"}`
                            : ""}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {latestEvidence.entityRefs.length > 0 && (
                <div>
                  <div className="section-label mb-2">
                    {t("assistant.entityRefs")}
                  </div>
                  <div className="space-y-1">
                    {latestEvidence.entityRefs.map((ref, i) => (
                      <button
                        key={i}
                        className="w-full flex items-center gap-2.5 p-3 border rounded-xl text-xs text-left hover:bg-accent/50 transition-all duration-200 bg-card shadow-soft"
                        onClick={() => navigate("/graph")}
                      >
                        <Share2 className="h-3 w-3 text-muted-foreground" />
                        <span className="font-bold">{ref.label}</span>
                        <span className="text-muted-foreground ml-auto">
                          {ref.type}
                        </span>
                      </button>
                    ))}
                  </div>
                </div>
              )}

              <div className="space-y-1.5 pt-2">
                <Button
                  variant="outline"
                  size="sm"
                  className="w-full justify-start"
                  onClick={() => navigate("/documents")}
                >
                  <FileText className="h-3.5 w-3.5 mr-2" />{" "}
                  {t("assistant.openDocuments")}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  className="w-full justify-start"
                  onClick={() => navigate("/graph")}
                >
                  <Share2 className="h-3.5 w-3.5 mr-2" />{" "}
                  {t("assistant.openGraph")}
                </Button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
