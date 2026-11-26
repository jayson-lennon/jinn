// The /web2 client build. Plain Vite mode (no `start`): jinn serves its own
// page shell, so the build only has to emit one JavaScript file and one
// stylesheet with fixed names the Rust server embeds.
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
    rollupOptions: {
      input: 'src/index.tsx',
      output: {
        entryFileNames: 'client2.js',
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
