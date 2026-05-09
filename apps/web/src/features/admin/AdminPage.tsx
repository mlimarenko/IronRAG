import { useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import { Activity, Brain, DollarSign, Key, Settings, Terminal } from 'lucide-react';
import { useApp } from '@/shared/contexts/app-context';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/shared/components/ui/tabs';
import AiConfigurationPanel from '@/features/admin/components/AiConfigurationPanel';
import { AccessTab } from '@/features/admin/components/AccessTab';
import { McpTab } from '@/features/admin/components/McpTab';
import { OperationsTab } from '@/features/admin/components/OperationsTab';
import { PricingTab } from '@/features/admin/components/PricingTab';
import { SettingsTab } from '@/features/admin/components/SettingsTab';

const TAB_VALUES = ['access', 'mcp', 'operations', 'ai', 'pricing', 'settings'] as const;
type AdminTab = (typeof TAB_VALUES)[number];

function parseTab(value: string | null): AdminTab {
  return TAB_VALUES.includes(value as AdminTab) ? (value as AdminTab) : 'access';
}

export default function AdminPage() {
  const { t } = useTranslation();
  const [searchParams, setSearchParams] = useSearchParams();
  const { activeWorkspace, activeLibrary, locale, setLocale } = useApp();

  const activeTab = parseTab(searchParams.get('tab'));

  const handleTabChange = useCallback(
    (nextTab: string) => {
      const parsed = parseTab(nextTab);
      const nextParams = new URLSearchParams(searchParams);
      nextParams.set('tab', parsed);
      setSearchParams(nextParams, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  const tabDescriptors = [
    { value: 'access' as const, label: t('admin.access'), icon: Key },
    { value: 'mcp' as const, label: t('admin.mcp'), icon: Terminal },
    { value: 'operations' as const, label: t('admin.operations'), icon: Activity },
    { value: 'ai' as const, label: t('admin.ai'), icon: Brain },
    { value: 'pricing' as const, label: t('admin.pricing'), icon: DollarSign },
    { value: 'settings' as const, label: t('admin.settings'), icon: Settings },
  ];

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="page-header">
        <h1 className="text-lg font-bold tracking-tight">{t('admin.title')}</h1>
        <p className="text-sm text-muted-foreground">
          {activeWorkspace?.name}
          {activeLibrary ? (
            <>
              <span className="mx-2 text-border">&middot;</span>
              {activeLibrary.name}
            </>
          ) : (
            ''
          )}
        </p>
      </div>

      <Tabs
        value={activeTab}
        onValueChange={handleTabChange}
        className="flex-1 flex flex-col overflow-hidden"
      >
        <div
          className="border-b px-6"
          style={{ background: 'linear-gradient(180deg, hsl(var(--card) / 0.8), transparent)' }}
        >
          <TabsList className="bg-transparent h-auto p-0 gap-0">
            {tabDescriptors.map((tab) => (
              <TabsTrigger
                key={tab.value}
                value={tab.value}
                className="rounded-none border-b-2 border-transparent data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:shadow-none px-4 py-3 gap-1.5 font-semibold text-sm transition-all duration-200"
              >
                <tab.icon className="h-3.5 w-3.5" /> {tab.label}
              </TabsTrigger>
            ))}
          </TabsList>
        </div>

        <div className="flex flex-1 min-h-0 flex-col">
          <TabsContent value="access" className="mt-0 flex-1 min-h-0 overflow-auto p-6 animate-fade-in">
            <AccessTab
              t={t}
              activeWorkspaceId={activeWorkspace?.id}
              active={activeTab === 'access'}
            />
          </TabsContent>

          <TabsContent value="mcp" className="mt-0 flex-1 min-h-0 overflow-auto p-6 animate-fade-in">
            <McpTab t={t} activeLibraryId={activeLibrary?.id} active={activeTab === 'mcp'} />
          </TabsContent>

          <TabsContent value="operations" className="mt-0 flex-1 min-h-0 overflow-auto p-6 animate-fade-in">
            <OperationsTab
              t={t}
              activeWorkspaceId={activeWorkspace?.id}
              activeLibraryId={activeLibrary?.id}
              active={activeTab === 'operations'}
            />
          </TabsContent>

          <TabsContent value="ai" className="mt-0 flex-1 min-h-0 flex flex-col p-6 animate-fade-in data-[state=inactive]:hidden">
            <AiConfigurationPanel active={activeTab === 'ai'} />
          </TabsContent>

          <TabsContent value="pricing" className="mt-0 flex-1 min-h-0 flex flex-col p-6 animate-fade-in data-[state=inactive]:hidden">
            <PricingTab
              t={t}
              activeWorkspaceId={activeWorkspace?.id}
              active={activeTab === 'pricing'}
            />
          </TabsContent>

          <TabsContent value="settings" className="mt-0 flex-1 min-h-0 overflow-auto p-6 animate-fade-in">
            <SettingsTab t={t} locale={locale} setLocale={setLocale} />
          </TabsContent>
        </div>
      </Tabs>
    </div>
  );
}
