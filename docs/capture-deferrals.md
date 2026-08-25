# Capture deferrals

What the capture audit found, what this wave wired, and what is still not
captured — with the reason, so the next person does not re-derive it.

Three reasons appear below and they are not interchangeable:

- **Not reachable** — the data exists inside the parser crate and no public
  API returns it. Fixing this means an upstream change, not a change here.
- **Needs a typed home** — the data reaches the event plane, fully typed,
  but the Document schema has no field for it. It is not flattened into a
  string map; per the fleet's typing rule, data whose shape is known gets a
  typed field or it waits for one.
- **Deliberate** — reachable and homed, and still not done, because the
  cost is not worth the return yet.

## Landed

| Audit row | Where it lands now |
|---|---|
| S1 page as an untyped custom field | `ProvenanceItem.page_no`, always |
| S2 / D17 link annotations | `InlineSpan.hyperlink` over the anchored words, plus item-level `hyperlink` when a block is entirely one link |
| S10 whole-stream verdict on the Document plane | `PageItem.quality` per page |
| D1 / D2 per-page OCR verdicts | `PageMarkdown.needs_ocr` / `.ocr_reason`, `ParseStatus.extraction_ocr_reasons` |
| D6 / D22 stripped furniture | `PageMarkdown.furniture`, and the furniture group under `CONTENT_LAYER_FURNITURE` |
| D8 per-run geometry | `TextSpan.bbox`, and `ProvenanceItem.bbox` on every located item |
| D9 page dimensions | `PageGeometry.media_box` / `.crop_box`, `PageItem.size` |
| D10 font identity and size | `TextSpan.font_family` / `.font_tag` / `.font_size` |
| D11 marked-content ids | `TextSpan.mcid` |
| D12 / U2 tagged roles | `StructureRole`, `PageStructure`, `TextItemBase.style_name`, authored heading depth |
| D16 table-of-contents classification | `TableKind.CONTENTS`, `DOC_ITEM_LABEL_DOCUMENT_INDEX` |
| D19 page rotation | `PageGeometry.rotation`, `PageQuality.rotation_degrees` (read from `/Rotate`, not inferred) |
| D21 text-quality evidence | `PageMarkdown.replacement_runs`, `PageQuality.replacement_runs` |
| S5 / U3 tier 1 tables | `TableRegion`, `Document.tables[]` with typed cells and per-cell boxes |
| S6 lists | `ListItem.enumerated` / `.marker` inside a `GROUP_LABEL_LIST` group |
| S7 code blocks | `CodeItem` with `code_language_raw` |
| U6 markdown policy | reachable now that the service renders from items rather than calling the options-less entry point |
| U8 / U9 / U16 information dictionary, XMP, version, language | `DocumentInfo`, `PdfMetadata.xmp_packet` / `.pdf_version` / `.language`, `Document.source_meta` |
| U10 outline | `PdfMetadata.outline`, `Document.outline` |
| U12 embedded files | `PdfMetadata.embedded_files`, `Document.attachments` |
| U13 encryption posture | `EncryptionInfo` |
| U15 page geometry | `PageGeometry` |
| U22 internal destinations | `LinkTarget.dest_page_no` / `.dest_name`, `InlineSpan.target`, `Document.anchors` |
| U23 page labels | `PageGeometry.label` |
| U24 file identifier | `PdfMetadata.file_id`, `DocumentOrigin.source_id` |
| S3 formatting | `TextSpan` flags, `InlineSpan.formatting` / `.font_family` / `.font_size_pt` over the characters they cover |
| D7 image placements | `SPAN_KIND_IMAGE` runs, `Document.pictures[]` with their boxes |
| Links over non-text regions | `PictureItem.hyperlink` for external targets, `PictureItem.target` for internal ones |
| U8 / U13 / U16 posture and identity, on the Document plane | `DocumentMeta.format_version` / `.structured` / `.authoring_tool` / `.subject` / `.protection` / `.raw_metadata` |
| U15 / U23 page geometry and labels, on the Document plane | `PageItem.page_label` / `.media_size` / `.user_unit` |
| Trapping declaration | `DocumentInfo.trapped` (verbatim) and `.trapped_state` (typed), `DocumentMeta.trapped` |
| Version constant | derived from the manifest during const evaluation |

## Not reachable without an upstream change

Four asks, and they are the whole of what this service cannot capture. Each
one is data the parser crate computes and does not return; none of them can
be fixed from here.

**D18 — invisible text (render mode 3).** The content-stream walker
recognises invisible text and takes a parameter controlling whether to keep
it, but every public entry point passes `false`, and the
`skipped_invisible` flag it returns is discarded by the caller inside the
crate. So an OCR-under-image text layer or a hidden watermark cannot be
emitted under `CONTENT_LAYER_INVISIBLE`, and cannot even be reported as
having existed. **Ask:** make `include_invisible` an option on
`extract_text_with_positions_mem_pages`, or return the flag.

**D21 — the garble score itself.** `PageQuality.garble_score` stays unset.
The letter-frequency correlation that distinguishes substitution-cipher
garble from natural text lives in a private module, and only the derived
boolean escapes. Replacement-character runs are counted here instead,
because they can be counted exactly; putting a differently-defined number
under the name `garble_score` would be worse than leaving it empty.
**Ask:** make the text-quality analysis public, or return its score
per page.

**D13 — vector rectangles and line segments.** `PdfRect` and `PdfLine` are
public types, but the memory-based accessor that returns them is
`pub(crate)`. Without them the rect- and line-driven table detectors are
unreachable and only the heuristic detector over items can run, so a table
drawn with real rules is detected no better than a borderless one.
**Ask:** a public `extract_text_with_positions_and_rects_mem`.

**Layout complexity from items.** `pages_with_columns` cannot be recomputed
from items a caller holds: the column detector is `pub(crate)`. That is why
FULL mode runs a separate analysis pass rather than deriving the layout
verdict from the runs it already has. Making the detector public would
remove a whole read of the file from every FULL call. **Ask:** a public
layout analyser over `&[TextItem]`.

Two rows the audit listed here are not asks and belong below with the rest
of the deliberate deferrals: **D20**, the detector's per-page statistics,
which are collapsed into one of four reason strings and several of which
the crate already marks dead; and **U4**, `pages_sampled` /
`pages_with_text` / `ocr_recommended`, which are on `PdfTypeResult` and
reachable with a second detection call this service chooses not to make.

## Needs a typed home in the Document schema

Nothing. Every row this ledger has carried under that heading landed in the
canonical schema and is wired: `DocumentMeta.format_version`, `.structured`,
`.authoring_tool`, `.subject`, `.protection`, `.raw_metadata` and `.trapped`
(gRParse `af1b769` and `4f6ad0d`); `DocumentOrigin.source_id`;
`PageItem.page_label`, `.media_size` and `.user_unit`;
`PictureItem.hyperlink` and `.target`.

What is left to capture is the four asks above, and they are all inside the
parser crate.

## Deliberate, and cheap to add later

- **Sub- and superscript.** The extractor computes them from font-size
  ratio plus baseline offset and uses the result only for spacing, so
  `Formatting.script` has no source. `TextSpan` would need to carry the
  judgement first.
- **S4 AcroForm fields.** The extractor concatenates `name: value` into one
  run, so the key/value boundary is already gone by the time this service
  sees it. `Document.form_items[].graph` and the `FieldItem` fields the
  audit proposed need the parser to emit fields structurally first.
- **U18 image detail.** Pictures are placed, but colourspace, bit depth,
  the filter chain and `/SMask` are never read by the crate. The direct
  reader this service now has could read all four off the XObject
  dictionary; the pixel bytes would need a decoder the default build does
  not link, and `ImageRef` stays unset until then.
- **U4 detector sampling statistics.** `pages_sampled` in particular would
  say how much of the document the confidence figure is actually based on.
  It costs a second detection call.
- **U5 page-count estimate for unparseable files.** A byte-scan estimate
  exists and the failure path returns `INVALID_ARGUMENT` and nothing else.
- **U7 region-scoped re-asks.** A coordinator holding a box from another
  collector could ask this one for that region's text. It needs a new RPC,
  not a new field.
- **U11 non-link annotations.** Sticky notes, highlights and reviewer
  comments are skipped by the annotation walk. The reader here already
  walks `/Annots`; adding the other subtypes is mechanical, and the
  Document's `FineRef comments` is the natural anchor.
- **U14 / U17 revision chain and signatures.** Both say the file was edited
  or signed after the fact. Neither is read.
- **U19 font inventory.** Which faces are embedded and which are not
  explains the garbled text this service reports as a count.
- **U21 optional content groups.** Text is extracted as if every layer were
  on, so a hidden draft or redaction layer enters the output silently. A
  `has_optional_content` flag is the cheap honesty fix.
- **U25 artifact-marked content.** `/Artifact` is never checked, which is
  why furniture detection is a repetition heuristic rather than a lookup.
  Reading it would make the furniture layer exact.
- **U27 inline images.** `BI`/`ID`/`EI` are skipped by the operator
  scanner.

## Known approximations

- **Table headers.** The detector reports no header row. The first row of a
  data table is marked `column_header` because that is the convention the
  crate's own markdown renderer follows when it prints the grid; a table of
  contents gets no header row at all.
- **Row and column spans.** Every cell is 1x1. Spans need the vector-grid
  route (`detect_vector_grid_in_region_mem` feeding
  `extract_tables_with_structure_cells_mem`), which is a larger job and is
  only available for border-drawn tables.
- **Locating a block among its runs.** The fold matches a block of markdown
  to the runs behind it by comparing letters and digits, forwards through
  the page. A page whose renderer reorders runs — multi-column layouts
  especially — can leave a block unlocated, and an unlocated block gets a
  page-only provenance entry rather than a wrong box.
- **Where a picture sits.** A picture is placed under whatever heading is
  open when its page begins, because the image runs are folded before the
  page's own blocks are read. Its box is exact; its position in the
  hierarchy is reading order at page granularity.
- **The furniture report.** It names runs the markdown does not contain, in
  reading order. A run the renderer moved backwards past another reads as
  dropped. The report is approximate on reordering pages and exact on
  ordinary ones.
- **Per-page rendering and document-wide stripping.** Markdown is rendered
  one page at a time, which is what keeps the stream a stream. The
  cross-page repetition classifier behind the header and footer stripper
  has no cross-page evidence in that mode, so it fires less than it would
  on a whole-document render. Less silent deletion, and the same
  `report_furniture` names whatever still goes.
