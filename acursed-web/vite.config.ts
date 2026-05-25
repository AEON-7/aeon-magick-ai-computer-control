import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()],
  server: {
    // During `npm run dev` the SvelteKit app runs on :5173 and proxies API
    // calls to a running acursed-supervisor (HTTPS, self-signed).
    proxy: {
      '/api': {
        target: process.env.VITE_API_TARGET || 'https://acursed.local',
        changeOrigin: true,
        secure: false,
        ws: true,
      },
    },
  },
});
