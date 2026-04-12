import { act, type ComponentProps, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '@/i18n';

import { DocumentEditorShell } from './DocumentEditorShell';

const {
  editorState,
  editorChain,
  mockEditor,
  useEditorMock,
  getLatestEditorConfig,
} = vi.hoisted(() => {
  const editorState = {
    markdown: '',
    activeTable: false,
    activeCodeBlock: false,
    activeBulletList: false,
    activeBlockquote: false,
    activeHeadingLevel: 0,
  };

  const editorChain = {
    focus: vi.fn(() => editorChain),
    toggleHeading: vi.fn(() => editorChain),
    toggleBulletList: vi.fn(() => editorChain),
    toggleBlockquote: vi.fn(() => editorChain),
    toggleCodeBlock: vi.fn(() => editorChain),
    insertTable: vi.fn(() => editorChain),
    addRowAfter: vi.fn(() => editorChain),
    addColumnAfter: vi.fn(() => editorChain),
    undo: vi.fn(() => editorChain),
    redo: vi.fn(() => editorChain),
    run: vi.fn(() => true),
  };

  let latestEditorConfig: Record<string, unknown> | null = null;

  const mockEditor = {
    getMarkdown: vi.fn(() => editorState.markdown),
    setEditable: vi.fn(),
    commands: {
      focus: vi.fn(),
    },
    chain: vi.fn(() => editorChain),
    isActive: vi.fn((name: string, attrs?: { level?: number }) => {
      if (name === 'table') {
        return editorState.activeTable;
      }
      if (name === 'codeBlock') {
        return editorState.activeCodeBlock;
      }
      if (name === 'bulletList') {
        return editorState.activeBulletList;
      }
      if (name === 'blockquote') {
        return editorState.activeBlockquote;
      }
      if (name === 'heading') {
        return editorState.activeHeadingLevel === (attrs?.level ?? 0);
      }
      return false;
    }),
  };

  const useEditorMock = vi.fn((config: Record<string, unknown>) => {
    latestEditorConfig = config;
    return mockEditor;
  });

  return {
    editorState,
    editorChain,
    mockEditor,
    useEditorMock,
    getLatestEditorConfig: () => latestEditorConfig,
  };
});

vi.mock('@tiptap/react', () => ({
  useEditor: useEditorMock,
  EditorContent: () => <div data-testid="editor-content" />,
}));

vi.mock('./DocumentEditorOverlay', () => ({
  DocumentEditorOverlay: ({
    actions,
    children,
    description,
    title,
  }: {
    actions: ReactNode;
    children: ReactNode;
    description: string;
    title: string;
  }) => (
    <div data-testid="document-editor-overlay">
      <div>{title}</div>
      <div>{description}</div>
      {children}
      {actions}
    </div>
  ),
}));

describe('DocumentEditorShell', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    editorState.markdown = '';
    editorState.activeTable = false;
    editorState.activeCodeBlock = false;
    editorState.activeBulletList = false;
    editorState.activeBlockquote = false;
    editorState.activeHeadingLevel = 0;
    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    container.remove();
  });

  async function flushUi() {
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 0));
    });
  }

  async function renderShell(props?: Partial<ComponentProps<typeof DocumentEditorShell>>) {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <DocumentEditorShell
          documentName="employees.xlsx"
          error={null}
          loading={false}
          markdown={props?.markdown ?? '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | AI |'}
          onOpenChange={props?.onOpenChange ?? vi.fn()}
          onSave={props?.onSave ?? vi.fn()}
          open={props?.open ?? true}
          saving={props?.saving ?? false}
          sourceFormat={props?.sourceFormat ?? 'xlsx'}
          t={i18n.t.bind(i18n)}
        />,
      );
    });

    await flushUi();
    await flushUi();
  }

  it('stays clean immediately after opening when editor serialization matches the loaded content', async () => {
    editorState.markdown = '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | AI |';

    await renderShell({
      markdown: '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | AI |\n',
      sourceFormat: 'xlsx',
    });

    expect(container.textContent).toContain('All changes saved');
    expect(container.textContent).not.toContain('Unsaved changes');
  });

  it('shows table-focused copy for spreadsheet documents', async () => {
    editorState.markdown = '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | AI |';
    editorState.activeTable = true;

    await renderShell({ sourceFormat: 'xlsx' });

    expect(container.textContent).toContain('Table');
    expect(container.textContent).toContain('Scroll inside the table to reach hidden columns.');
    expect(container.textContent).toContain('Row+');
    expect(container.textContent).toContain('Col+');
  });

  it('shows code-focused copy for code documents', async () => {
    editorState.markdown = '```rust\nuse uuid::Uuid;\n```';
    editorState.activeCodeBlock = true;

    await renderShell({
      documentName: 'graph_store.rs',
      markdown: '```rust\nuse uuid::Uuid;\n```',
      sourceFormat: 'rs',
    });

    expect(container.textContent).toContain('Code');
    expect(container.textContent).toContain('Code files keep a monospace workspace with scrollable long lines.');
    expect(container.textContent).toContain('Lines: 1');
    expect(container.textContent).toContain('Tabs preserved · Tab size 4');
  });

  it('becomes dirty only after a real content update', async () => {
    editorState.markdown = '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | AI |';

    await renderShell({ sourceFormat: 'xlsx' });

    editorState.markdown = '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | Platform |';

    await act(async () => {
      const latestEditorConfig = getLatestEditorConfig() as {
        onUpdate?: ({ editor }: { editor: typeof mockEditor }) => void;
      } | null;
      latestEditorConfig?.onUpdate?.({ editor: mockEditor });
    });

    await flushUi();

    expect(container.textContent).toContain('Unsaved changes');
  });
});
