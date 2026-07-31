/**
 * The HireShelby mark as a plain static SVG — no SMIL, no scripting, no
 * animation machinery. Rendered in `currentColor` so it tints per-theme, and it
 * paints complete on the very first frame regardless of animation support.
 *
 * This previously drew the upstream bee silhouette — two wing circles and a
 * slotted body masked together, which is Block's logo. The geometry *was* the
 * branding, so it was replaced outright rather than recolored.
 */
export function BuzzMark({ className }: { className?: string }) {
  return (
    <svg
      aria-hidden="true"
      className={["buzz-mark", className].filter(Boolean).join(" ")}
      viewBox="0 0 64 64"
      fill="none"
    >
      <rect width="64" height="64" rx="14" fill="currentColor" />
      <text
        x="32"
        y="34"
        textAnchor="middle"
        dominantBaseline="central"
        fontFamily="ui-sans-serif, -apple-system, 'Segoe UI', sans-serif"
        fontSize="28"
        fontWeight="700"
        letterSpacing="-0.5"
        fill="var(--buzz-onboarding-shell-bottom, #0d1117)"
      >
        HS
      </text>
    </svg>
  );
}
