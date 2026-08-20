// SPDX-License-Identifier: Apache-2.0
//
// Writes the demo's sample PDFs into ../sample-data.
//
// This repository commits no binaries — the Rust test suite authors its
// fixtures in memory with lopdf, and the demo follows the same rule by
// *generating* its documents here instead of checking them in. A minimal
// PDF is a short text container with a byte-offset table, so the writer
// below is about sixty lines of stdlib node rather than a dependency.
//
// The documents mirror tests/common/mod.rs: `hello-text.pdf` and
// `long-text.pdf` are text_pdf fixtures (real text operators, base-14
// Helvetica), `scanned-image.pdf` is image_pdf (a full-page raster and no
// text layer, which is what a scan is as far as classification is
// concerned), and `mixed.pdf` interleaves the two so the page shows a
// `pages_needing_ocr` list with something in it.

import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const OUT_DIR = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..", "..", "sample-data",
);

// --- Minimal PDF writer: objects, xref table, trailer -----------------------

/**
 * Serialize a document from its parts. `objects` is a 1-indexed list of
 * object bodies (strings or Buffers); the catalog must be the last one so
 * the trailer can name it. No compression, no object streams — a PDF a
 * reader cannot choke on beats a small one.
 */
function buildPdf(objects) {
  const chunks = [Buffer.from("%PDF-1.5\n%\xE2\xE3\xCF\xD3\n", "latin1")];
  const offsets = [0]; // object numbers are 1-indexed
  let at = chunks[0].length;

  objects.forEach((body, index) => {
    const head = Buffer.from(`${index + 1} 0 obj\n`);
    const tail = Buffer.from("\nendobj\n");
    offsets.push(at);
    chunks.push(head, Buffer.isBuffer(body) ? body : Buffer.from(body), tail);
    at += head.length + body.length + tail.length;
  });

  const xrefAt = at;
  let xref = `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (let n = 1; n <= objects.length; n++) {
    xref += `${String(offsets[n]).padStart(10, "0")} 00000 n \n`;
  }
  xref += `trailer\n<< /Size ${objects.length + 1} /Root ${objects.length} 0 R >>\n`;
  xref += `startxref\n${xrefAt}\n%%EOF\n`;
  chunks.push(Buffer.from(xref));

  return Buffer.concat(chunks);
}

/** Assemble a document from per-page descriptors into the object list. */
function assemble(pages) {
  const objects = [];
  const fontId = 1;
  objects[fontId - 1] =
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

  const kids = [];
  for (const page of pages) {
    let contents;
    const resources = { font: true };
    if (page.kind === "text") {
      contents = textContent(page.marker, page.page, page.bodyWords);
    } else {
      // A full-page raster and nothing else: what a scan looks like to a
      // classifier. An 8x8 gray square scaled over the whole MediaBox.
      const imageId = objects.length + 1;
      objects[imageId - 1] = imageObject();
      contents = "q 612 0 0 792 0 0 cm /Im1 Do Q";
      resources.xobject = imageId;
    }
    const contentId = objects.length + 1;
    objects[contentId - 1] = streamObject(Buffer.from(contents));
    const pageId = objects.length + 1; // filled in below, once pagesId known
    kids.push({ pageId, contentId, resources });
    objects[pageId - 1] = null;
  }

  const pagesId = objects.length + 1;
  for (const { pageId, contentId, resources } of kids) {
    const xobject = resources.xobject
      ? ` /XObject << /Im1 ${resources.xobject} 0 R >>`
      : "";
    objects[pageId - 1] =
      `<< /Type /Page /Parent ${pagesId} 0 R /MediaBox [0 0 612 792]` +
      ` /Resources << /Font << /F1 ${fontId} 0 R >>${xobject} >>` +
      ` /Contents ${contentId} 0 R >>`;
  }
  objects[pagesId - 1] =
    `<< /Type /Pages /Kids [${kids.map((k) => `${k.pageId} 0 R`).join(" ")}]` +
    ` /Count ${kids.length} >>`;
  objects.push(`<< /Type /Catalog /Pages ${pagesId} 0 R >>`);

  return buildPdf(objects);
}

function streamObject(data) {
  return Buffer.concat([
    Buffer.from(`<< /Length ${data.length} >>\nstream\n`),
    data,
    Buffer.from("\nendstream"),
  ]);
}

function imageObject() {
  const pixels = Buffer.alloc(64, 0x80);
  return Buffer.concat([
    Buffer.from(
      "<< /Type /XObject /Subtype /Image /Width 8 /Height 8" +
      " /ColorSpace /DeviceGray /BitsPerComponent 8" +
      ` /Length ${pixels.length} >>\nstream\n`,
    ),
    pixels,
    Buffer.from("\nendstream"),
  ]);
}

/** One page of real text operators, mirroring text_pdf in the Rust suite. */
function textContent(marker, page, bodyWords) {
  const body = "lorem ipsum dolor sit amet ".repeat(Math.floor(bodyWords / 5) + 1);
  // One Tj per line; long bodies are split so no line runs off the page.
  let content = `BT /F1 12 Tf 50 750 Td\n(${marker} page ${page}) Tj\n`;
  let rest = body;
  while (rest.length > 0) {
    const line = rest.slice(0, 80).replace(/[()\\]/g, "");
    rest = rest.slice(80);
    content += `0 -14 Td (${line}) Tj\n`;
  }
  return content + "ET";
}

// --- The documents ----------------------------------------------------------

const textPages = (count, bodyWords, marker) =>
  Array.from({ length: count }, (_, i) => ({
    kind: "text", page: i + 1, bodyWords, marker,
  }));

const DOCUMENTS = [
  ["hello-text.pdf", () => assemble(textPages(2, 12, "hello from the demo,"))],
  ["long-text.pdf", () => assemble(textPages(12, 120, "the long document,"))],
  ["scanned-image.pdf", () => assemble(
    Array.from({ length: 3 }, () => ({ kind: "image" })),
  )],
  ["mixed.pdf", () => assemble([
    ...textPages(2, 30, "the mixed document,"),
    { kind: "image" },
    { kind: "image" },
    ...textPages(1, 30, "the mixed document,").map((p) => ({ ...p, page: 5 })),
    { kind: "image" },
  ])],
];

mkdirSync(OUT_DIR, { recursive: true });
for (const [name, build] of DOCUMENTS) {
  const bytes = build();
  const file = path.join(OUT_DIR, name);
  writeFileSync(file, bytes);
  console.log(`wrote ${path.relative(process.cwd(), file)} (${bytes.length} bytes)`);
}
