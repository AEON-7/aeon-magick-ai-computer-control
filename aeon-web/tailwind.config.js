/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{svelte,html,ts}'],
  theme: {
    extend: {
      colors: {
        // Void chassis — cold near-black, slightly steel-tinted (not purple mud).
        // Use ink-950 as the page void; ink-900 panels; ink-800 wells; ink-700 hairlines.
        ink: {
          950: '#06060a',
          900: '#0a0b10',
          800: '#11131a',
          700: '#1a1c26',
          600: '#262a36',
          500: '#3a3f50',
        },
        // Anodized edge / chassis border (industrial, separate from fill).
        steel: {
          400: '#6b7288',
          500: '#4b5166',
          600: '#353a4a',
          700: '#2a2e3c',
        },
        // Cursed violet — single sigil accent (the brand)
        cursed: {
          200: '#ddd6fe',
          300: '#c4b5fd',
          400: '#a78bfa',
          500: '#8b5cf6',
          600: '#7c3aed',
          700: '#6d28d9',
          800: '#5b21b6',
          900: '#3b1d6e',
        },
        // Phosphor live / success
        live: {
          400: '#34d399',
          500: '#10b981',
        },
        // Machine / power rail (rare — power, thermal, arming)
        rail: {
          400: '#fbbf24',
          500: '#f59e0b',
        },
      },
      borderRadius: {
        // Brutal default: almost square. Prefer rounded-sm / rounded for UI.
        none: '0',
        sm: '2px',
        DEFAULT: '3px',
        md: '4px',
        lg: '4px',
        xl: '4px',
        '2xl': '4px',
        full: '9999px',
      },
      fontFamily: {
        mono: ['"JetBrains Mono"', 'ui-monospace', 'SFMono-Regular', 'monospace'],
        sans: ['"Inter"', 'system-ui', 'sans-serif'],
      },
      fontSize: {
        // Instrument cluster labels
        '2xs': ['0.625rem', { lineHeight: '0.875rem', letterSpacing: '0.08em' }],
      },
      boxShadow: {
        // Hard industrial plates — no soft SaaS blobs
        plate: 'inset 0 1px 0 0 rgb(255 255 255 / 0.04)',
        'plate-cursed':
          'inset 0 1px 0 0 rgb(196 181 253 / 0.12), 0 0 0 1px rgb(139 92 246 / 0.15)',
        bezel:
          'inset 0 0 0 1px rgb(139 92 246 / 0.12), inset 0 0 80px -40px rgb(139 92 246 / 0.25)',
        'glow-cursed': '0 0 18px -6px rgb(139 92 246 / 0.45)',
        'glow-live': '0 0 16px -6px rgb(52 211 153 / 0.4)',
        'depth-void':
          '0 0 0 1px rgb(42 46 60 / 0.8), 0 24px 48px -24px rgb(0 0 0 / 0.75)',
      },
      backgroundImage: {
        // Faint instrument lattice under pages (use with bg-void-lattice)
        'void-grid':
          'linear-gradient(rgb(139 92 246 / 0.04) 1px, transparent 1px), linear-gradient(90deg, rgb(139 92 246 / 0.04) 1px, transparent 1px)',
        'void-radial':
          'radial-gradient(ellipse 80% 50% at 50% -10%, rgb(124 58 237 / 0.12), transparent 55%)',
        'sigil-fade':
          'linear-gradient(90deg, rgb(139 92 246 / 0.7), rgb(139 92 246 / 0.15), transparent)',
      },
      backgroundSize: {
        lattice: '28px 28px',
      },
      keyframes: {
        'orb-breathe': {
          '0%,100%': { transform: 'scale(1)', opacity: '0.3' },
          '50%': { transform: 'scale(1.3)', opacity: '0.55' },
        },
        'orb-flicker': {
          '0%,92%,100%': { opacity: '0.7' },
          '94%': { opacity: '0.3' },
          '96%': { opacity: '0.8' },
        },
        ember: {
          '0%,100%': { opacity: '1' },
          '50%': { opacity: '0.65' },
        },
        'cursor-blink': {
          '0%,49%': { opacity: '1' },
          '50%,100%': { opacity: '0' },
        },
        phosphor: {
          '0%,100%': { opacity: '1' },
          '50%': { opacity: '0.55' },
        },
        'sigil-sweep': {
          '0%': { transform: 'translateX(-30%)', opacity: '0.4' },
          '50%': { opacity: '1' },
          '100%': { transform: 'translateX(130%)', opacity: '0.4' },
        },
        'telemetry-tick': {
          '0%,100%': { opacity: '0.35' },
          '50%': { opacity: '0.9' },
        },
      },
      animation: {
        'orb-breathe': 'orb-breathe 4s ease-in-out infinite',
        'orb-flicker': 'orb-flicker 4s steps(1) infinite',
        ember: 'ember 2.4s ease-in-out infinite',
        'cursor-blink': 'cursor-blink 1.2s steps(1) infinite',
        phosphor: 'phosphor 2s ease-in-out infinite',
        'sigil-sweep': 'sigil-sweep 4.5s ease-in-out infinite',
        'telemetry-tick': 'telemetry-tick 2.8s ease-in-out infinite',
      },
      letterSpacing: {
        instrument: '0.14em',
        rite: '0.22em',
      },
    },
  },
  plugins: [],
};
