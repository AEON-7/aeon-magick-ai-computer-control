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
      keyframes: {
        // The orb's slow "breathing" halo (transform+opacity only — both
        // compositor-composited, so the loop costs no main-thread time).
        'orb-breathe': {
          '0%,100%': { transform: 'scale(1)', opacity: '0.3' },
          '50%': { transform: 'scale(1.3)', opacity: '0.55' },
        },
        // Signal-lost flicker — mostly steady with a brief stutter.
        'orb-flicker': {
          '0%,92%,100%': { opacity: '0.7' },
          '94%': { opacity: '0.3' },
          '96%': { opacity: '0.8' },
        },
        // Gentler stand-in for animate-pulse on armed/destructive buttons.
        ember: {
          '0%,100%': { opacity: '1' },
          '50%': { opacity: '0.65' },
        },
        'cursor-blink': {
          '0%,49%': { opacity: '1' },
          '50%,100%': { opacity: '0' },
        },
      },
      animation: {
        'orb-breathe': 'orb-breathe 4s ease-in-out infinite',
        'orb-flicker': 'orb-flicker 4s steps(1) infinite',
        ember: 'ember 2.4s ease-in-out infinite',
        'cursor-blink': 'cursor-blink 1.2s steps(1) infinite',
      },
    },
  },
  plugins: [],
};
