// SPDX-License-Identifier: Apache-2.0
//
// Thin wrapper around the ai.pipestream.pdf.v1 gRPC contract.
//
// The protos are loaded dynamically from ../../proto (the single source of
// truth in this repository) — no generated code is checked in.

import { fileURLToPath } from "node:url";
import path from "node:path";
import grpc from "@grpc/grpc-js";
import protoLoader from "@grpc/proto-loader";

const PROTO_ROOT = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..", "..", "..", "proto",
);

const packageDefinition = protoLoader.loadSync(
  path.join(PROTO_ROOT, "ai", "pipestream", "pdf", "v1", "pdf_service.proto"),
  {
    includeDirs: [PROTO_ROOT],
    keepCase: false,
    longs: Number,
    enums: String,
    defaults: true,
    oneofs: true,
  },
);

const { ai } = grpc.loadPackageDefinition(packageDefinition);
const PdfParseService = ai.pipestream.pdf.v1.PdfParseService;

/** Upload chunk size. Any value gives the same events; this one is quick. */
export const CHUNK_BYTES = 64 * 1024;

/** A connected grpc-pdf-inspector client. */
export class PdfClient {
  /** @param {string} address host:port of the grpc-pdf-inspector server. */
  constructor(address = process.env.PDF_ADDR ?? "127.0.0.1:50067") {
    this.stub = new PdfParseService(
      address,
      grpc.credentials.createInsecure(),
    );
  }

  /**
   * Open a ParsePdf call and send the options frame.
   *
   * The caller then writes `{ chunk }` frames as the PDF becomes available
   * and calls `.end()`. The server buffers the whole upload before it emits
   * anything — the cross-reference table of a PDF is its last bytes, so no
   * page can be located until the upload is complete — but it still reads
   * the request stream rather than a length, so the options-first ordering
   * is the caller's job.
   *
   * @param {object} options a PdfOptions message.
   * @returns {object} the duplex call.
   */
  openParse(options) {
    const call = this.stub.parsePdf();
    call.write({ options });
    return call;
  }

  close() {
    grpc.closeClient(this.stub);
  }
}

/**
 * Reduce one response event to what the demo page draws.
 *
 * `page` carries the page's markdown verbatim, which the page does not need
 * whole: it becomes its length plus a short preview. `info` and `status`
 * forward whole, `document` becomes a name-and-counts summary (the fold is
 * a whole-document message the page only needs to prove arrived), and an
 * arm this client has never heard of forwards whole too, per the contract's
 * ignore-unknown rule.
 *
 * @param {object} response a ParsePdfResponse.
 * @returns {[string, object] | null} the event name and its summary, or null
 *   for a response with no event set.
 */
export function summarizeEvent(response) {
  const kind = response.event;
  if (!kind) return null;
  const payload = response[kind] ?? {};

  if (kind === "page") {
    const markdown = payload.markdown ?? "";
    return [kind, {
      pageNo: payload.pageNo,
      chars: markdown.length,
      preview: clip(markdown.replace(/\s+/g, " ").trim()),
    }];
  }
  if (kind === "document") {
    return [kind, {
      name: payload.name ?? "",
      texts: (payload.texts ?? []).length,
      pages: Object.keys(payload.pages ?? {}).length,
    }];
  }
  return [kind, payload];
}

function clip(text, max = 200) {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}
