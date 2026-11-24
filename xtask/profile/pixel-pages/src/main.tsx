import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { useState } from "react";
import { Box, Image, Text, createRoot } from "@zenbu-labs/pixel";
import type { NodeHandle } from "@zenbu-labs/pixel";

type Page = { file: string; width: number; height: number };

const ARROW_ROWS = 3;

const pdf = process.argv[2];
if (!pdf) throw new Error("usage: pixel . -- <file.pdf>");

let list: NodeHandle | null = null;
let offset = 0;

const root = createRoot({
  name: "pixel-pages",
  devtools: false,
  onKey(event) {
    if (event.kind === "release") return true;
    const cell = root.info.cellHeight;
    const visible = root.info.height - cell;
    if (event.key === "q" || (event.mods.ctrl && event.key === "c")) {
      root.stop();
    } else if (event.key === "down") {
      list?.scrollBy(ARROW_ROWS * cell);
    } else if (event.key === "up") {
      list?.scrollBy(-ARROW_ROWS * cell);
    } else if (event.mods.ctrl && event.key === "d") {
      list?.scrollBy(Math.floor(visible / 2));
    } else if (event.mods.ctrl && event.key === "u") {
      list?.scrollBy(-Math.floor(visible / 2));
    } else if (event.key === "j") {
      const next = tops.find((top) => top > offset + 1);
      if (next !== undefined) list?.scrollTo(next);
    } else if (event.key === "k") {
      const previous = [...tops].reverse().find((top) => top < offset - 1);
      list?.scrollTo(previous ?? 0);
    } else if (event.key === "g") {
      list?.scrollTo(0);
    } else if (event.key === "G" || (event.mods.shift && event.key === "g")) {
      list?.scrollTo(tops[tops.length - 1] ?? 0);
    }
    return true;
  },
});

root.setClearColor([0, 0, 0, 255]);

const gap = root.info.cellHeight;
const pages = renderPages(pdf, root.info.width);
const tops = pages.reduce<number[]>((acc, page, index) => {
  acc.push(index === 0 ? 0 : acc[index - 1] + pages[index - 1].height + gap);
  return acc;
}, []);

const infoPath = process.env.PIXEL_PAGES_INFO;
if (infoPath) {
  fs.writeFileSync(infoPath, JSON.stringify({ ...root.info, displayScale: root.displayScale, pages: pages.length }));
}

function renderPages(file: string, width: number): Page[] {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "pixel-pages-"));
  execFileSync("mutool", ["draw", "-q", "-F", "png", "-w", String(width), "-o", path.join(dir, "page-%d.png"), file]);
  return fs
    .readdirSync(dir)
    .map((name) => ({ name, number: Number(/page-(\d+)\.png/.exec(name)?.[1]) }))
    .filter(({ number }) => Number.isFinite(number))
    .sort((a, b) => a.number - b.number)
    .map(({ name }) => {
      const png = fs.readFileSync(path.join(dir, name));
      return { file: path.join(dir, name), width: png.readUInt32BE(16), height: png.readUInt32BE(20) };
    });
}

function pageAt(position: number): number {
  let current = 0;
  tops.forEach((top, index) => {
    if (top <= position + 1) current = index;
  });
  return current + 1;
}

function App() {
  const [page, setPage] = useState(1);
  return (
    <Box style={{ flexDirection: "column", width: "100%", height: "100%", background: "#000000", opaque: true }}>
      <Box
        ref={(handle) => {
          list = handle;
        }}
        style={{ flexGrow: 1, flexDirection: "column", overflow: "scroll", layout: "list", gap, opaque: true }}
        onScroll={(event) => {
          offset = event.offset;
          setPage(pageAt(event.offset));
        }}
      >
        {pages.map((p) => (
          <Image key={p.file} src={p.file} style={{ width: p.width, height: p.height }} />
        ))}
      </Box>
      <Box style={{ height: root.info.cellHeight, flexShrink: 0 }}>
        <Text style={{ color: "#c0c0c0", fontSize: Math.round(root.info.cellHeight * 0.75) }}>
          {`page ${page}/${pages.length} · ${path.basename(pdf)}`}
        </Text>
      </Box>
    </Box>
  );
}

root.render(<App />);
