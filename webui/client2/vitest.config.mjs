// The web UI client's unit tests. Vitest reads this file and never the
// production `vite.config.mjs`, so adding tests changes nothing the server
// embeds. The Solid plugin is here so the modules under test compile with
// the same pipeline the build uses.
import { defineConfig } from 'vitest/config';
import solid from '@solidjs/vite-plugin';

export default defineConfig({
  plugins: [solid()],
  // The `!markdown` showcase document lives in `crates/jinn-provider/`,
  // outside this directory, and Vite refuses a read beyond the project root
  // unless this is off. Test config only: the production build never reads
  // that file, and nothing of it reaches the bundle.
  server: { fs: { strict: false } },
  test: {
    include: ['src/**/*.test.ts'],
    // No in-source tests: a test lives in its own `*.test.ts` file.
    includeSource: [],
  },
});
