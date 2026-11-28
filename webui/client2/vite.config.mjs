// The web UI client build. Plain Vite mode (no `start`): jinn serves its own
// page shell, so the build only has to emit one JavaScript file, one
// stylesheet, and the service worker — each with a fixed name the Rust
// server embeds.
import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

export default defineConfig({
  plugins: [solid()],
  build: {
    outDir: 'dist',
    // Fixed, content-hash-free names on disk: the Rust build embeds these
    // exact paths and verifies each file against its sha256 pin, and the
    // page shell then names each asset by that pin
    // (`client2.<sha256>.js`), so a rebuilt bundle is a new URL.
    //
    // Two entries. The worker is its own entry because it runs with no DOM
    // and must not share a chunk with the page: a page module reaching into
    // a worker's scope, or the reverse, would be a bug at runtime that the
    // bundler would happily emit. Each entry names its own output file.
    rollupOptions: {
      input: { client2: 'src/index.tsx', sw: 'src/sw.ts' },
      output: {
        entryFileNames: (chunk) => (chunk.name === 'sw' ? 'sw.js' : 'client2.js'),
        assetFileNames: 'client2.[ext]',
      },
    },
    target: 'esnext',
    // Keep CSS and JS as separate files with the fixed names above.
    assetsInlineLimit: 0,
    // A library-free build warns that nothing is bundler-resolved here.
    reportCompressedSize: false,
  },
});
