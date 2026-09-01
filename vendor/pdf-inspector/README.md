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

## How the patches are kept auditable

The copy landed in its own commit, with the tree building and testing
identically to the registry build, before any API was touched. Each patch is a
separate commit after it, listed by SHA in `docs/capture-deferrals.md`. Every patch is additive: nothing that was public
changed shape, and the crate's own tests are the ones it shipped with.

The crate keeps its own style, its own formatting and its own lint posture.
Files here are not reformatted to match the service.

## Re-vendoring a newer release

Copy the new version over this directory, keep this README and re-apply the
patches by reading the commits named in `docs/capture-deferrals.md`. Do not
merge the service's style into the crate.
