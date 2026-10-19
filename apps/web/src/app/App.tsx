import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { lazy, Suspense } from 'react'
import { BrowserRouter, Navigate, Route, Routes, useLocation } from 'react-router-dom'
import { Toaster } from 'sonner'
import { AppProvider } from '@/shared/contexts/AppContext'
import { useApp } from '@/shared/contexts/app-context'
import { ScreenState } from '@/shared/ui'
import { AppShell } from './AppShell'
import { returnToFromLocation } from './returnTo'
import { PreferencesProvider } from './PreferencesContext'
import LoginPage from '@/features/auth/LoginPage'
import DashboardPage from '@/features/dashboard/DashboardPage'

const DocumentsPage = lazy(() => import('@/features/documents/DocumentsPage'))
const GraphPage = lazy(() => import('@/features/graph/GraphPage'))
const AssistantPage = lazy(() => import('@/features/assistant/AssistantPage'))
const AdminLayout = lazy(() => import('@/features/admin/AdminLayout'))
const NotFoundPage = lazy(() => import('./NotFoundPage'))

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 30_000, retry: 1, refetchOnWindowFocus: false },
    mutations: { retry: 0 },
  },
})

function AuthenticatedRoutes() {
  const { user, isLoading, isBootstrapRequired, isBootstrapMode } = useApp()
  const location = useLocation()

  if (isLoading) {
    return (
      <ScreenState scenario="loading">
        <span />
      </ScreenState>
    )
  }
  if (!user || isBootstrapRequired || isBootstrapMode) {
    // Carry the whole address, not just the path: a session that dies on a
    // filtered list must come back to that same filtered list.
    return (
      <Navigate
        to="/login"
        replace
        state={{ from: `${location.pathname}${location.search}${location.hash}` }}
      />
    )
  }
  return (
    <AppShell>
      <Suspense
        fallback={
          <ScreenState scenario="loading">
            <span />
          </ScreenState>
        }
      >
        <Routes>
          <Route path="dashboard" element={<DashboardPage />} />
          <Route path="documents" element={<DocumentsPage />} />
          <Route path="graph" element={<GraphPage />} />
          <Route path="assistant" element={<AssistantPage />} />
          <Route path="admin/*" element={<AdminLayout />} />
          <Route path="" element={<Navigate to="dashboard" replace />} />
          <Route path="*" element={<NotFoundPage />} />
        </Routes>
      </Suspense>
    </AppShell>
  )
}

// The one authority on where an authenticated visitor to /login goes. Sign-in,
// bootstrap setup and an already-live session all land here, so there is a
// single place that honours the page the user was originally heading for.
function LoginRoute() {
  const { user, isLoading, isBootstrapRequired, isBootstrapMode } = useApp()
  const location = useLocation()
  if (!isLoading && user && !isBootstrapRequired && !isBootstrapMode) {
    return <Navigate to={returnToFromLocation(location)} replace />
  }
  return <LoginPage />
}

function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<LoginRoute />} />
      <Route path="/*" element={<AuthenticatedRoutes />} />
    </Routes>
  )
}

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <AppProvider>
        <PreferencesProvider>
          <BrowserRouter>
            <AppRoutes />
            <Toaster richColors position="bottom-right" />
          </BrowserRouter>
        </PreferencesProvider>
      </AppProvider>
    </QueryClientProvider>
  )
}
