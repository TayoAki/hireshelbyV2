import { cn } from "@/shared/lib/cn";

export type FuzzyLogoProps = {
  /** Retained for call-site compatibility; the HireShelby mark carries no texture filter. */
  fuzz?: boolean;
  className?: string;
  ariaLabel?: string;
  loop?: boolean;
  /** Retained for call-site compatibility. */
  loopRestSeconds?: number;
  /** Set false when a parent drives its own opacity animation over the mark. */
  pulse?: boolean;
  /** Retained for call-site compatibility. */
  reverse?: boolean;
  /** Retained for call-site compatibility. */
  variant?: string;
};

/**
 * The HireShelby mark.
 *
 * This used to render `BuzzLogoAnimation`, an SVG morph that draws out the word
 * "Buzz" — the upstream wordmark, and Block's trademark. It shipped on the
 * onboarding splash, the app boot screen, and the agent turn indicator, so it
 * was the first thing a new user saw.
 *
 * The animation machinery is deliberately not reused: its path data *is* the
 * lettering, so there was nothing in it to retheme. This draws the HS monogram
 * instead, in `currentColor`, so every existing call site's color and sizing
 * classes keep working untouched.
 */
export function FuzzyLogo({
  fuzz = true,
  className,
  ariaLabel = "HireShelby logo",
  loop = false,
  pulse = true,
}: FuzzyLogoProps) {
  return (
    <svg
      aria-label={ariaLabel}
      className={cn((pulse || loop) && !fuzz && "buzz-logo--pulse", className)}
      fill="none"
      role="img"
      viewBox="0 0 64 64"
      xmlns="http://www.w3.org/2000/svg"
    >
      <rect fill="currentColor" height="64" rx="14" width="64" />
      <text
        dominantBaseline="central"
        fill="var(--buzz-onboarding-shell-bottom, #0d1117)"
        fontFamily="ui-sans-serif, -apple-system, 'Segoe UI', sans-serif"
        fontSize="28"
        fontWeight="700"
        letterSpacing="-0.5"
        textAnchor="middle"
        x="32"
        y="34"
      >
        HS
      </text>
    </svg>
  );
}
