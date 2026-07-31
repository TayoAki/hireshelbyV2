import * as React from "react";

type Particle = {
  top: string;
  left: string;
  size: number;
  color: string;
};

// Brand blue and a cooler slate, both drawn from the onboarding palette.
const BLUE = "#63b3ed";
const SLATE = "#8fa6bf";

// Fixed scatter so the field doesn't shimmer between renders.
const PARTICLES: Particle[] = [
  { top: "4%", left: "27%", size: 7, color: SLATE },
  { top: "7%", left: "58%", size: 5, color: BLUE },
  { top: "5%", left: "88%", size: 6, color: SLATE },
  { top: "13%", left: "12%", size: 8, color: BLUE },
  { top: "12%", left: "73%", size: 5, color: SLATE },
  { top: "18%", left: "44%", size: 4, color: BLUE },
  { top: "22%", left: "90%", size: 7, color: SLATE },
  { top: "28%", left: "5%", size: 6, color: BLUE },
  { top: "31%", left: "21%", size: 4, color: BLUE },
  { top: "35%", left: "84%", size: 6, color: SLATE },
  { top: "45%", left: "13%", size: 7, color: BLUE },
  { top: "47%", left: "93%", size: 5, color: BLUE },
  { top: "55%", left: "30%", size: 5, color: SLATE },
  { top: "57%", left: "70%", size: 7, color: BLUE },
  { top: "63%", left: "8%", size: 7, color: SLATE },
  { top: "66%", left: "88%", size: 6, color: BLUE },
  { top: "72%", left: "48%", size: 5, color: BLUE },
  { top: "76%", left: "18%", size: 6, color: SLATE },
  { top: "80%", left: "64%", size: 6, color: BLUE },
  { top: "86%", left: "34%", size: 7, color: SLATE },
  { top: "88%", left: "80%", size: 6, color: BLUE },
  { top: "92%", left: "10%", size: 5, color: BLUE },
  { top: "3%", left: "42%", size: 4, color: SLATE },
  { top: "9%", left: "5%", size: 4, color: BLUE },
  { top: "16%", left: "62%", size: 6, color: BLUE },
  { top: "20%", left: "30%", size: 4, color: SLATE },
  { top: "26%", left: "52%", size: 5, color: BLUE },
  { top: "33%", left: "68%", size: 4, color: SLATE },
  { top: "40%", left: "40%", size: 4, color: BLUE },
  { top: "42%", left: "78%", size: 5, color: BLUE },
  { top: "52%", left: "55%", size: 4, color: SLATE },
  { top: "60%", left: "42%", size: 5, color: BLUE },
  { top: "68%", left: "26%", size: 4, color: SLATE },
  { top: "70%", left: "76%", size: 6, color: BLUE },
  { top: "82%", left: "6%", size: 5, color: SLATE },
  { top: "84%", left: "50%", size: 4, color: BLUE },
  { top: "94%", left: "60%", size: 5, color: BLUE },
  { top: "95%", left: "90%", size: 4, color: SLATE },
];

const REPEL_RADIUS = 180;
const REPEL_STRENGTH = 110;
// Autonomous wander: each particle drifts on its own smooth loop.
const WANDER_X = 26;
const WANDER_Y = 20;

/**
 * Ambient drift field behind the onboarding landing page.
 *
 * This was `LandingBees`, which scattered ~38 of Block's bee marks across the
 * splash in Buzz yellow. The motion — a per-particle wander plus a pointer
 * repel — is worth keeping, so only the glyphs and palette changed: bees became
 * soft brand-colored dots.
 */
export function LandingParticles() {
  const fieldRef = React.useRef<HTMLDivElement>(null);
  const dotRefs = React.useRef<(HTMLSpanElement | null)[]>([]);
  const pointer = React.useRef<{ x: number; y: number } | null>(null);
  const offsets = React.useRef(PARTICLES.map(() => ({ x: 0, y: 0 })));

  React.useEffect(() => {
    const field = fieldRef.current;
    if (!field) return;

    let raf = 0;
    const start = performance.now();

    const tick = (now: number) => {
      const t = (now - start) / 1000;
      const rect = field.getBoundingClientRect();
      const p = pointer.current;
      dotRefs.current.forEach((el, i) => {
        if (!el) return;
        const dot = PARTICLES[i];
        // Per-particle wander: two incommensurate sine waves, phase-shifted by index.
        const phase = i * 1.7;
        const wx =
          Math.sin(t * (0.7 + (i % 5) * 0.13) + phase) * WANDER_X +
          Math.sin(t * 1.9 + phase * 2.1) * 6;
        const wy =
          Math.cos(t * (0.6 + (i % 7) * 0.11) + phase) * WANDER_Y +
          Math.cos(t * 2.3 + phase * 1.3) * 5;
        let rx = 0;
        let ry = 0;
        if (p) {
          const cx = rect.left + (rect.width * parseFloat(dot.left)) / 100;
          const cy = rect.top + (rect.height * parseFloat(dot.top)) / 100;
          const ox = cx - p.x;
          const oy = cy - p.y;
          const dist = Math.hypot(ox, oy);
          if (dist < REPEL_RADIUS && dist > 0.01) {
            const push =
              ((REPEL_RADIUS - dist) / REPEL_RADIUS) * REPEL_STRENGTH;
            rx = (ox / dist) * push;
            ry = (oy / dist) * push;
          }
        }
        // Ease toward the combined target so repulsion enters/exits smoothly.
        const target = { x: wx + rx, y: wy + ry };
        const cur = offsets.current[i];
        cur.x += (target.x - cur.x) * 0.12;
        cur.y += (target.y - cur.y) * 0.12;
        el.style.transform = `translate(${cur.x}px, ${cur.y}px)`;
      });
      raf = requestAnimationFrame(tick);
    };

    const onMove = (event: MouseEvent) => {
      pointer.current = { x: event.clientX, y: event.clientY };
    };
    const onLeave = () => {
      pointer.current = null;
    };

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
    if (!reduced.matches) {
      raf = requestAnimationFrame(tick);
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseout", onLeave);
    }
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseout", onLeave);
      if (raf) cancelAnimationFrame(raf);
    };
  }, []);

  return (
    <div
      ref={fieldRef}
      aria-hidden
      className="pointer-events-none absolute inset-0 overflow-hidden"
      data-testid="onboarding-landing-particles"
    >
      {PARTICLES.map((dot, i) => (
        <span
          key={`${dot.top}-${dot.left}`}
          ref={(el) => {
            dotRefs.current[i] = el;
          }}
          className="absolute block rounded-full will-change-transform"
          style={{
            top: dot.top,
            left: dot.left,
            width: dot.size,
            height: dot.size,
            background: dot.color,
            opacity: 0.35,
          }}
        />
      ))}
    </div>
  );
}
