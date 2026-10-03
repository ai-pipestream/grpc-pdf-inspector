# Capture deferrals

What the capture audit found, what this wave wired, and what is still not
captured — with the reason, so the next person does not re-derive it.

Three reasons appear below and they are not interchangeable:

- **Not reachable** — the data exists inside the parser crate and no public
  API returns it. This category is now empty: the crate is vendored under
  `vendor/pdf-inspector` and the four APIs that were private are public
  there. See "The upstream asks, and the patches that answered them".
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
| D6 / D22 stripped furniture | `PageMarkdown.furniture`, and the furniture group under `CONTENT_LAYER_FURNITURE`, on chrome evidence only |
| Runs the rendering left out | `PageMarkdown.dropped`, folded back into `#/body` where the page drew them |
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
| Figures drawn as Form XObjects | `SPAN_KIND_FORM` runs, `Document.pictures[]` with the form's placed box, each picture in reading order after the text above it |
| Links over non-text regions | `PictureItem.hyperlink` for external targets, `PictureItem.target` for internal ones |
| U8 / U13 / U16 posture and identity, on the Document plane | `DocumentMeta.format_version` / `.structured` / `.authoring_tool` / `.subject` / `.protection` / `.raw_metadata` |
| U15 / U23 page geometry and labels, on the Document plane | `PageItem.page_label` / `.media_size` / `.user_unit` |
| Trapping declaration | `DocumentInfo.trapped` (verbatim) and `.trapped_state` (typed), `DocumentMeta.trapped` |
| Version constant | derived from the vendored crate's manifest during const evaluation |
| D18 invisible text | `PageMarkdown.invisible` runs with their boxes, `ParseStatus.has_invisible_text`, and the runs as items under `CONTENT_LAYER_INVISIBLE` |
| D21 the garble score | `PageMarkdown.garble_score`, `PageQuality.garble_score` |
| D13 vector rectangles and line segments | the ruled-table detectors, reached through `TableRegion` and `Document.tables[]` |
| Layout complexity from items | `ParseStatus.layout`, computed in the page loop instead of by a second read of the file |

## The upstream asks, and the patches that answered them

Four asks stood here, and each one was data the parser crate computes and
returns through no public API. They were recorded as needing an upstream
change because there was no way to fix them from the service side. A fifth
ask arrived later, from a regression rather than from the audit, and is
below with them.

There was a way. The crate is MIT licensed, so it is vendored under
`vendor/pdf-inspector` and the four APIs are public there. The copy landed
unpatched first, so the vendoring can be diffed against the crates.io
tarball on its own, and each patch is a commit of its own on top of it.
Every patch is an additive visibility change: nothing that was public
changed shape, no threshold moved, and the crate parses exactly what it
parsed before. `vendor/pdf-inspector/README.md` is the map;
`AGENTS.md` has the rules for touching it.

**D18 — invisible text (render mode 3).** The content-stream walker
recognised invisible text and took a parameter controlling whether to keep
it, but every public entry point passed `false`, and the
`skipped_invisible` flag it returned was discarded by the caller inside the
crate.

- *Patch* `5c726bc`: `extract_text_with_positions_and_rects_mem_with_invisible`
  and `extract_text_with_positions_mem_pages_with_invisible` take the
  option and return the flag. The per-page flag is folded across the pages
  instead of being dropped in the extraction loop.
- *Wired*: `ParseStatus.has_invisible_text` in every FULL call, free from
  the extraction pass. `PdfOptions.report_invisible` (or a Document, which
  consumes the same events) adds `PageMarkdown.invisible`: the hidden runs
  with their boxes, and the same runs as Document items under
  `CONTENT_LAYER_INVISIBLE` in the furniture group. A hidden watermark is
  a reportable item and is never silently absent.
- *Tests*: `tests/invisible.rs`, over a fixture that draws its watermark
  with `3 Tr`.

**D21 — the garble score itself.** The letter-frequency correlation that
distinguishes substitution-cipher garble from natural text lived in a
private module, and only the derived boolean escaped, so
`PageQuality.garble_score` had no source.

- *Patch* `758c6ed`: `text_quality` is a public module.
  `analyze_text_quality`, `TextQualityReport` and `detect_encoding_issues`
  are public, and the report gains `letter_frequency`: a
  `LetterFrequencyScore` per page with both cosines, the letter count they
  are computed over, and the verdict they feed.
  `MIN_LETTERS_FOR_GARBLE_SCORE` names the floor below which the statistic
  is noise.
- *Wired*: `PageMarkdown.garble_score` and `PageQuality.garble_score` carry
  `1 - english_cosine`, which is the library's own number turned the right
  way up for a field defined with 0.0 as clean. A page with too few letters
  to measure reports nothing rather than a reassuring zero.
  Replacement-character runs are still counted beside it: they measure a
  different failure.
- *Tests*: `tests/quality.rs`, over a fixture whose second page is its
  first page with every letter substituted.

**D13 — vector rectangles and line segments.** `PdfRect` and `PdfLine` were
public types and `detect_tables_from_rects` and `detect_tables_from_lines`
were public functions, but the memory-based accessor returning the geometry
they run on was `pub(crate)`, so both detectors were unreachable and only
the alignment heuristic could run.

- *Patch* `8bb537e`: `extract_text_with_positions_and_rects_mem`, and
  `PageExtraction` with it.
- *Wired*: `src/tables.rs` runs the crate's own three detectors in the
  crate's own order, rules first and alignment last. It calls them; it
  reimplements nothing.
- *Tests*: `tests/tables.rs`, over a ruled fixture that the alignment
  detector misses outright, asserted as part of the test so the fixture
  cannot quietly stop being interesting.

**Layout complexity from items.** `pages_with_columns` could not be
recomputed from items a caller held, because the column detector was
`pub(crate)`. FULL therefore ran a whole separate analysis pass over the
file for a verdict its own runs already contained.

- *Patch* `9340221`: `extractor::detect_columns` and `ColumnRegion` are
  public.
- *Wired*: FULL computes `pages_with_tables` from the table detectors it
  runs per page anyway and `pages_with_columns` from `detect_columns` over
  the runs it holds, and the analysis pass is deleted. FULL reads the
  document twice now, not three times.
- *Tests*: `tests/passes.rs` counts the reads through
  `Metrics::parser_pass`, so a pass coming back fails a test.

**The furniture verdict itself.** The first version of the furniture report
compared a page's runs against its markdown and named everything missing.
That conflates two facts: the strippers judged this run to be chrome, and
the renderer did not emit it. On a two-column paper the second happens to
most of the page, because the renderer reads one column at a time and a
scan of its output in reading order finds nothing where it expects it: 1181 of
one conference paper's 1291 runs were filed as furniture, leaving 110 in
the body. Consumers that walk the body saw an empty paper while the
markdown looked fine.

- *Patch* `a3527ad`: `markdown::strip_repeated_header_footer_lines` is
  public. The classifier behind it proves furniture by repetition across
  pages, and this service renders one page at a time, so on the service's
  own calls it can prove nothing; asking it over the whole document is the
  only way to hear the parser's own verdict.
- *Wired*: `src/furniture.rs` weighs three kinds of chrome evidence over
  the whole document (the parser's verdict, a line repeating at the same
  isolated page edge with its digits read as a shape, and a column of short
  numbers standing outside the text block), and `PageMarkdown.furniture`
  carries what they convict. What the renderer left out with no chrome
  evidence behind it is content, and goes out on `PageMarkdown.dropped`
  with its box, to be folded back into the body at the place the page drew
  it.
- *Tests*: `tests/body_reachability.rs`, over a two-column fixture with a
  running head and a line number beside every row, asserting that only the
  chrome is furniture, that every body-layer item is reachable from
  `#/body`, and that nothing is in both layers.
- *Then measured against the paper itself*, which found two more faults
  that no fixture here reproduced. Its running head is ruled underneath,
  and the renderer spells an underlined run `<u>text</u>`; the fold matched
  a rendered block against the chrome report on letters alone, the tags put
  two letters into the block that the run did not have, and a head the
  report had already filed as furniture was folded into the body as well,
  on every page. And the head sits an ordinary line above the first line of
  text, so no white space isolates it and the repetition evidence never
  looked at it: on a document of three pages or more the parser's own
  classifier keeps the first occurrence of a running head, so page one's
  head stayed in the body. Tags are now dropped before letters are counted,
  and a line at a page edge set in a smaller face than the body is an edge
  line whether or not a gap proves it.
- *Held by*: `tests/dropped_runs.rs`, which drives the fold by hand because
  the renderer will not drop a run on any fixture this suite can author,
  and `common::assert_layers_and_parents_agree`, which states the invariant
  the whole plane rests on: an item's layer and the group it hangs under
  say the same thing, and the body walk reaches exactly the body layer.
- *And the verdict is taken before the rendering, not after it.* The paper
  numbers every line, and a margin number shares the baseline of the line
  it stands beside. The renderer assembles a line from the runs sharing
  one, so a number left in its input is not a run the rendering omits or
  keeps: it is fused into the middle of the sentence, printed `**001**`
  from the bold face the template sets it in and glued to the word after
  it. Five hundred and fifty-six of them read as body text on the real
  paper. Nothing decided about the output can separate them again, so the
  convicted runs are held out of the input `markdown` is rendered from,
  which is also the input every body item's text is folded from.
  `tests/furniture.rs` asserts both halves: no chrome in the rendering with
  the verdict, and the fusion itself without it.

**Form XObject placements.** A figure included from another PDF, which
is how a paper's plots are set, is a Form XObject: paths and a few
labels, no image anywhere. The walker entered every form to read its text
and knew, at the `Do`, the form's `/BBox` and the transformation in
force, and reported neither; the paper's second and third figures
therefore had no picture item, only their captions.

- *Patch* `6606929`:
  `extract_text_with_positions_rects_and_forms_mem_with_invisible` returns
  the runs, rectangles and lines of its sibling and, beside them, a
  `PdfForm` per invocation: the resource name and the `/BBox` carried
  through the form's `/Matrix` and the CTM at the call, page-level and
  nested alike, clipped and rotated exactly as the rectangles are. The
  sibling entry points return what they returned; the page walker gained a
  `_with_forms` twin and the old name is a wrapper that drops the forms.
- *Wired*: `SPAN_KIND_FORM` runs on the `spans` event, after the page's
  runs, with the placeholder `[Form: name]` and the placed box; the runs
  the renderer sees do not include them, so the markdown is unchanged.
  The fold makes a picture of a form that is a region of the page and not
  the page: not the wrapper a print-to-PDF producer draws the whole page
  through, judged against the page size the metadata pass measured; not a
  glyph or rule drawn as a form; not a form inside a form that already
  counts; and not a form whose content is an image the page reports
  already. The picture is placed in reading order, before the first
  located block below it that shares its width.
- *Tests*: `tests/pictures.rs`, over a fixture whose figure is a form of
  paths and one label placed between two paragraphs, and over a page drawn
  entirely through one form.

**Routing review (2026-10-02): scans the fast path dropped.** gRParse
takes a text-based classification with no OCR pages as finished and never
runs its own models, so a scanned page this service does not name
disappears from its output. A searchable scan (a page image behind an
invisible OCR layer) classified text-based and extracted as empty pages,
and a scanned page outside detection's eight-page sample was never named.

- *Patch* `47c55a9`: detection follows `Tr` with the extractor's scoping
  and counts invisible show operators apart; an image page whose text is
  all invisible is a scan carrying an OCR layer, its document is Mixed, and
  the per-page OCR list is built from every page's own analysis for
  text-based documents as well as mixed ones.
- *Patch* `667c916`: `extract_text_with_positions_rects_and_forms_mem_with_ocr_layer`
  applies the region extractor's OCR-layer fallback to the positioned runs
  and names the pages that adopted their layer.
- *Patch* `fbdf5fb`: `PdfProcessResult.ocr_recommended`.
- *Wired*: an adopted page's markdown is its OCR layer and the page is
  flagged `needs_ocr` with `OCR_REASON_SCANNED`; a page that drew a picture
  and no text is flagged on its page event and in
  `extraction_ocr_reasons`; `has_invisible_text` covers an adopted layer;
  `PdfInfo.ocr_recommended` carries the newspaper and template verdicts.
- *Tests*: `tests/ocr_routing.rs`, over a generated searchable scan and a
  twenty-page text document with two unsampled scanned pages.

**Follow-up (2026-10-03): the rendering mode is graphics state.** Both the
detector's scanner and the extractor set the text rendering mode back to 0
at `BT`, which the PDF spec does not do (ISO 32000-1, 9.3.1): `Tr` holds
across `BT` and `ET` until another `Tr` or a `Q`. A scan whose producer
sets `3 Tr` once, before its text objects, classified text-based and
extracted its hidden OCR layer as the body.

- *Patch* `bd074fe`: neither resets the mode at `BT`; both keep it on
  their `q`/`Q` stacks.
- *Tests*: `tests/ocr_routing.rs`, over a searchable scan that sets the
  mode before `BT`; the crate's `detector` and `extractor::content_stream`
  modules test the scoping.
- *CI* (`0d4fd39`): the crate's own unit tests run in the Rust job with
  `--manifest-path vendor/pdf-inspector/Cargo.toml`. Upstream tests that
  read `tests/fixtures`, which the published crate does not package, are
  marked `#[ignore]`, and the crate's lock holds `aes` at 0.9.2 so the
  1.88 job builds it.

**Hostile-input review (2026-10-02): bounded decoding.** Only the metadata
reader capped decompression; every pass of the parser decoded content,
font, CMap and Form XObject streams without limit.

- *Patch* `f92e3fc`: the guard module. Every decoding call site goes
  through it; documents load with the per-stream ceiling as lopdf's
  `max_decompressed_size`; a caller's `ParseGuard` adds a per-run budget, a
  deadline and a cancellation flag checked at page boundaries.
  `PdfError::Interrupted` reports the stop.
- *Wired*: every parser pass runs under a guard built from
  `GRPC_PDF_MAX_STREAM_BYTES`, `GRPC_PDF_MAX_DECOMPRESSED_BYTES` and
  `GRPC_PDF_MAX_PARSE_SECONDS` (also on `ServerLimits`), and the
  supervisor cancels it when the response stream is dropped.
- *Tests*: `tests/errors.rs` (a one-stream bomb, a many-stream budget, a
  call past its time), `tests/metadata.rs` (an XMP bomb) and
  `tests/streaming.rs` (a hang-up and a deadline inside the extraction
  pass); the crate's own `guard` and `detector` modules test the pieces.
- *Not covered*: lopdf skips an object stream past the ceiling while it
  loads and reports nothing, so a document whose object stream is a bomb
  loads without that stream's objects rather than failing; and lopdf's
  load-time decoding has no total budget of its own, only the per-stream
  ceiling. Nor does loading check the deadline or the cancellation flag:
  the first checkpoint is after it, so a document slow to load holds its
  slot past its time and its caller until loading ends.

Two rows the audit listed here were never asks and belong below with the
rest of the deliberate deferrals: **D20**, the detector's per-page
statistics, which are collapsed into one of four reason strings and several
of which the crate already marks dead; and **U4**, `pages_sampled` /
`pages_with_text`, which are on `PdfTypeResult` and reachable with a
second detection call this service chooses not to make. (`ocr_recommended`
was the third, and is wired now: see the routing review above.)

### What the patches did not change

The FULL layout verdict and the per-page OCR reasons are computed from the
runs this service renders from, rather than from the runs the analysis pass
used to build for itself. The analysis pass filtered folio context out of
its items first and suppressed the text of pages whose CID passthrough
produced garbage; this does neither. The verdicts can therefore differ from
the ones the deleted pass would have given on a document where that
filtering mattered. It is the more direct answer of the two: it describes
the runs that were actually delivered.

## Needs a typed home in the Document schema

Nothing. Every row this ledger has carried under that heading landed in the
canonical schema and is wired: `DocumentMeta.format_version`, `.structured`,
`.authoring_tool`, `.subject`, `.protection`, `.raw_metadata` and `.trapped`
(gRParse `af1b769` and `4f6ad0d`); `DocumentOrigin.source_id`;
`PageItem.page_label`, `.media_size` and `.user_unit`;
`PictureItem.hyperlink` and `.target`.

Nothing is deferred on reachability either. The asks above are wired,
and what remains below is deliberate: things that are reachable and homed
and not worth doing yet.

## Deliberate, and cheap to add later

Several of these say "the crate does not emit it". That is no longer a wall,
because the crate is vendored and patchable here; it is a cost. The bar for
a patch is the one in `AGENTS.md`: additive, visible in its own commit, and
never a change to what the crate parses. A row below that needs the crate to
compute something new rather than to return something it already computes is
the one to be careful with.

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

- **Separating hidden runs from visible ones.** The parser has one switch
  for the invisible layer and it governs the whole walk, so the hidden runs
  are the multiset difference between a walk that kept them and one that
  did not, keyed on page, text and box. Both walks read the same operators
  in the same order and the skip branch advances the text matrix exactly as
  the keep branch does, so a visible run is identical in both. The
  exception is a hidden run close enough to a visible one for the extractor
  to join them into a single item: that item is in neither walk unchanged,
  so it reads as invisible. Its text and its box are still exactly what the
  page drew, and over-reporting is the safe direction for a field whose
  purpose is that nothing hides.
- **What a table's column boundaries mean.** They are the detector's own
  numbers and the three detectors do not agree: a ruled table reports the
  rules, so a two-column grid has three boundaries, while a table found
  from alignment reports where each column's text starts. `TableRegion.bbox`
  is measured from the runs either way, so the table's own edges are exact
  in both cases.
- **Table headers.** The detector reports no header row. The first row of a
  data table is marked `column_header` because that is the convention the
  crate's own markdown renderer follows when it prints the grid; a table of
  contents gets no header row at all.
- **Row and column spans.** Every cell is 1x1. Spans need the vector-grid
  route (`detect_vector_grid_in_region_mem` feeding
  `extract_tables_with_structure_cells_mem`), which is a larger job and is
  only available for border-drawn tables. The geometry those entry points
  need does reach this service now, so the job is no longer blocked; it is
  just still a job.
- **Locating a block among its runs.** The fold matches a block of markdown
  to the runs behind it by following its letters and digits from run to
  run, so a paragraph whose lines the extractor interleaved with the
  neighbouring column's is found as a chain rather than missed as a
  stretch. A run is offered to one block only, from its unclaimed
  beginning first, so two identical lines match their own runs and a
  short label does not take letters out of a paragraph that contains the
  same word. What still goes unlocated is a block whose letters the
  rendering changed, or a chain the search could not finish inside its
  budget, and an unlocated block gets a page-only provenance entry rather
  than a wrong box.
- **Blocks set side by side.** The renderer assembles a line from the runs
  sharing a baseline, so a caption beside the prose wrapped around its
  figure comes out of it as one block whose words alternate between the
  two. The fold cuts such a block back onto the runs that drew each side
  when a gutter no run crosses separates them and their lines alternate,
  and each side keeps the renderer's own characters in the renderer's own
  order. Two blocks whose lines never alternate, a marker beside the first
  line of its item or a label beside a one-line value, stay one block, as
  does anything the renderer assembled across a gutter narrower than six
  points.
- **Where a picture sits.** A picture is placed in reading order before the
  first located block below it that shares its width, so a figure between
  two paragraphs of its column stands between them. On a page whose blocks
  could not be located, or below the last block of its column, it stands
  at the end of the page. Its box is exact either way.
- **Form XObjects as figures.** A form is a picture when it is a region of
  the page and not the page's own wrapper, is no smaller than a figure,
  sits inside no form that already counts, and holds no image the page
  reports already. A text box drawn as a form passes those tests too, and
  becomes a picture beside the text it holds, which still arrives as text.
  With no page size to judge by, the largest form on a page stands.
- **The furniture report.** The parser's own stripper is heard only on a
  page's edge lines: the outermost lines set off by white space or by a
  smaller face. A page whose body repeats page after page, a form printed
  many times over or a sample of one page copied, is convicted whole by a
  classifier that proves chrome by repetition, and the verdict on its body
  is set aside. A same-face first line with no gap under it is body text,
  whatever it says.
- **Grids trimmed to their rules.** The line detector takes the nearest
  rule above a grid as its top, and a page that underlines its title puts a
  rule there, so the paragraph between the title and the table became the
  table's first row. A leading or trailing row whose boundary lies beyond
  every vertical rule of the grid, and whose cells are all but one empty,
  is dropped and its runs go back to the page. A header populated across
  its columns above an open-edged grid stays, because that is the shape
  the detector accepts it for; a title row over a ruled grid with one cell
  filled would stay too, and is not one this corpus has.
- **Glued list markers.** A list set tight enough that the number and the
  first word of an item came out of the extractor as one run is printed by
  the renderer as the run had it, `2.minimize`, and the fold reads that as
  a marker when the line before or after it carries one too. A single such
  line is prose, which is what a version string or a sentence beginning
  with a number is.
- **Per-page rendering and document-wide stripping.** Markdown is rendered
  one page at a time, which is what keeps the stream a stream. The
  cross-page repetition classifier behind the header and footer stripper
  has no cross-page evidence in that mode, so it fires less than it would
  on a whole-document render. Less silent deletion, and the same
  `report_furniture` names whatever still goes.

## Vendored dependency moves (owner decisions, not re-vendors)

- *Patch* (lopdf 0.44, 2026-09-02): the vendored crate's lopdf moved from
  0.42.0 to 0.44.0 at the owner's request, ahead of upstream pdf-inspector.
  One call site changed: `Document::get_page_content` lost a `Result` that
  was vestigial (in 0.42 the body already skipped unreadable stream objects
  and used raw bytes when a decode failed; the only error path was
  `write_all` into a `Vec`, which cannot fail), so
  `extractor/content_stream.rs` no longer maps an error that could not
  happen. Same bytes out; the crate suite keeps 1015 passed / 14
  fixture-missing and the service suite keeps 228.
- *Patch* (pyo3 0.29, 2026-09-02): the vendored crate's pyo3 moved from
  0.28 to 0.29 at the owner's request. `Python::allow_threads` became
  `Python::detach` (a rename with the same GIL semantics); the two call
  sites in `python.rs` follow it. The `python` feature is outside the
  service's default gate, so it was checked directly: `cargo check
  --features python` compiles with zero errors and the crate's usual
  warning posture.
