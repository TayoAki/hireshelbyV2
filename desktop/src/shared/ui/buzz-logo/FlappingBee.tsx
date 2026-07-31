import { BuzzMark } from "./BuzzMark";

/**
 * The HireShelby mark with an idle "breathing" pulse, used on the loading gates
 * so a slow boot reads as alive rather than hung.
 *
 * This used to be the upstream bee with flapping wings — the wings were two
 * circles of the bee silhouette, so the animation had nothing left to drive
 * once the mark was rebranded.
 *
 * The animation stays on an HTML-level wrapper rather than inside the SVG, and
 * that placement is load-bearing: WebKit paints SVG *children* on the main
 * thread, so animating them freezes for as long as boot work (bundle eval,
 * first React render) hogs that thread — exactly the window in which the
 * loading gate is on screen. Transforms on HTML elements run on the compositor
 * (Core Animation in WKWebView) and keep moving regardless.
 *
 * Everything is plain SVG + CSS (no JS/SMIL), so it paints on the very first
 * frame. Reduced motion falls back to the static mark via the CSS media query.
 */
export function FlappingBee({ className }: { className?: string }) {
  return (
    <div
      aria-hidden="true"
      className={["hs-mark-sprite", "relative", "aspect-square", className]
        .filter(Boolean)
        .join(" ")}
    >
      <BuzzMark className="hs-mark-breathe block h-full w-full" />
    </div>
  );
}
