use super::*;

/// The optional decoration of a block, kept out of `build_block`'s positional
/// signature so a block is built from its ordinal, kind, lines, heading trail
/// and this bundle rather than from eight positional arguments.
#[derive(Default)]
struct BlockDecoration {
    parent_block_id: Option<Uuid>,
    code_language: Option<String>,
    table_coordinates: Option<StructuredTableCoordinates>,
    normalized_text_override: Option<String>,
}

/// The table-scoped context shared by the row and summary emitters, so the
/// heading stack, owning table block and derived sheet/table names travel as one
/// value instead of four positional arguments.
struct TableBlockContext<'a> {
    heading_stack: &'a [String],
    table_block_id: Uuid,
    sheet_name: Option<&'a str>,
    table_name: Option<&'a str>,
}

pub(super) fn build_structured_blocks(
    command: &PrepareStructuredRevisionCommand,
    cancellation_token: &CancellationToken,
) -> Result<Vec<StructuredBlockData>, StructuredPreparationError> {
    let lines = if command.structure_hints.lines.is_empty() {
        fallback_line_hints(&command.normalized_text)
    } else {
        command.structure_hints.lines.clone()
    };
    let mut blocks = Vec::<StructuredBlockData>::new();
    let mut heading_stack = Vec::<String>::new();
    let mut ordinal = 0_i32;
    let mut index = 0_usize;

    while index < lines.len() {
        ensure_not_cancelled(cancellation_token)?;
        let line = &lines[index];
        if line.text.trim().is_empty() {
            index += 1;
            continue;
        }
        if is_code_fence(line) {
            append_code_block(
                &lines,
                &mut index,
                &mut ordinal,
                &heading_stack,
                &mut blocks,
                cancellation_token,
            )?;
            continue;
        }
        if is_heading_line(line) {
            append_heading_block(line, &mut index, &mut ordinal, &mut heading_stack, &mut blocks);
            continue;
        }
        if is_table_row_line(line) {
            append_table_blocks(
                &lines,
                &mut index,
                &mut ordinal,
                &heading_stack,
                &mut blocks,
                cancellation_token,
            )?;
            continue;
        }
        append_scalar_block(&lines, &mut index, &mut ordinal, &heading_stack, &mut blocks);
    }

    mark_structural_boilerplate(&mut blocks, cancellation_token)?;
    Ok(blocks)
}

fn append_code_block(
    lines: &[ExtractionLineHint],
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &[String],
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    let language = lines[*index].text.trim().trim_start_matches('`').trim().to_string();
    *index += 1;
    let mut code_lines = Vec::<ExtractionLineHint>::new();
    while *index < lines.len() && !is_code_fence(&lines[*index]) {
        ensure_not_cancelled(cancellation_token)?;
        if !lines[*index].text.trim().is_empty() {
            code_lines.push(lines[*index].clone());
        }
        *index += 1;
    }
    if *index < lines.len() && is_code_fence(&lines[*index]) {
        *index += 1;
    }
    if code_lines.is_empty() {
        return Ok(());
    }
    let resolved_language = if language.is_empty() {
        let code_text =
            code_lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
        crate::shared::ast_extraction::detect_language(&code_text).map(str::to_string)
    } else {
        Some(language)
    };
    blocks.push(build_block(
        *ordinal,
        StructuredBlockKind::CodeBlock,
        &code_lines,
        heading_stack,
        BlockDecoration { code_language: resolved_language, ..Default::default() },
    ));
    *ordinal += 1;
    Ok(())
}

fn append_heading_block(
    line: &ExtractionLineHint,
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &mut Vec<String>,
    blocks: &mut Vec<StructuredBlockData>,
) {
    let trimmed = line.text.trim();
    let heading_text = normalize_heading_text(trimmed);
    update_heading_stack(heading_stack, heading_depth(trimmed), &heading_text);
    blocks.push(build_block(
        *ordinal,
        StructuredBlockKind::Heading,
        std::slice::from_ref(line),
        heading_stack,
        BlockDecoration::default(),
    ));
    *ordinal += 1;
    *index += 1;
}

fn append_table_blocks(
    lines: &[ExtractionLineHint],
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &[String],
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    let start = *index;
    while *index < lines.len() && is_table_row_line(&lines[*index]) {
        ensure_not_cancelled(cancellation_token)?;
        *index += 1;
    }
    let row_lines = &lines[start..*index];
    let table_block = build_block(
        *ordinal,
        StructuredBlockKind::Table,
        row_lines,
        heading_stack,
        BlockDecoration::default(),
    );
    let table_block_id = table_block.block_id;
    blocks.push(table_block);
    *ordinal += 1;
    let header_cells =
        row_lines.first().map(|row| parse_markdown_table_row(&row.text)).unwrap_or_default();
    let (sheet_name, table_name) = table_context_from_heading_stack(heading_stack);
    let context = TableBlockContext { heading_stack, table_block_id, sheet_name, table_name };
    let data_rows =
        append_table_row_blocks(row_lines, *ordinal, &context, blocks, cancellation_token)?;
    *ordinal += i32::try_from(data_rows.len()).unwrap_or(i32::MAX);
    append_table_summary_blocks(
        &header_cells,
        &data_rows,
        &context,
        ordinal,
        blocks,
        cancellation_token,
    )
}

fn append_table_row_blocks(
    row_lines: &[ExtractionLineHint],
    first_ordinal: i32,
    context: &TableBlockContext<'_>,
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<Vec<Vec<String>>, StructuredPreparationError> {
    let header_cells =
        row_lines.first().map(|row| parse_markdown_table_row(&row.text)).unwrap_or_default();
    let mut data_rows = Vec::new();
    for row_line in row_lines.iter().skip(1) {
        ensure_not_cancelled(cancellation_token)?;
        let row_cells = parse_markdown_table_row(&row_line.text);
        if row_cells.is_empty() || is_markdown_separator_row(&row_cells) {
            continue;
        }
        let row_index = data_rows.len();
        blocks.push(build_block(
            first_ordinal.saturating_add(i32::try_from(row_index).unwrap_or(i32::MAX)),
            StructuredBlockKind::TableRow,
            std::slice::from_ref(row_line),
            context.heading_stack,
            BlockDecoration {
                parent_block_id: Some(context.table_block_id),
                table_coordinates: Some(StructuredTableCoordinates {
                    row_index: i32::try_from(row_index).unwrap_or(i32::MAX),
                    column_index: 0,
                    row_span: 1,
                    column_span: 1,
                }),
                normalized_text_override: Some(build_semantic_table_row_text(
                    context.sheet_name,
                    context.table_name,
                    row_index,
                    &header_cells,
                    &row_cells,
                )),
                ..Default::default()
            },
        ));
        data_rows.push(row_cells);
    }
    Ok(data_rows)
}

fn append_table_summary_blocks(
    header_cells: &[String],
    data_rows: &[Vec<String>],
    context: &TableBlockContext<'_>,
    ordinal: &mut i32,
    blocks: &mut Vec<StructuredBlockData>,
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    for summary in build_table_column_summaries(
        context.sheet_name,
        context.table_name,
        header_cells,
        data_rows,
    ) {
        ensure_not_cancelled(cancellation_token)?;
        blocks.push(build_block(
            *ordinal,
            StructuredBlockKind::MetadataBlock,
            &[],
            context.heading_stack,
            BlockDecoration {
                parent_block_id: Some(context.table_block_id),
                normalized_text_override: Some(render_table_column_summary(&summary)),
                ..Default::default()
            },
        ));
        *ordinal += 1;
    }
    Ok(())
}

/// Emits one block for the run of lines starting at `index`.
///
/// Prose is wrapped at the source line width, so a single sentence — and the
/// qualifier that scopes it — routinely spans several physical lines. One block
/// per line would strand a value in one block and the condition restricting it
/// in another, which no later stage can put back together. A run therefore
/// continues until a blank line or a construct that opens its own block.
///
/// Line-oriented records keep one block per line: a line whose shape *opens* a
/// record (a list item, a `key: value` pair, an endpoint, a record unit, code)
/// stands alone. The same shape *inside* a run is absorbed, because there the
/// sentence began on an earlier line.
fn append_scalar_block(
    lines: &[ExtractionLineHint],
    index: &mut usize,
    ordinal: &mut i32,
    heading_stack: &[String],
    blocks: &mut Vec<StructuredBlockData>,
) {
    let start = *index;
    *index += 1;
    if !opens_own_block(&lines[start]) {
        let page = lines[start].page_number;
        while lines.get(*index).is_some_and(|line| continues_prose_run(line, page)) {
            *index += 1;
        }
    }
    let run = &lines[start..*index];
    blocks.push(build_block(
        *ordinal,
        classify_prose_run_kind(run),
        run,
        heading_stack,
        BlockDecoration::default(),
    ));
    *ordinal += 1;
}

/// Signals whose line shape is a record of its own wherever it appears, so it
/// neither opens a prose run nor is absorbed into one.
const STANDALONE_LINE_SIGNALS: [ExtractionLineSignal; 6] = [
    ExtractionLineSignal::ListItem,
    ExtractionLineSignal::Quote,
    ExtractionLineSignal::MetadataCandidate,
    ExtractionLineSignal::SourceProfile,
    ExtractionLineSignal::SourceUnit,
    ExtractionLineSignal::CodeLine,
];

/// The two predicates differ in exactly one signal, and that difference is the
/// point: an endpoint *opens* its own block, but an endpoint reached in the
/// middle of a sentence is part of that sentence.
fn opens_own_block(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::EndpointCandidate)
        || STANDALONE_LINE_SIGNALS.iter().any(|signal| has_signal(line, *signal))
}

/// A run never crosses a page boundary: the block records the page of its first
/// line, so absorbing a later page would file the text under the wrong one.
fn continues_prose_run(line: &ExtractionLineHint, page: Option<i32>) -> bool {
    line.page_number == page
        && !line.text.trim().is_empty()
        && !is_code_fence(line)
        && !is_heading_line(line)
        && !is_table_row_line(line)
        && !is_listed_endpoint_line(line)
        && !STANDALONE_LINE_SIGNALS.iter().any(|signal| has_signal(line, *signal))
}

/// A line holding nothing but an address — optionally behind a request verb —
/// is an entry someone will look up on its own, so it stays addressable even
/// when the lines above it are prose. A line that merely *starts* with an
/// address and then keeps talking is the tail of a sentence.
fn is_listed_endpoint_line(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::EndpointCandidate)
        && line.text.split_whitespace().count() <= LISTED_ENDPOINT_MAX_TOKENS
}

/// An address, plus at most the verb in front of it.
const LISTED_ENDPOINT_MAX_TOKENS: usize = 2;

/// A run keeps the kind its opening line earns, except that an endpoint named
/// anywhere inside it still makes the whole run endpoint evidence — the address
/// and the conditions around it are one statement.
fn classify_prose_run_kind(run: &[ExtractionLineHint]) -> StructuredBlockKind {
    let Some(first) = run.first() else {
        return StructuredBlockKind::Paragraph;
    };
    let kind = classify_scalar_block_kind(first);
    if matches!(kind, StructuredBlockKind::Paragraph)
        && run.iter().any(|line| has_signal(line, ExtractionLineSignal::EndpointCandidate))
    {
        return StructuredBlockKind::EndpointBlock;
    }
    kind
}

pub(super) fn detect_boilerplate(block_kind: StructuredBlockKind, text: &str) -> bool {
    if !is_boilerplate_candidate_kind(block_kind) {
        return false;
    }
    has_dense_link_shape(text)
        || ['|', '•', '>', '›']
            .into_iter()
            .any(|separator| has_dense_delimiter_shape(text, separator))
}

const fn is_boilerplate_candidate_kind(block_kind: StructuredBlockKind) -> bool {
    matches!(block_kind, StructuredBlockKind::Paragraph | StructuredBlockKind::ListItem)
}

fn has_dense_link_shape(text: &str) -> bool {
    let (token_count, link_count) =
        text.split_whitespace().fold((0_usize, 0_usize), |(token_count, link_count), token| {
            (token_count + 1, link_count + usize::from(token.contains("://")))
        });
    link_count >= MIN_DENSE_LINK_COUNT && link_count.saturating_mul(2) >= token_count
}

fn has_dense_delimiter_shape(text: &str, separator: char) -> bool {
    let mut segment_count = 0_usize;
    for segment in text.split(separator).map(str::trim) {
        if segment.is_empty() || segment.chars().count() > MAX_STRUCTURAL_SEGMENT_CHARS {
            return false;
        }
        segment_count += 1;
    }
    segment_count >= MIN_STRUCTURAL_SEGMENT_COUNT
}

fn mark_structural_boilerplate(
    blocks: &mut [StructuredBlockData],
    cancellation_token: &CancellationToken,
) -> Result<(), StructuredPreparationError> {
    let mut occurrences_by_key = BTreeMap::<String, (BTreeSet<i32>, Vec<usize>)>::new();
    for (index, block) in blocks.iter_mut().enumerate() {
        ensure_not_cancelled(cancellation_token)?;
        block.is_boilerplate = detect_boilerplate(block.block_kind, &block.text);
        if let (Some(key), Some(page_number)) = (repeated_block_key(block), block.page_number) {
            let (pages, indexes) = occurrences_by_key.entry(key).or_default();
            pages.insert(page_number);
            indexes.push(index);
        }
    }
    for (pages, indexes) in occurrences_by_key.into_values() {
        ensure_not_cancelled(cancellation_token)?;
        if pages.len() >= MIN_REPEATED_BLOCK_PAGE_COUNT {
            for index in indexes {
                blocks[index].is_boilerplate = true;
            }
        }
    }
    if !blocks.is_empty() && blocks.iter().all(|block| block.is_boilerplate) {
        for block in blocks {
            ensure_not_cancelled(cancellation_token)?;
            block.is_boilerplate = false;
        }
    }
    Ok(())
}

fn repeated_block_key(block: &StructuredBlockData) -> Option<String> {
    if !is_boilerplate_candidate_kind(block.block_kind) {
        return None;
    }
    let key = block.normalized_text.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = key.chars().count();
    (length > 0 && length <= MAX_REPEATED_BLOCK_CHARS).then_some(key)
}

fn fallback_line_hints(content: &str) -> Vec<ExtractionLineHint> {
    crate::shared::extraction::build_text_layout_from_content(content).structure_hints.lines
}

pub(super) fn classify_scalar_block_kind(line: &ExtractionLineHint) -> StructuredBlockKind {
    let trimmed = line.text.trim();
    if has_signal(line, ExtractionLineSignal::ListItem) {
        StructuredBlockKind::ListItem
    } else if has_signal(line, ExtractionLineSignal::EndpointCandidate) {
        StructuredBlockKind::EndpointBlock
    } else if has_signal(line, ExtractionLineSignal::Quote) {
        StructuredBlockKind::QuoteBlock
    } else if has_signal(line, ExtractionLineSignal::SourceProfile) {
        StructuredBlockKind::SourceProfile
    } else if has_signal(line, ExtractionLineSignal::SourceUnit) {
        StructuredBlockKind::SourceUnit
    } else if has_signal(line, ExtractionLineSignal::MetadataCandidate)
        && !looks_like_compound_product_label(trimmed)
    {
        StructuredBlockKind::MetadataBlock
    } else if has_signal(line, ExtractionLineSignal::CodeLine) {
        StructuredBlockKind::CodeBlock
    } else {
        StructuredBlockKind::Paragraph
    }
}

fn build_block(
    ordinal: i32,
    block_kind: StructuredBlockKind,
    lines: &[ExtractionLineHint],
    heading_stack: &[String],
    decoration: BlockDecoration,
) -> StructuredBlockData {
    let BlockDecoration {
        parent_block_id,
        code_language,
        table_coordinates,
        normalized_text_override,
    } = decoration;
    let block_id = Uuid::now_v7();
    let raw_text = lines.iter().map(|line| line.text.trim_end()).collect::<Vec<_>>().join("\n");
    let normalized_text = normalized_text_override.unwrap_or_else(|| match block_kind {
        StructuredBlockKind::Heading => {
            heading_stack.last().cloned().unwrap_or_else(|| raw_text.trim().to_string())
        }
        _ => raw_text.trim().to_string(),
    });
    let heading_trail = heading_stack.to_vec();
    let section_path = heading_stack
        .iter()
        .map(|heading| {
            crate::services::graph::identity::normalize_graph_identity_component(heading)
        })
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let page_number = lines.iter().find_map(|line| line.page_number);
    let source_span = match (lines.first(), lines.last()) {
        (Some(first), Some(last)) => Some(StructuredSourceSpan {
            start_offset: first.start_offset.unwrap_or_default(),
            end_offset: last.end_offset.unwrap_or_else(|| first.end_offset.unwrap_or_default()),
        }),
        _ => None,
    };

    StructuredBlockData {
        block_id,
        ordinal,
        block_kind,
        text: raw_text.trim().to_string(),
        normalized_text,
        heading_trail,
        section_path,
        page_number,
        source_span,
        parent_block_id,
        table_coordinates,
        code_language,
        is_boilerplate: false,
    }
}

pub(super) fn build_outline(blocks: &[StructuredBlockData]) -> Vec<StructuredOutlineEntry> {
    blocks
        .iter()
        .filter(|block| matches!(block.block_kind, StructuredBlockKind::Heading))
        .map(|block| StructuredOutlineEntry {
            block_id: block.block_id,
            block_ordinal: block.ordinal,
            depth: i32::try_from(block.heading_trail.len().saturating_sub(1)).unwrap_or(i32::MAX),
            heading: block.normalized_text.clone(),
            heading_trail: block.heading_trail.clone(),
            section_path: block.section_path.clone(),
        })
        .collect()
}

fn table_context_from_heading_stack(heading_stack: &[String]) -> (Option<&str>, Option<&str>) {
    match heading_stack {
        [] => (None, None),
        [sheet] => (Some(sheet.as_str()), None),
        [rest @ .., last] => {
            let sheet = rest.first().map(String::as_str).or(Some(last.as_str()));
            (sheet, Some(last.as_str()))
        }
    }
}

fn is_code_fence(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::CodeFence) || line.text.trim().starts_with("```")
}

fn is_heading_line(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::Heading) || line.text.trim().starts_with('#')
}

fn is_table_row_line(line: &ExtractionLineHint) -> bool {
    has_signal(line, ExtractionLineSignal::TableRow)
}

fn has_signal(line: &ExtractionLineHint, signal: ExtractionLineSignal) -> bool {
    line.signals.contains(&signal)
}

fn normalize_heading_text(text: &str) -> String {
    text.trim_start_matches('#').trim().to_string()
}

fn heading_depth(text: &str) -> usize {
    let trimmed = text.trim_start();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    usize::max(hashes, 1)
}

fn update_heading_stack(stack: &mut Vec<String>, depth: usize, heading: &str) {
    while stack.len() >= depth {
        stack.pop();
    }
    stack.push(heading.to_string());
}

fn looks_like_compound_product_label(text: &str) -> bool {
    let Some((left, right)) = text.split_once(':') else {
        return false;
    };
    !left.trim().contains(' ')
        && !right.trim().is_empty()
        && (right.contains('–') || right.contains('-'))
        && !text.contains(": ")
}
