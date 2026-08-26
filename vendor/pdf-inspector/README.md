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

The four private APIs, and what each unblocks:

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

## How the patches are kept auditable

The copy landed in its own commit, with the tree building and testing
identically to the registry build, before any API was touched. Each of the four
patches is a separate commit after it, listed by SHA in
`docs/capture-deferrals.md`. Every patch is additive: nothing that was public
changed shape, and the crate's own tests are the ones it shipped with.

The crate keeps its own style, its own formatting and its own lint posture.
Files here are not reformatted to match the service.

## Re-vendoring a newer release

Copy the new version over this directory, keep this README and re-apply the four
patches by reading the commits named in `docs/capture-deferrals.md`. Do not
merge the service's style into the crate.
