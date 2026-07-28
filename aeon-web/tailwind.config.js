/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{svelte,html,ts}'],
  theme: {
    extend: {
      colors: {
        // Void chassis — cold near-black, slightly steel-tinted (not purple mud).
        // 500–950 are SURFACES: ink-950 page void; ink-900 panels; ink-800 wells;
        // ink-700 hairlines.
        // 100–400 are the TEXT end of the same ramp (lower = brighter), mirroring
        // zinc so `text-ink-*` and `text-zinc-*` are interchangeable. These steps
        // were referenced by ~265 `text-ink-{100..400}` call sites long before they
        // existed here, so those classes compiled to nothing and the text silently
        // fell back to the inherited body colour — flattening the hierarchy.
        // Don't use 100–400 as backgrounds; surfaces stop at 500.
        ink: {
          100: '#f4f4f5',
          200: '#e4e4e7',
          300: '#d4d4d8',
          400: '#a1a1aa',
          500: '#3a3f50',
          600: '#262a36',
          700: '#1a1c26',
          800: '#11131a',
          900: '#0a0b10',
          950: '#06060a',
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
        // Phosphor live / success (300 = the lighter step badges/pills already use)
        live: {
          300: '#6ee7b7',
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
        // ── Instrument type scale ────────────────────────────────────────────
        // A modular scale (perfect fourth, 1.333) anchored at 0.8125rem/13px so
        // sizes are chosen from a system instead of guessed per element. The
        // classical half of the language is proportion, not ornament.
        //   inscription  →  monumental capitals: page + section titles
        //   readout      →  large telemetry numerals
        '2xs': ['0.625rem', { lineHeight: '0.875rem', letterSpacing: '0.08em' }],
        readout: ['1.5rem', { lineHeight: '1.75rem', letterSpacing: '-0.01em' }],
        'inscription-sm': ['0.8125rem', { lineHeight: '1.25rem', letterSpacing: '0.22em' }],
        inscription: ['1.0625rem', { lineHeight: '1.5rem', letterSpacing: '0.24em' }],
        'inscription-lg': ['1.4375rem', { lineHeight: '2rem', letterSpacing: '0.26em' }],
      },
      spacing: {
        // Rack units — vertical rhythm in multiples of a 4px chassis unit, so
        // section spacing is a cadence rather than an assortment of gaps.
        ru: '0.25rem',
        '2ru': '0.5rem',
        '3ru': '0.75rem',
        '4ru': '1rem',
        '6ru': '1.5rem',
        '8ru': '2rem',
        '12ru': '3rem',
        '16ru': '4rem',
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
        // ── Aether ───────────────────────────────────────────────────────────
        // Light as material: a pale luminous edge along the TOP of a surface, as
        // if lit from an oculus above, plus depth beneath. Inset only — never an
        // outer neon halo (that reads as cyberpunk wallpaper, not an instrument).
        stele:
          'inset 0 1px 0 0 rgb(221 214 254 / 0.09), inset 0 -1px 0 0 rgb(0 0 0 / 0.4), 0 1px 0 0 rgb(0 0 0 / 0.5)',
        'stele-lit':
          'inset 0 1px 0 0 rgb(221 214 254 / 0.16), inset 0 24px 40px -32px rgb(167 139 250 / 0.22), inset 0 -1px 0 0 rgb(0 0 0 / 0.4)',
        // A struck hairline — the gilt edge on an inscription.
        gilt: 'inset 0 0 0 1px rgb(196 181 253 / 0.14)',
      },
      backgroundImage: {
        // Faint instrument lattice under pages (use with bg-void-lattice)
        'void-grid':
          'linear-gradient(rgb(139 92 246 / 0.04) 1px, transparent 1px), linear-gradient(90deg, rgb(139 92 246 / 0.04) 1px, transparent 1px)',
        'void-radial':
          'radial-gradient(ellipse 80% 50% at 50% -10%, rgb(124 58 237 / 0.12), transparent 55%)',
        'sigil-fade':
          'linear-gradient(90deg, rgb(139 92 246 / 0.7), rgb(139 92 246 / 0.15), transparent)',
        // Aether fall — a vertical light gradient down a surface (oculus light).
        'aether-fall':
          'linear-gradient(180deg, rgb(167 139 250 / 0.07) 0%, rgb(167 139 250 / 0.015) 38%, transparent 72%)',
        // Colonnade — the rhythmic vertical rule field that replaces box-in-box.
        colonnade:
          'linear-gradient(90deg, rgb(139 92 246 / 0.05) 1px, transparent 1px)',
        // Entablature — the struck line under a title band.
        entablature:
          'linear-gradient(90deg, rgb(196 181 253 / 0.30), rgb(139 92 246 / 0.12) 42%, transparent 88%)',
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
