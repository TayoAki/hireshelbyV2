# hireshelby.com

Single-file static marketing site: hero, features, agents, per-seat pricing,
and download links. No build step — deploy `index.html` to any static host
(Railway static, Cloudflare Pages, S3).

Checkout buttons currently route signed-out visitors to the download section,
because a hosted Stripe Checkout session requires a signed-in owner of a
community (`POST /v1/billing/checkout` on the control plane). Once the site
gains a session bridge, swap the handler in the inline `<script>` for the
fetch call sketched there — the endpoint already returns `{url}` to Stripe's
hosted page.

Inject `window.HIRESHELBY_ACCOUNTS_URL` before the script tag to point at a
non-production control plane.
