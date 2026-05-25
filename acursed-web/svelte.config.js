import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

const config = {
  preprocess: vitePreprocess(),
  kit: {
    // SvelteKit ships as a static SPA we copy into /usr/share/acursed/web/.
    // adapter-static + a fallback index.html keeps client-side routing working.
    adapter: adapter({
      pages: 'build',
      assets: 'build',
      fallback: 'index.html',
      precompress: false,
      strict: true,
    }),
    paths: {
      base: '',
    },
    alias: {
      $lib: 'src/lib',
    },
  },
};

export default config;
