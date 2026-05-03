import type { PreparedSegmentItem } from "@/api/documents";

import {
  codeLanguageForSourceFormat,
  isCodeLikeSourceFormat,
  isTableLikeSourceFormat,
} from "./editorSurfaceMode";

export type DocumentEditorBlockKind =
  | "heading"
  | "paragraph"
  | "list_item"
  | "code_block"
  | "quote_block"
  | "metadata_block"
  | "source_unit"
  | "table";

type BaseBlock = {
  kind: DocumentEditorBlockKind;
};

export type DocumentEditorBlock =
  | (BaseBlock & { kind: "heading"; level: number; text: string })
  | (BaseBlock & { kind: "paragraph"; text: string })
  | (BaseBlock & { kind: "list_item"; text: string })
  | (BaseBlock & { kind: "code_block"; text: string; language?: string })
  | (BaseBlock & { kind: "quote_block"; text: string })
  | (BaseBlock & { kind: "metadata_block"; text: string })
  | (BaseBlock & { kind: "source_unit"; text: string })
  | (BaseBlock & { kind: "table"; rows: string[][]; sheetName?: string });

type NormalizedSegment = {
  ordinal: number;
  blockKind: string;
  headingTrail: string[];
  text: string;
  parentBlockId: string | null;
  tableRowIndex: number | null;
  codeLanguage: string | null;
};

export function buildEditorBlocks(
  items: PreparedSegmentItem[],
  sourceFormat?: string,
): DocumentEditorBlock[] {
  const normalized = items.map(normalizeSegment);

  if (isCodeLikeSourceFormat(sourceFormat)) {
    const codeLines = normalized
      .map((segment) => segment.text)
      .filter((text) => text.trim().length > 0);

    return codeLines.length > 0
      ? [
          {
            kind: "code_block",
            text: codeLines.join("\n"),
            language: codeLanguageForSourceFormat(sourceFormat),
          },
        ]
      : [];
  }

  const rowSegmentsByParent = new Map<string, NormalizedSegment[]>();

  for (const segment of normalized) {
    if (segment.blockKind !== "table_row" || !segment.parentBlockId) {
      continue;
    }
    const bucket = rowSegmentsByParent.get(segment.parentBlockId) ?? [];
    bucket.push(segment);
    rowSegmentsByParent.set(segment.parentBlockId, bucket);
  }

  const blocks: DocumentEditorBlock[] = [];
  let currentHeading: string | undefined;

  for (const segment of normalized) {
    const blockKind = normalizeBlockKind(segment.blockKind);
    if (!blockKind || blockKind === "table_row") {
      continue;
    }

    if (blockKind === "heading") {
      const heading = parseHeading(segment.text, segment.headingTrail);
      currentHeading = heading.text;
      blocks.push(heading);
      continue;
    }

    if (blockKind === "table") {
      const rows = buildTableRows(
        segment,
        rowSegmentsByParent.get(segment.parentBlockId ?? "") ?? [],
        normalized,
      );
      if (rows.length > 0) {
        blocks.push({
          kind: "table",
          rows,
          sheetName: isTableLikeSourceFormat(sourceFormat)
            ? currentHeading
            : undefined,
        });
      }
      continue;
    }

    blocks.push(buildScalarBlock(blockKind, segment));
  }

  return blocks;
}

export function serializeEditorBlocks(blocks: DocumentEditorBlock[]): string {
  const rendered: string[] = [];

  for (let index = 0; index < blocks.length; index += 1) {
    const block = blocks[index];
    if (block.kind === "list_item") {
      const items = [block];
      while (
        index + 1 < blocks.length &&
        blocks[index + 1]?.kind === "list_item"
      ) {
        index += 1;
        items.push(
          blocks[index] as Extract<DocumentEditorBlock, { kind: "list_item" }>,
        );
      }
      rendered.push(items.map((item) => `- ${item.text}`.trimEnd()).join("\n"));
      continue;
    }

    rendered.push(renderBlock(block));
  }

  return rendered.filter(Boolean).join("\n\n");
}

export function serializeSourceTextForEditor(
  sourceText: string,
  sourceFormat?: string,
): string {
  const normalized = sourceText.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");
  if (!isCodeLikeSourceFormat(sourceFormat)) {
    return normalized;
  }

  return renderBlock({
    kind: "code_block",
    text: normalized,
    language: codeLanguageForSourceFormat(sourceFormat),
  });
}

function normalizeSegment(item: PreparedSegmentItem): NormalizedSegment {
  const segment = item.segment ?? {};
  const tableCoordinates =
    item.tableCoordinates ?? item.table_coordinates ?? null;

  return {
    ordinal: readNumber(segment.ordinal) ?? 0,
    blockKind:
      readString(segment.blockKind ?? segment.block_kind) ?? "paragraph",
    headingTrail: readStringArray(
      segment.headingTrail ?? segment.heading_trail,
    ),
    text:
      readString(
        item.text ??
          item.content ??
          item.normalizedText ??
          item.normalized_text ??
          item.content,
      ) ?? "",
    parentBlockId: readString(item.parentBlockId ?? item.parent_block_id),
    tableRowIndex:
      readNumber(tableCoordinates?.rowIndex ?? tableCoordinates?.row_index) ??
      null,
    codeLanguage: readString(item.codeLanguage ?? item.code_language),
  };
}

function normalizeBlockKind(
  blockKind: string,
): DocumentEditorBlockKind | "table_row" | null {
  switch (blockKind) {
    case "heading":
    case "paragraph":
    case "list_item":
    case "code_block":
    case "quote_block":
    case "metadata_block":
    case "source_unit":
    case "table":
    case "table_row":
      return blockKind;
    case "endpoint_block":
      return "metadata_block";
    default:
      return "paragraph";
  }
}

function parseHeading(
  text: string,
  headingTrail: string[],
): Extract<DocumentEditorBlock, { kind: "heading" }> {
  const match = text.match(/^(#{1,6})\s+(.*)$/);
  if (match) {
    return { kind: "heading", level: match[1].length, text: match[2].trim() };
  }

  return {
    kind: "heading",
    level: Math.min(Math.max(headingTrail.length, 1), 6),
    text: text.trim(),
  };
}

function buildScalarBlock(
  kind: Exclude<DocumentEditorBlockKind, "heading" | "table">,
  segment: NormalizedSegment,
): DocumentEditorBlock {
  switch (kind) {
    case "list_item":
      return { kind, text: stripListMarker(segment.text) };
    case "code_block":
      return {
        kind,
        text: stripCodeFence(segment.text),
        language: segment.codeLanguage ?? undefined,
      };
    case "quote_block":
      return { kind, text: stripQuoteMarkers(segment.text) };
    case "metadata_block":
    case "source_unit":
      return { kind, text: segment.text.trim() };
    case "paragraph":
    default:
      return { kind: "paragraph", text: segment.text.trim() };
  }
}

function buildTableRows(
  tableSegment: NormalizedSegment,
  tableRowSegments: NormalizedSegment[],
  normalized: NormalizedSegment[],
): string[][] {
  const parentId = findTableParentId(tableSegment, normalized);
  const candidateRows = (
    parentId
      ? normalized.filter(
          (segment) =>
            segment.parentBlockId === parentId &&
            segment.blockKind === "table_row",
        )
      : tableRowSegments
  )
    .slice()
    .sort(
      (left, right) =>
        (left.tableRowIndex ?? left.ordinal) -
        (right.tableRowIndex ?? right.ordinal),
    );

  const parsedRows = candidateRows
    .map((segment) => parseMarkdownTableRow(segment.text))
    .filter((cells) => cells.length > 0 && !isMarkdownSeparatorRow(cells));

  const tableRows = tableSegment.text
    .split(/\r?\n/)
    .map((line) => parseMarkdownTableRow(line))
    .filter((cells) => cells.length > 0 && !isMarkdownSeparatorRow(cells));

  if (parsedRows.length > 0) {
    const header = tableRows[0];
    return header ? [header, ...parsedRows] : parsedRows;
  }

  return tableRows;
}

function findTableParentId(
  tableSegment: NormalizedSegment,
  normalized: NormalizedSegment[],
): string | null {
  const match = normalized.find(
    (segment) =>
      segment.blockKind === "table" &&
      segment.ordinal === tableSegment.ordinal &&
      segment.text === tableSegment.text,
  );
  return match?.parentBlockId ?? tableSegment.parentBlockId;
}

function parseMarkdownTableRow(rowText: string): string[] {
  const trimmed = rowText.trim();
  if (!trimmed.includes("|")) {
    return [];
  }

  return trimmed
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split(/(?<!\\)\|/g)
    .map((cell) =>
      cell.trim().replace(/\\\|/g, "|").replace(/<br\s*\/?>/gi, "\n"),
    );
}

function isMarkdownSeparatorRow(cells: string[]): boolean {
  return cells.every((cell) => /^:?-{3,}:?$/.test(cell.trim()));
}

function stripListMarker(text: string): string {
  return text.replace(/^(\s*([-*+]\s+|\d+\.\s+))/, "").trim();
}

function stripCodeFence(text: string): string {
  const withoutStartFence = text.replace(/^```[^\n]*\n?/, "");
  const withoutEndFence = withoutStartFence.replace(/\n?```$/, "");
  return withoutEndFence.replace(/^\n+|\n+$/g, "");
}

function stripQuoteMarkers(text: string): string {
  return text
    .split(/\r?\n/)
    .map((line) => line.replace(/^\s*>\s?/, ""))
    .join("\n")
    .trim();
}

function renderBlock(block: DocumentEditorBlock): string {
  switch (block.kind) {
    case "heading":
      return `${"#".repeat(block.level)} ${block.text}`.trimEnd();
    case "paragraph":
    case "metadata_block":
    case "source_unit":
      return block.text;
    case "code_block":
      return `\`\`\`${block.language ?? ""}\n${block.text}\n\`\`\``;
    case "quote_block":
      return block.text
        .split(/\r?\n/)
        .map((line) => `> ${line}`.trimEnd())
        .join("\n");
    case "table":
      return renderMarkdownTable(block.rows);
    default:
      return "";
  }
}

function renderMarkdownTable(rows: string[][]): string {
  if (rows.length === 0) {
    return "";
  }

  const maxColumns = Math.max(...rows.map((row) => row.length));
  const normalizedRows = rows.map((row, rowIndex) => {
    const next = [...row];
    next.length = maxColumns;
    return next.map((cell, cellIndex) => {
      const value = (cell ?? "").trim();
      if (rowIndex === 0 && value.length === 0) {
        return `col_${cellIndex + 1}`;
      }
      return value.replace(/\|/g, "\\|").replace(/\r?\n/g, " <br> ");
    });
  });

  const lines = [
    `| ${normalizedRows[0].join(" | ")} |`,
    `| ${Array.from({ length: maxColumns }, () => "---").join(" | ")} |`,
  ];
  for (const row of normalizedRows.slice(1)) {
    lines.push(`| ${row.join(" | ")} |`);
  }
  return lines.join("\n");
}

function readString(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function readStringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];
}

function readNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}
