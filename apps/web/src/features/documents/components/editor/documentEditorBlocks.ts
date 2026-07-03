import type { PreparedSegmentItem } from "@/shared/api/documents";

import {
  codeLanguageForSourceFormat,
  isCodeLikeSourceFormat,
  isRasterImageSourceFormat,
  isTableLikeSourceFormat,
} from "./editorSurfaceMode";

type DocumentEditorBlockKind =
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

type DocumentEditorBlock =
  | (BaseBlock & { kind: "heading"; level: number; text: string })
  | (BaseBlock & { kind: "paragraph"; text: string })
  | (BaseBlock & { kind: "list_item"; text: string })
  | (BaseBlock & { kind: "code_block"; text: string; language?: string | undefined })
  | (BaseBlock & { kind: "quote_block"; text: string })
  | (BaseBlock & { kind: "metadata_block"; text: string })
  | (BaseBlock & { kind: "source_unit"; text: string })
  | (BaseBlock & { kind: "table"; rows: string[][]; sheetName?: string | undefined });

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
  const sourceSegments = items.map(normalizeSegment);

  if (isCodeLikeSourceFormat(sourceFormat)) {
    const codeLines = sourceSegments
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

  const normalized = prepareSegmentsForEditor(sourceSegments, sourceFormat);
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
    if (!block) {
      continue;
    }
    if (block.kind === "list_item") {
      const items: Extract<DocumentEditorBlock, { kind: "list_item" }>[] = [block];
      while (index + 1 < blocks.length) {
        const next = blocks[index + 1];
        if (!next || next.kind !== "list_item") {
          break;
        }
        index += 1;
        items.push(next);
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
  const segment = item.segment;
  const tableCoordinates = item.tableCoordinates ?? null;

  return {
    ordinal: readNumber(segment.ordinal) ?? 0,
    blockKind: readString(segment.blockKind) ?? "paragraph",
    headingTrail: readStringArray(segment.headingTrail),
    text: readString(item.text ?? item.normalizedText) ?? "",
    parentBlockId: readString(item.parentBlockId),
    tableRowIndex: readNumber(tableCoordinates?.rowIndex) ?? null,
    codeLanguage: readString(item.codeLanguage),
  };
}

function prepareSegmentsForEditor(
  segments: NormalizedSegment[],
  sourceFormat?: string,
): NormalizedSegment[] {
  if (isRasterImageSourceFormat(sourceFormat)) {
    return segments;
  }

  const readableSegments = segments.filter(
    (segment) => !isExtractionScaffoldSegment(segment),
  );

  return readableSegments.some((segment) => segment.text.trim().length > 0)
    ? readableSegments
    : segments;
}

function isExtractionScaffoldSegment(segment: NormalizedSegment): boolean {
  const text = segment.text.trim();
  if (text.length === 0) {
    return false;
  }

  if (text === "<!-- image -->") {
    return true;
  }

  const unquoted = stripQuoteMarkers(text);
  if (/^Image OCR:\s*/i.test(unquoted)) {
    return true;
  }

  return /^--- Embedded image \d+(?:\s*\([^)]+\))?\s*---/i.test(text);
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
  const hashes = match?.[1];
  const body = match?.[2];
  if (hashes !== undefined && body !== undefined) {
    return {
      kind: "heading",
      level: hashes.length,
      text: normalizeDisplayText(body),
    };
  }

  return {
    kind: "heading",
    level: Math.min(Math.max(headingTrail.length, 1), 6),
    text: normalizeDisplayText(text),
  };
}

function buildScalarBlock(
  kind: Exclude<DocumentEditorBlockKind, "heading" | "table">,
  segment: NormalizedSegment,
): DocumentEditorBlock {
  switch (kind) {
    case "list_item":
      return { kind, text: normalizeDisplayText(stripListMarker(segment.text)) };
    case "code_block":
      return {
        kind,
        text: stripCodeFence(segment.text),
        language: segment.codeLanguage ?? undefined,
      };
    case "quote_block":
      return {
        kind,
        text: normalizeDisplayText(stripQuoteMarkers(segment.text)),
      };
    case "metadata_block":
    case "source_unit":
      return { kind, text: normalizeDisplayText(segment.text) };
    case "paragraph":
    default:
      return { kind: "paragraph", text: normalizeDisplayText(segment.text) };
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

function normalizeDisplayText(text: string): string {
  return text
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
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

  const headerRow = normalizedRows[0];
  if (!headerRow) {
    return "";
  }

  const lines = [
    `| ${headerRow.join(" | ")} |`,
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
