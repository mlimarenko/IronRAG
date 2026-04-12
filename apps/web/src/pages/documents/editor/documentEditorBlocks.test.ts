import { describe, expect, it } from "vitest";

import {
  buildEditorBlocks,
  serializeSourceTextForEditor,
  serializeEditorBlocks,
} from "./documentEditorBlocks";

describe("documentEditorBlocks", () => {
  it("hydrates spreadsheet prepared segments into heading and table blocks", () => {
    const blocks = buildEditorBlocks(
      [
        {
          segment: {
            ordinal: 0,
            blockKind: "heading",
            headingTrail: ["Sheet1"],
          },
          text: "## Sheet1",
        },
        {
          segment: {
            ordinal: 1,
            blockKind: "table",
          },
          text: "| Item | Quantity |\n| --- | --- |\n| Widget | 7 |",
        },
      ],
      "xlsx",
    );

    expect(blocks).toEqual([
      { kind: "heading", level: 2, text: "Sheet1" },
      {
        kind: "table",
        rows: [
          ["Item", "Quantity"],
          ["Widget", "7"],
        ],
        sheetName: "Sheet1",
      },
    ]);
  });

  it("keeps sheet names for ods tables too", () => {
    const blocks = buildEditorBlocks(
      [
        {
          segment: {
            ordinal: 0,
            blockKind: "heading",
            headingTrail: ["Sheet1"],
          },
          text: "## Sheet1",
        },
        {
          segment: {
            ordinal: 1,
            blockKind: "table",
          },
          text: "| Item | Quantity |\n| --- | --- |\n| Widget | 7 |",
        },
      ],
      "ods",
    );

    expect(blocks[1]).toMatchObject({
      kind: "table",
      sheetName: "Sheet1",
    });
  });

  it("hydrates table_row segments that use semantic normalized text but raw markdown text", () => {
    const blocks = buildEditorBlocks(
      [
        {
          segment: {
            ordinal: 0,
            blockKind: "heading",
            headingTrail: ["people"],
          },
          text: "## people",
        },
        {
          segment: {
            ordinal: 1,
            blockKind: "table",
          },
          text: "| Name | Email |\n| --- | --- |\n| Alice | alice@example.com |",
        },
        {
          segment: {
            ordinal: 2,
            blockKind: "table_row",
          },
          text: "| Alice | alice@example.com |",
          normalizedText:
            "Sheet: people | Row 1 | Name: Alice | Email: alice@example.com",
        },
      ],
      "csv",
    );

    expect(blocks[1]).toEqual({
      kind: "table",
      rows: [
        ["Name", "Email"],
        ["Alice", "alice@example.com"],
      ],
      sheetName: "people",
    });
  });

  it("serializes canonical blocks back into markdown", () => {
    const markdown = serializeEditorBlocks([
      { kind: "heading", level: 2, text: "Sheet1" },
      { kind: "list_item", text: "First row changed" },
      {
        kind: "table",
        rows: [
          ["Item", "Quantity"],
          ["Widget", "9"],
        ],
      },
    ]);

    expect(markdown).toBe(
      "## Sheet1\n\n- First row changed\n\n| Item | Quantity |\n| --- | --- |\n| Widget | 9 |",
    );
  });

  it("hydrates code-like source formats into one code block", () => {
    const blocks = buildEditorBlocks(
      [
        {
          segment: {
            ordinal: 0,
            blockKind: "paragraph",
          },
          text: "use uuid::Uuid;",
        },
        {
          segment: {
            ordinal: 1,
            blockKind: "paragraph",
          },
          text: "pub struct Node {",
        },
        {
          segment: {
            ordinal: 2,
            blockKind: "paragraph",
          },
          text: "  pub id: Uuid,",
        },
        {
          segment: {
            ordinal: 3,
            blockKind: "paragraph",
          },
          text: "}",
        },
      ],
      "rs",
    );

    expect(blocks).toEqual([
      {
        kind: "code_block",
        language: "rust",
        text: "use uuid::Uuid;\npub struct Node {\n  pub id: Uuid,\n}",
      },
    ]);
  });

  it("preserves leading tabs in code-like source formats", () => {
    const blocks = buildEditorBlocks(
      [
        {
          segment: {
            ordinal: 0,
            blockKind: "paragraph",
          },
          text: "\tif (user == null)",
        },
        {
          segment: {
            ordinal: 1,
            blockKind: "paragraph",
          },
          text: "\t\treturn false;",
        },
      ],
      "cs",
    );

    expect(blocks).toEqual([
      {
        kind: "code_block",
        language: "cs",
        text: "\tif (user == null)\n\t\treturn false;",
      },
    ]);
  });

  it("serializes raw code source into one fenced block without losing leading spaces", () => {
    expect(
      serializeSourceTextForEditor("def run():\n    return 42\n", "py"),
    ).toBe("```python\ndef run():\n    return 42\n\n```");
  });
});
