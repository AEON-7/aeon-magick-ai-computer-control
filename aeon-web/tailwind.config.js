/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{svelte,html,ts}'],
  theme: {
    extend: {
      colors: {
        // Aeon Cursed palette — pure dark with violet accent
        ink: {
          950: '#08080d',
          900: '#0c0d14',
          800: '#13141d',
          700: '#1a1c28',
          600: '#262837',
        },
        // Cursed violet
        cursed: {
          300: '#c4b5fd',
          400: '#a78bfa',
          500: '#8b5cf6',
          600: '#7c3aed',
          700: '#6d28d9',
          800: '#5b21b6',
        },
        // Live indicator green
        live: {
          400: '#34d399',
          500: '#10b981',
        },
      },
      fontFamily: {
        mono: ['"JetBrains Mono"', 'ui-monospace', 'monospace'],
        sans: ['"Inter"', 'system-ui', 'sans-serif'],
      },
    },
  },
  plugins: [],
};
