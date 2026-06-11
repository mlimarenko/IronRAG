import { lazy, Suspense, type ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter, Route, Routes, Navigate } from "react-router-dom";
import { Toaster as Sonner } from "@/shared/components/ui/sonner";
import { Toaster } from "@/shared/components/ui/toaster";
import { TooltipProvider } from "@/shared/components/ui/tooltip";
import { FeatureErrorBoundary } from "@/shared/components/FeatureErrorBoundary";
import { AppProvider } from "@/shared/contexts/AppContext";
import { PreferencesProvider } from "@/shared/contexts/PreferencesProvider";
import { useApp } from "@/shared/contexts/app-context";
import { AppShell } from "@/app/components/AppShell";
import { useTranslation } from "react-i18next";
import LoginPage from "@/features/auth/LoginPage";
import DashboardPage from "@/features/dashboard/DashboardPage";

// Lazy-load every non-landing route so the initial bundle drops by the
// weight of admin (Radix-heavy), graph (Sigma + Graphology, ~190 KB
// gzipped), assistant (Tiptap markdown surface), and the Swagger UI
// runtime (≈1 MB on its own). Login + Dashboard stay eager because they
// are reached on first paint or right after auth.
const DocumentsPage = lazy(() => import("@/features/documents/DocumentsPage"));
const GraphPage = lazy(() => import("@/features/graph/GraphPage"));
const AssistantPage = lazy(() => import("@/features/assistant/AssistantPage"));
const AdminPage = lazy(() => import("@/features/admin/AdminPage"));
const SwaggerPage = lazy(() => import("@/features/swagger/SwaggerPage"));
const NotFoundPage = lazy(() => import("@/app/NotFoundPage"));
const ReactQueryDevtools =
  import.meta.env.DEV === true
    ? lazy(async () => {
        const { ReactQueryDevtools } = await import("@tanstack/react-query-devtools");
        return { default: ReactQueryDevtools };
      })
    : null;

// Shared QueryClient. Defaults tuned for an internal back-office app:
//   - staleTime 30s: small but non-zero so a remount inside the same screen
//     reuses the cached payload without a flicker.
//   - refetchOnWindowFocus disabled: noisy and rarely useful for the kinds of
//     surfaces IronRAG renders (long-lived dashboards, document lists). Pages
//     that do need it can opt in per-query.
//   - retry once: the API is on the same origin behind an internal proxy, so
//     three retries adds latency without buying reliability.
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      gcTime: 5 * 60_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
    mutations: {
      retry: 0,
    },
  },
});

function RouteSuspenseFallback() {
  const { t } = useTranslation();
  return (
    <div className="flex items-center justify-center h-screen">
      {t("common.loading")}
    </div>
  );
}

function FeatureRoute({
  feature,
  children,
}: {
  feature: string;
  children: ReactNode;
}) {
  return <FeatureErrorBoundary feature={feature}>{children}</FeatureErrorBoundary>;
}

function LazyFeatureRoute({
  feature,
  children,
}: {
  feature: string;
  children: ReactNode;
}) {
  return (
    <FeatureRoute feature={feature}>
      <Suspense fallback={<RouteSuspenseFallback />}>{children}</Suspense>
    </FeatureRoute>
  );
}

function QueryDevtools() {
  if (ReactQueryDevtools === null) return null;
  return (
    <Suspense fallback={null}>
      <ReactQueryDevtools initialIsOpen={false} />
    </Suspense>
  );
}

function AuthenticatedRoutes() {
  const { t } = useTranslation();
  const { isAuthenticated, isLoading } = useApp();
  if (isLoading) {
    return <div className="flex items-center justify-center h-screen">{t("common.loading")}</div>;
  }
  if (!isAuthenticated) return <Navigate to="/login" replace />;
  return (
    <AppShell>
      <Routes>
        <Route
          path="/dashboard"
          element={
            <FeatureRoute feature="dashboard">
              <DashboardPage />
            </FeatureRoute>
          }
        />
        <Route
          path="/documents"
          element={
            <LazyFeatureRoute feature="documents">
              <DocumentsPage />
            </LazyFeatureRoute>
          }
        />
        <Route
          path="/graph"
          element={
            <LazyFeatureRoute feature="graph">
              <GraphPage />
            </LazyFeatureRoute>
          }
        />
        <Route
          path="/assistant"
          element={
            <LazyFeatureRoute feature="assistant">
              <AssistantPage />
            </LazyFeatureRoute>
          }
        />
        <Route
          path="/admin/*"
          element={
            <LazyFeatureRoute feature="admin">
              <AdminPage />
            </LazyFeatureRoute>
          }
        />
        <Route
          path="/swagger"
          element={
            <LazyFeatureRoute feature="swagger">
              <SwaggerPage />
            </LazyFeatureRoute>
          }
        />
        <Route path="/" element={<Navigate to="/dashboard" replace />} />
        <Route
          path="*"
          element={
            <LazyFeatureRoute feature="not-found">
              <NotFoundPage />
            </LazyFeatureRoute>
          }
        />
      </Routes>
    </AppShell>
  );
}

const App = () => (
  <QueryClientProvider client={queryClient}>
    <TooltipProvider>
      <PreferencesProvider>
      <AppProvider>
        <Toaster />
        <Sonner />
        <BrowserRouter>
          <Routes>
            <Route
              path="/login"
              element={
                <FeatureRoute feature="auth">
                  <LoginPage />
                </FeatureRoute>
              }
            />
            <Route path="/*" element={<AuthenticatedRoutes />} />
          </Routes>
        </BrowserRouter>
      </AppProvider>
      </PreferencesProvider>
    </TooltipProvider>
    <QueryDevtools />
  </QueryClientProvider>
);

export default App;
