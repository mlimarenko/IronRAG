import { act, type ComponentProps, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '@/shared/i18n';

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
    activeOrderedList: false,
    activeBlockquote: false,
    activeHeadingLevel: 0,
    activeBold: false,
    activeItalic: false,
    activeLink: false,
    linkHref: '',
  };

  const editorChain = {
    focus: vi.fn(() => editorChain),
    toggleHeading: vi.fn(() => editorChain),
    toggleBold: vi.fn(() => editorChain),
    toggleItalic: vi.fn(() => editorChain),
    toggleBulletList: vi.fn(() => editorChain),
    toggleOrderedList: vi.fn(() => editorChain),
    toggleBlockquote: vi.fn(() => editorChain),
    toggleCodeBlock: vi.fn(() => editorChain),
    extendMarkRange: vi.fn(() => editorChain),
    insertContent: vi.fn(() => editorChain),
    setLink: vi.fn(() => editorChain),
    unsetLink: vi.fn(() => editorChain),
    setImage: vi.fn(() => editorChain),
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
    getAttributes: vi.fn((name: string) => {
      if (name === 'link') {
        return { href: editorState.linkHref };
      }
      return {};
    }),
    state: {
      selection: {
        empty: false,
      },
    },
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
      if (name === 'orderedList') {
        return editorState.activeOrderedList;
      }
      if (name === 'blockquote') {
        return editorState.activeBlockquote;
      }
      if (name === 'bold') {
        return editorState.activeBold;
      }
      if (name === 'italic') {
        return editorState.activeItalic;
      }
      if (name === 'link') {
        return editorState.activeLink;
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
    editorState.activeOrderedList = false;
    editorState.activeBlockquote = false;
    editorState.activeHeadingLevel = 0;
    editorState.activeBold = false;
    editorState.activeItalic = false;
    editorState.activeLink = false;
    editorState.linkHref = '';
    mockEditor.state.selection.empty = false;
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
          documentName={props?.documentName ?? 'employees.xlsx'}
          error={null}
          loading={false}
          markdown={props?.markdown ?? '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | AI |'}
          onOpenChange={props?.onOpenChange ?? vi.fn()}
          onSave={props?.onSave ?? vi.fn()}
          open={props?.open ?? true}
          readOnly={props?.readOnly ?? false}
          saving={props?.saving ?? false}
          sourceFormat={props?.sourceFormat ?? 'xlsx'}
          sourceHref={props?.sourceHref}
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

    const saveButtons = Array.from(container.querySelectorAll('button'));
    const saveButton = saveButtons[saveButtons.length - 1];
    expect(saveButton?.getAttribute('disabled')).not.toBeNull();
  });

  it('shows table-focused copy for spreadsheet documents', async () => {
    editorState.markdown = '## Employees\n\n| Name | Team |\n| --- | --- |\n| Elena | AI |';
    editorState.activeTable = true;

    await renderShell({ sourceFormat: 'xlsx' });

    expect(container.textContent).toContain('Table');
    expect(container.textContent).toContain('Scroll inside the table to reach hidden columns.');
    expect(container.querySelector('button[aria-label="Wrap text"]')).toBeTruthy();
    expect(container.querySelector('button[aria-label="Bold"]')).toBeTruthy();
    expect(container.querySelector('button[aria-label="Link"]')).toBeTruthy();
    expect(container.querySelector('button[aria-label="Row+"]')).toBeTruthy();
    expect(container.querySelector('button[aria-label="Col+"]')).toBeTruthy();
  });

  it('wires prose toolbar actions through canonical Tiptap commands', async () => {
    editorState.markdown = '# Notes\n\nParagraph';
    const promptSpy = vi.spyOn(window, 'prompt')
      .mockReturnValueOnce('https://example.test/reference')
      .mockReturnValueOnce('/assets/diagram.png');

    await renderShell({
      documentName: 'notes.md',
      markdown: '# Notes\n\nParagraph',
      sourceFormat: 'md',
    });

    const clickByLabel = async (label: string) => {
      const button = Array.from(container.querySelectorAll('button')).find(
        (candidate) => candidate.getAttribute('aria-label') === label,
      );
      expect(button, label).toBeTruthy();
      await act(async () => {
        button?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
    };

    await clickByLabel('Bold');
    await clickByLabel('Italic');
    await clickByLabel('Numbered list');
    await clickByLabel('Link');
    await clickByLabel('Image');

    expect(editorChain.toggleBold).toHaveBeenCalled();
    expect(editorChain.toggleItalic).toHaveBeenCalled();
    expect(editorChain.toggleOrderedList).toHaveBeenCalled();
    expect(editorChain.setLink).toHaveBeenCalledWith({ href: 'https://example.test/reference' });
    expect(editorChain.setImage).toHaveBeenCalledWith({ src: '/assets/diagram.png' });
    expect(promptSpy).toHaveBeenCalledTimes(2);

    promptSpy.mockRestore();
  });

  it('inserts a visible markdown link when no text is selected', async () => {
    editorState.markdown = '# Notes\n\nParagraph';
    mockEditor.state.selection.empty = true;
    const promptSpy = vi.spyOn(window, 'prompt').mockReturnValueOnce('https://example.test/reference');

    await renderShell({
      documentName: 'notes.md',
      markdown: '# Notes\n\nParagraph',
      sourceFormat: 'md',
    });

    const linkButton = Array.from(container.querySelectorAll('button')).find(
      (candidate) => candidate.getAttribute('aria-label') === 'Link',
    );
    expect(linkButton).toBeTruthy();

    await act(async () => {
      linkButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(editorChain.insertContent).toHaveBeenCalledWith({
      type: 'text',
      text: 'https://example.test/reference',
      marks: [
        {
          type: 'link',
          attrs: { href: 'https://example.test/reference' },
        },
      ],
    });
    expect(editorChain.setLink).not.toHaveBeenCalled();

    promptSpy.mockRestore();
    mockEditor.state.selection.empty = false;
  });

  it('can remove an existing markdown link from the toolbar', async () => {
    editorState.markdown = '[Reference](https://example.test/reference)';
    editorState.activeLink = true;
    editorState.linkHref = 'https://example.test/reference';

    await renderShell({
      documentName: 'notes.md',
      markdown: '[Reference](https://example.test/reference)',
      sourceFormat: 'md',
    });

    const unlinkButton = Array.from(container.querySelectorAll('button')).find(
      (candidate) => candidate.getAttribute('aria-label') === 'Remove link',
    );
    expect(unlinkButton).toBeTruthy();

    await act(async () => {
      unlinkButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(editorChain.unsetLink).toHaveBeenCalled();
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

  it('opens plain text source in the raw textarea instead of ProseMirror', async () => {
    const onSave = vi.fn();

    await renderShell({
      documentName: 'chat.txt',
      markdown: 'line 1\nline 2\n',
      onSave,
      sourceFormat: 'txt',
    });

    const textarea = container.querySelector('textarea');
    expect(textarea).toBeTruthy();
    expect(textarea?.value).toBe('line 1\nline 2\n');
    expect(getLatestEditorConfig()?.content).toBe('');
    expect(container.textContent).toContain('Text');
    expect(container.textContent).not.toContain('Code files keep a monospace workspace');

    await act(async () => {
      const valueSetter = Object.getOwnPropertyDescriptor(
        window.HTMLTextAreaElement.prototype,
        'value',
      )?.set;
      valueSetter?.call(textarea, 'line 1\nline 2\nline 3\n');
      textarea!.dispatchEvent(new Event('input', { bubbles: true }));
    });

    await flushUi();

    const buttons = Array.from(container.querySelectorAll('button'));
    const saveButton = buttons[buttons.length - 1];
    expect(saveButton).toBeTruthy();

    await act(async () => {
      saveButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(onSave).toHaveBeenCalledWith('line 1\nline 2\nline 3\n');
  });

  it('does not pass large code source into ProseMirror', async () => {
    const largeSource = `${'use uuid::Uuid;\n'.repeat(40000)}`;

    await renderShell({
      documentName: 'large.rs',
      markdown: largeSource,
      sourceFormat: 'rs',
    });

    expect(container.querySelector('textarea')?.value).toBe(largeSource);
    expect(getLatestEditorConfig()?.content).toBe('');
  });

  it('opens non-editable documents in a read-only viewer without save controls', async () => {
    editorState.markdown = '# Prepared PDF\n\nExtracted text.';

    await renderShell({
      documentName: 'guide.pdf',
      markdown: '# Prepared PDF\n\nExtracted text.',
      readOnly: true,
      sourceFormat: 'pdf',
    });

    expect(container.textContent).toContain('Document Viewer');
    expect(container.textContent).toContain('Read-only');
    expect(container.textContent).not.toContain('Save And Reprocess');
    expect(container.textContent).not.toContain('Bullets');
    expect(getLatestEditorConfig()?.editable).toBe(false);
    expect(mockEditor.setEditable).toHaveBeenCalledWith(false);
  });

  it('keeps line wrapping enabled by default and lets the toolbar disable it', async () => {
    await renderShell({
      documentName: 'notes.txt',
      markdown: 'A very long line should wrap by default.',
      sourceFormat: 'txt',
    });

    const textarea = container.querySelector('textarea');
    expect(textarea?.className).toContain('document-editor-raw-textarea--wrap');

    const wrapButton = Array.from(container.querySelectorAll('button')).find(
      (candidate) => candidate.getAttribute('aria-label') === 'Wrap text',
    );
    expect(wrapButton).toBeTruthy();

    await act(async () => {
      wrapButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(container.querySelector('textarea')?.className).not.toContain('document-editor-raw-textarea--wrap');
  });

  it('uses viewport-width table content while line wrapping is enabled', async () => {
    editorState.markdown = [
      '# Readme',
      '',
      '| Badge | Description |',
      '| --- | --- |',
      '| Stars | IronRAG pipeline from messy data to structured knowledge |',
    ].join('\n');

    await renderShell({
      documentName: 'README.md',
      markdown: editorState.markdown,
      sourceFormat: 'md',
    });

    const tableContent = container.querySelector('[data-testid="document-editor-table-content"]');
    expect(tableContent?.className).toContain('w-full');
    expect(tableContent?.className).not.toContain('w-max');

    const wrapButton = Array.from(container.querySelectorAll('button')).find(
      (candidate) => candidate.getAttribute('aria-label') === 'Wrap text',
    );
    expect(wrapButton).toBeTruthy();

    await act(async () => {
      wrapButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(container.querySelector('[data-testid="document-editor-table-content"]')?.className).toContain('w-max');
  });

  it('uses the stored source as the read-only PDF viewer frame', async () => {
    await renderShell({
      documentName: 'guide.pdf',
      markdown: '',
      readOnly: true,
      sourceFormat: 'pdf',
      sourceHref: '/v1/content/documents/doc-1/source',
    });

    const iframe = container.querySelector('iframe');
    expect(iframe).toBeTruthy();
    expect(iframe?.getAttribute('src')).toBe('/v1/content/documents/doc-1/source');
    expect(iframe?.getAttribute('title')).toBe('guide.pdf');
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
