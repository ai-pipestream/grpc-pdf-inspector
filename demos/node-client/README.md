# Node demo client

A live web viewer for the grpc-pdf-inspector server. Stubs are loaded
dynamically from [`../../proto`](../../proto) at run time, so nothing
generated is checked in.

```bash
npm install

# Web viewer, then open http://127.0.0.1:8093
npm start
```

`npm start` first runs `scripts/make-samples.mjs`, which writes the sample
documents into [`../sample-data`](../sample-data). They are generated rather
than committed: this repository keeps no binaries, and the documents mirror
the fixtures the Rust test suite authors in memory with lopdf.

The bridge honours `PDF_ADDR` (default `127.0.0.1:50067`) and `PORT`
(default `8093`).

### Serving under a base path

Set `UI_BASE` and the whole viewer moves under that prefix, for example
behind a reverse proxy that forwards `/ui/pdf/*` unchanged:

```bash
UI_BASE=/ui/pdf npm start   # page at http://127.0.0.1:8093/ui/pdf/
```

The bridge strips the prefix before routing, so every endpoint lives at
`$UI_BASE/api/*`, and it injects a `<meta name="ui-base">` tag into the
served page, which the page reads to prefix its own `fetch()` calls. Unset,
nothing changes: the bridge answers at the root exactly as before. This is
how the shared demo shell mounts the viewer as its "pdf" tab, matching the
`/ui/pdf` path the service advertises in its `UiInfo`.

## The web viewer

The viewer exists to make the service's one promise visible: **the
classification — the routing answer — arrives first and costs almost
nothing**. A PDF's cross-reference table is at the end of the file, so
nothing can begin until the upload bar fills; the moment it does, `info`
arrives (the page reports the milliseconds from the last uploaded byte to
the classification, and the server reports its own `detection_time_ms`),
and only then does extraction begin, one `page` of markdown at a time.

It is a single HTTP request. The browser POSTs the PDF and reads
Server-Sent Events off the *same* response, which is deliberately the same
shape as the gRPC call underneath it. Nothing buffers the document in the
bridge: each upload slice is written into the gRPC call as it lands, and
each event is flushed to the page as the Rust server emits it.

Worth trying:

| Document | What you see |
|---|---|
| `hello-text.pdf` | `text-based`, two pages of markdown streaming in |
| `long-text.pdf` | twelve pages, enough to watch the page stream |
| `scanned-image.pdf` | classified `scanned` just as fast — but no pages stream; every page is named in `pages_needing_ocr` |
| `mixed.pdf` | classified `mixed`: text pages stream; the image pages are named for OCR |
| detect mode | the stream stops after `info` + `status`: the ~10–50ms routing answer, nothing extracted |

Checking **document fold** adds the `document` event: the whole parse
projected into one `ai.pipestream.document.v1.Document`, summarised on the
page as a name and item counts.

### A real document

The generated fixtures are small on purpose. Drop any real `.pdf` into
[`../sample-data/large/`](../sample-data) (gitignored) and it appears in the
dropdown, or use the file picker for something on your disk.

### Why SSE is parsed by hand

`EventSource` only does `GET`, and the whole point is that the upload and
the event stream are one request. So the page reads `response.body` as a
stream and splits frames itself. It is about fifteen lines and it is in
`stream()` in `public/index.html`.

## Things that bite

**Send the options frame first.** The first frame of the request stream must
carry `options`; every frame after it is a `chunk`. `lib/pdf.js` writes
options inside `openParse()` so the ordering cannot be got wrong by a
caller.

**The upload completes before the first event, and that is the format, not
the server.** Do not "fix" a client for what looks like a stalled stream
mid-upload. The demo's timing stat exists to show how small the real
classification latency is once the document is whole.

**Handle the oneof by name, not by guessing.** With `oneofs: true`,
proto-loader sets `message.event` to the name of the active arm. The bridge
forwards `message[message.event]` rather than sniffing which key is
populated, so an arm added to the contract later is passed through to the
page instead of being dropped silently.

**Backpressure is real and worth keeping.** `res.write()` returning false
means the browser is behind. The bridge pauses the gRPC call and resumes on
`drain`, which propagates through gRPC flow control back to the server.
