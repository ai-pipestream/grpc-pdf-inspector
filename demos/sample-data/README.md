# Sample PDFs

The small documents here are **generated, not committed** — this repository
keeps no binaries. Regenerate them with:

```bash
cd ../node-client
npm run samples
```

| File | What it is |
|---|---|
| `hello-text.pdf` | a two-page text fixture like `text_pdf` in `tests/common/mod.rs` |
| `long-text.pdf` | twelve padded pages, enough to watch the page stream |
| `scanned-image.pdf` | three pages, each a full-page raster with no text layer — what a scan is to the classifier |
| `mixed.pdf` | text pages interleaved with image pages, so `pages_needing_ocr` has something to say |

`large/` is gitignored and meant for real documents (a report, a paper)
that do not belong in the repository. Anything `.pdf` dropped there appears
in the viewer's dropdown.
