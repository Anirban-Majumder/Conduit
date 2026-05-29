---
name: Conduit Unified
colors:
  surface: '#131313'
  surface-dim: '#131313'
  surface-bright: '#3a3939'
  surface-container-lowest: '#0e0e0e'
  surface-container-low: '#1c1b1b'
  surface-container: '#201f1f'
  surface-container-high: '#2a2a2a'
  surface-container-highest: '#353534'
  on-surface: '#e5e2e1'
  on-surface-variant: '#bccac2'
  inverse-surface: '#e5e2e1'
  inverse-on-surface: '#313030'
  outline: '#86948d'
  outline-variant: '#3d4a44'
  surface-tint: '#5adcb3'
  primary: '#ffffff'
  on-primary: '#003829'
  primary-container: '#79f9ce'
  on-primary-container: '#007258'
  inverse-primary: '#006c52'
  secondary: '#c8c6c5'
  on-secondary: '#313030'
  secondary-container: '#4a4949'
  on-secondary-container: '#bab8b7'
  tertiary: '#ffffff'
  on-tertiary: '#303030'
  tertiary-container: '#e4e2e1'
  on-tertiary-container: '#656464'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#79f9ce'
  primary-fixed-dim: '#5adcb3'
  on-primary-fixed: '#002117'
  on-primary-fixed-variant: '#00513d'
  secondary-fixed: '#e5e2e1'
  secondary-fixed-dim: '#c8c6c5'
  on-secondary-fixed: '#1c1b1b'
  on-secondary-fixed-variant: '#474646'
  tertiary-fixed: '#e4e2e1'
  tertiary-fixed-dim: '#c8c6c5'
  on-tertiary-fixed: '#1b1c1c'
  on-tertiary-fixed-variant: '#474746'
  background: '#131313'
  on-background: '#e5e2e1'
  surface-variant: '#353534'
typography:
  display-lg:
    fontFamily: Hanken Grotesk
    fontSize: 48px
    fontWeight: '700'
    lineHeight: 56px
    letterSpacing: -0.02em
  headline-lg:
    fontFamily: Hanken Grotesk
    fontSize: 32px
    fontWeight: '600'
    lineHeight: 40px
    letterSpacing: -0.01em
  headline-md:
    fontFamily: Hanken Grotesk
    fontSize: 24px
    fontWeight: '600'
    lineHeight: 32px
  body-lg:
    fontFamily: Hanken Grotesk
    fontSize: 18px
    fontWeight: '400'
    lineHeight: 28px
  body-md:
    fontFamily: Hanken Grotesk
    fontSize: 16px
    fontWeight: '400'
    lineHeight: 24px
  label-md:
    fontFamily: Hanken Grotesk
    fontSize: 14px
    fontWeight: '500'
    lineHeight: 20px
    letterSpacing: 0.01em
  label-sm:
    fontFamily: Hanken Grotesk
    fontSize: 12px
    fontWeight: '600'
    lineHeight: 16px
    letterSpacing: 0.05em
rounded:
  sm: 0.25rem
  DEFAULT: 0.5rem
  md: 0.75rem
  lg: 1rem
  xl: 1.5rem
  full: 9999px
spacing:
  unit: 8px
  container-padding: 32px
  gutter: 24px
  max-width: 1440px
  section-gap: 48px
---

## Brand & Style

The design system establishes a high-fidelity, crystalline environment for a modern desktop application. It transitions the focused intensity of a professional workspace into a deep, immersive, and fluid aesthetic inspired by "Dark Glassmorphism."

**Style Direction: Nocturnal Glassmorphism**
The aesthetic is defined by clarity, transparency, and refracted light against a dark canvas. Surfaces feel like smoked glass—translucent and ethereal. The visual narrative centers on "Prismatic Depth," where a dark, concentrated base is punctuated by sharp mint accents and soft, diffusive glows. The emotional response is one of organized calm, modern efficiency, and premium precision.

- **Primary Motif:** Translucent layering over a dark, ink-like canvas.
- **Visual Weight:** Deep, immersive containers with high-density internal content and crisp definition.
- **Atmosphere:** Focused, tech-forward, and sophisticated.

## Colors

The palette is anchored in darkness and transparency to maximize focus and reduce eye strain during professional use.

- **Background:** The foundation is a clean, deep neutral (`#0a0a0a`) to provide a focused workspace.
- **Primary (Mint):** Used for high-intent actions, active states, and focus indicators. In dark mode, it acts as a luminous beacon that cuts through the dark interface.
- **Surfaces:** Smoked glass panels with high transparency allow background blurs to create depth without losing the sense of a unified dark space.
- **Borders:** Subtle, low-contrast strokes (`#404944`) define the silhouette of glass layers.
- **Typography:** Bright mint (`#7fffd4`) or off-white provides peak legibility for content, while muted grays handle secondary metadata and labels.

## Typography

This design system utilizes **Hanken Grotesk** across all levels to maintain a sharp, contemporary, and engineered feel. 

- **Headlines:** Use tighter letter-spacing and heavier weights to create a commanding presence against the dark background.
- **Body Text:** Standardized at 16px for desktop comfort, emphasizing line height (1.5x) to ensure readability within high-density layouts.
- **Labels:** Small labels use a slight tracking increase and medium-to-semibold weights to ensure they remain legible even when de-emphasized.
- **Contrast:** Typography transitions to light neutrals or vibrant mint to ensure high contrast against dark surfaces.

## Layout & Spacing

The layout philosophy follows a **Fixed Center Grid** model, optimized for wide-screen desktop displays.

- **Structure:** A 12-column grid with a 1440px max-width, centered on the viewport.
- **Top Navigation:** No sidebar. A centered, glassmorphic Top Navigation bar floats at the top of the screen, acting as the primary anchor.
- **White Space:** The UI balances high-density data with generous internal padding (`32px` for main cards) and significant gaps between major sections (`48px`) to maintain a breathable feel.
- **Alignment:** Elements are strictly aligned to an 8px base grid.
- **Responsive:** On narrower viewports, the 1440px container shrinks to fill 90% of the screen width before reflowing for tablet/mobile breakpoints.

## Elevation & Depth

In dark mode, depth is communicated through **Refractive Layering** and **Luminous Inner Glows**.

1.  **Level 0 (Base):** The deep, dark background.
2.  **Level 1 (Panels):** Translucent dark surfaces with 40px backdrop blur. These panels use a subtle inner highlight on the top edge to simulate the rim of a glass pane.
3.  **Level 2 (Overlays):** Modals and dropdowns use increased backdrop blur and a soft, diffused dark shadow to lift from the panels below.
4.  **Interaction:** Hovering over a glass panel should subtly increase the border opacity or add a faint Mint (`#7fffd4`) glow to indicate focus.

## Shapes

The shape language balances professional structure with friendly, modern curves.

- **Main Containers:** All primary glass panels and cards use a large `24px` radius to soften the edges and enhance the glass aesthetic.
- **Interactive Components:** Inputs and secondary buttons use a standard `12px` radius.
- **Action Elements:** Primary buttons, tags, and status chips use a `9999px` (Pill-shaped) radius to signify high interactivity.
- **Borders:** All strokes are exactly 1px, centered, to maintain the "sleek" desktop feel.

## Components

### Buttons
- **Primary:** Pill-shaped, Solid Mint (`#7fffd4`) with dark charcoal text. Features a crisp, luminous silhouette.
- **Secondary:** Pill-shaped, transparent with a 1px Mint border.
- **Ghost:** Pill-shaped, no background or border, Mint text.

### Top Navigation
- A floating, glassmorphic bar (85% opacity, 40px blur).
- Fixed to the top center with a maximum width of 1200px.
- Active states indicated by a Mint dot below the navigation label or a subtle glow.

### Cards & Panels
- Main layout containers use the `24px` radius and dark glassmorphic styling.
- Internal sections are separated by low-opacity `1px` lines to maintain the sophisticated feel.
- Generous `32px` internal padding is mandatory for all main containers.

### Input Fields
- Dark, semi-transparent background with a `1px` border.
- `12px` rounded corners.
- On focus: Border changes to Mint (`#7fffd4`) with a soft outer glow.

### Status Indicators
- Small circular dots or "glows" using the Mint accent. 
- Active status is always accompanied by a clear, high-contrast indicator to signify active data streams.