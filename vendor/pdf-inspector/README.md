# Vendored: pdf-inspector 1.17.0

This directory is firecrawl's `pdf-inspector` crate, version **1.17.0**, copied
verbatim from crates.io (`~/.cargo/registry/src/index.crates.io-*/pdf-inspector-1.17.0`)
and then patched here. It is MIT licensed; `LICENSE` is the crate's own and is
unchanged. The firecrawl monorepo is AGPL and is not involved: this is the
published crate only.

## Why it is vendored

`docs/capture-deferrals.md` in the repo root carried four capture rows that were
blocked on nothing but visibility. Each one is data this crate already computes
and does not return, and each one was reachable only by an upstream change. The
crate's licence permits making that change here, so it is made here.

The private APIs, and what each unblocks:

1. **`include_invisible` and `skipped_invisible`** (`src/extractor/`,
   `src/extractor/content_stream.rs`). The content-stream walker recognises
   text drawn with rendering mode 3 and takes a parameter deciding whether to
   keep it, but every public entry point passed `false` and every caller
   discarded the `skipped_invisible` flag the walk returned. An OCR-under-image
   text layer or a hidden watermark could therefore not be emitted, and could
   not even be reported as having existed.
2. **The per-page text-quality analysis** (`src/text_quality.rs`).
   `analyze_text_quality`, its report and the letter-frequency correlation
   behind the garble verdict were all `pub(crate)`, so only the derived boolean
   escaped and `PageQuality.garble_score` had no source.
3. **The rect and line accessor** (`src/extractor/mod.rs`). `PdfRect` and
   `PdfLine` are public types, and so are the rect- and line-driven table
   detectors, but the memory-based accessor returning the geometry they run on
   was `pub(crate)`. Both detectors were therefore unreachable, so a table drawn
   with real rules was detected no better than a borderless one.
4. **The column detector** (`src/extractor/layout.rs`). `detect_columns` and
   `ColumnRegion` were `pub(crate)`, so layout complexity could not be
   recomputed from items a caller already holds, and FULL mode had to run a
   whole second read of the file to get the answer.
5. **The header, footer and folio stripper** (`src/markdown/mod.rs`).
   `strip_repeated_header_footer_lines` was `pub(crate)`, so a caller could not
   ask which lines the crate judges to be page furniture. The service renders
   one page at a time, and the classifier behind that function proves furniture
   by repetition across pages, so on the service's own calls it can prove
   nothing: the judgement had to be reachable over the whole document
   separately from the rendering, or be reinvented outside the parser.
6. **Form XObject placements** (`src/extractor/xobjects.rs`,
   `src/extractor/content_stream.rs`, `src/extractor/mod.rs`). The walker
   entered every Form XObject to read its text and knew, at the `Do`, the
   form's `/BBox` and the transformation in force, and reported neither. A
   figure included from another PDF is a form of paths with no image in it,
   so nothing said where it was drawn.
   `extract_text_with_positions_rects_and_forms_mem_with_invisible` returns a
   `PdfForm` per invocation beside the runs, rectangles and lines its sibling
   returns; the sibling entry points and the page walker's old name return
   exactly what they did.
7. **The OCR-layer fallback on the positioned runs** (`src/extractor/mod.rs`,
   `src/lib.rs`, `src/types.rs`). The region extractor adopts a scanned
   page's invisible OCR layer when the visible walk found no text, and the
   whole-document pipeline retries a mixed document the same way, but no
   entry point returning positioned runs did either.
   `extract_text_with_positions_rects_and_forms_mem_with_ocr_layer` applies
   the region extractor's own gate and thresholds (now shared as
   `has_visible_text` and `is_adoptable_ocr_layer`) page by page and returns
   an `OcrLayerExtraction` naming the pages whose runs are their layer.
8. **Detection's OCR recommendation** (`src/lib.rs`). `PdfTypeResult`
   carried `ocr_recommended` and `PdfProcessResult` dropped it;
   `PdfProcessResult.ocr_recommended` carries it.

## Patches that change what the crate does

Two patches go further than visibility, because the service could not be
made safe or correct from outside the crate. Both are recorded with their
reasons in `docs/capture-deferrals.md`.

- **Detection follows the text rendering mode** (`src/detector.rs`). The
  byte scanner counted every show operator as text, including the hundreds
  of invisible (Tr 3) ones behind a searchable scan's page image, so such a
  scan classified text-based with no OCR pages. Invisible show operators
  are now counted apart, an image page whose text is all invisible is a
  scan carrying an OCR layer (and its document is Mixed), and the per-page
  OCR list is built for text-based documents too, not only mixed ones.
- **Decoding is bounded** (`src/guard.rs`, and every decoding call site).
  Every stream the crate decodes goes through the guard module, which holds
  it to a per-stream ceiling and a per-run budget, loads documents with the
  ceiling as lopdf's `max_decompressed_size`, and lets a caller's
  `ParseGuard` stop a run at a deadline or on cancellation at page
  boundaries. `PdfError` gained an `Interrupted` variant for it, which is
  the one change to a public type's shape.

## How the patches are kept auditable

The copy landed in its own commit, with the tree building and testing
identically to the registry build, before any API was touched. Each patch is a
separate commit after it, listed by SHA in `docs/capture-deferrals.md`. The
visibility patches are additive: nothing that was public changed shape. The
two patches under "Patches that change what the crate does" are the
exceptions, with their reasons there, and they bring their own tests beside
the ones the crate shipped with. One more exception is listed there under
"Vendored dependency moves": the lopdf dependency moved ahead of upstream at
the owner's request, with the one call site the API change reached.

The crate keeps its own style, its own formatting and its own lint posture.
Files here are not reformatted to match the service.

## Re-vendoring a newer release

Copy the new version over this directory, keep this README and re-apply the
patches by reading the commits named in `docs/capture-deferrals.md`. Do not
merge the service's style into the crate.
