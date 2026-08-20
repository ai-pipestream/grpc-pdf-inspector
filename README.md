# grpc-pdf-inspector

A gRPC server that classifies PDFs in memory and streams per-page markdown
back. It wraps firecrawl's MIT-licensed
[`pdf-inspector`](https://crates.io/crates/pdf-inspector) crate (default
features only: pure Rust, lopdf + rayon, no models) behind the ai-pipestream
fleet's collector conventions.

It is the fleet's cheap routing answer for PDF:

- **Text-based** PDFs carry a real text layer. Classification says so in
  ~10–50ms and the markdown streams out page by page.
- **Scanned / image-based** PDFs have no usable text layer. Classification
  says so, `pages_needing_ocr` names the pages, and the caller routes only
  those to a heavy OCR path (e.g. gRParse's ONNX engines). This server never
  OCRs.
- **Mixed** PDFs get both: text pages stream, the rest are reported.

Nothing is written to disk at any point: the upload lives in one `Vec<u8>`
and every library call is a `*_mem` entry point.

## The stream

`ParsePdf` is a bidirectional stream. The first request frame carries
`options`; the rest carry `chunk`s of PDF bytes. The upload is fully received
before the first event — a PDF's cross-reference table is at the end of the
file, so no page is locatable until the last byte has arrived — and streaming
begins the moment it is possible:

```text
info      always first: pdf_type, confidence, page_count, title,
          pages_needing_ocr + reasons, detection_time_ms
page      FULL mode, text-bearing documents only: one per page,
          1-indexed, in requested page order
status    trailer: pages_extracted, warnings, layout complexity,
          has_encoding_issues, total processing_time_ms
```

Modes (`options.mode`): `DETECT_ONLY` (classification only, the ~10–50ms
routing answer), `ANALYZE` (classification + layout/encoding analysis, no
markdown), `FULL` (classification + per-page markdown; the default).

Errors: oversize upload → `RESOURCE_EXHAUSTED`; not-a-PDF / truncated /
malformed / encrypted-without-password / page 0 → `INVALID_ARGUMENT`;
parser panic → `INTERNAL`. Events already delivered before a failure remain
valid.

Passwords: supply `options.password` for an encrypted PDF. The library's
per-page extraction API takes no password, so FULL mode with a password
falls back to whole-document extraction — one `page` event with `page_no` 0
plus a `PARSE_WARNING_CODE_PASSWORD_FALLBACK` warning.

Page indexing: the wire is 1-indexed everywhere. The library's per-page
extraction API is 0-indexed; the conversion lives in `src/parse.rs`, at the
library boundary, and nowhere else.

## Run

```sh
cargo run --release
# or
docker build -t grpc-pdf-inspector .
docker run --rm --read-only --cap-drop ALL --security-opt no-new-privileges \
  -p 50067:50067 grpc-pdf-inspector
```

Configuration is environment-only:

| Variable | Default | Meaning |
|---|---|---|
| `GRPC_PDF_ADDR` | `0.0.0.0:50067` | Listen address. |
| `GRPC_PDF_MAX_BYTES` | `134217728` (128 MiB) | Largest accepted upload. |
| `GRPC_PDF_MAX_CHUNK_BYTES` | `16777216` (16 MiB) | Largest single `chunk` frame. |
| `GRPC_PDF_MAX_CONCURRENT_PARSES` | `8` | Concurrent parse calls; further calls wait. |
| `GRPC_PDF_WORKERS` | CPU count | Tokio worker threads. |
| `GRPC_PDF_WINDOW_BYTES` | `4194304` | HTTP/2 initial window (stream and connection). |
| `GRPC_PDF_METRICS_INTERVAL_SECS` | `60` | Seconds between metrics lines; 0 disables. |

The server also registers `grpc.health.v1.Health` and v1 server reflection
(with the health descriptor included, so `grpcurl` can probe it):

```sh
grpcurl -plaintext localhost:50067 list
grpcurl -plaintext localhost:50067 grpc.health.v1.Health/Check
```

## Develop

```sh
cargo test          # the suite, incl. the stream-liveness tests
cargo clippy --all-targets -- -Dwarnings
cargo fmt --check
```

Protobuf codegen is dev-time only (no `build.rs`, no protoc):

```sh
cargo install protoc-gen-prost protoc-gen-tonic   # once
buf lint && buf generate && buf build -o src/gen/file_descriptor_set.binpb
```

The generated Rust and the descriptor set are checked in under `src/gen/`;
regenerate after any change under `proto/`. Tests author every PDF fixture in
memory with lopdf; nothing binary is committed.

## License

Apache-2.0 (this repo). The wrapped `pdf-inspector` crate is MIT.
