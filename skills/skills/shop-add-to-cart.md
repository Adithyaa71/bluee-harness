# Shop add to cart

> Find a product on Amazon or Flipkart in the user's own browser and add it to the cart - never buy it.

- category: skills
- tools: snarevec__browser_status, snarevec__browser_new_tab, snarevec__browser_navigate, snarevec__browser_query, snarevec__browser_type, snarevec__browser_click, snarevec__browser_get_page_info, snarevec__browser_wait_for, snarevec__browser_scroll, uacc__get_screen_info, uacc__smart_click
- triggers: amazon, flipkart, shopping, shop for, add to cart, add it to my cart, my cart, buy me, order me
- created: 2026-10-07T05:30:00+00:00
- updated: 2026-10-07T05:30:00+00:00

---

**The hard line: stop at the cart.** Never press Buy Now, Proceed to Checkout, Place
Order or Pay, never choose an address or payment method, never enter an OTP. Adding to
the cart is the whole job. If the user asks you to complete a purchase, tell them the
cart is ready and that paying is theirs to do.

## Steps (SnareVec, in the user's real Chrome - their own login and cart)
1. `snarevec__browser_status`. No extension polling → ask the user to open Chrome with the
   SnareVec workbench open, then retry. Do not switch to UACC for this silently.
2. Open the search directly - fastest and most reliable:
   - Amazon: `https://www.amazon.in/s?k=<query, spaces as +>`
   - Flipkart: `https://www.flipkart.com/search?q=<query, spaces as +>`
   with `snarevec__browser_new_tab` (or `browser_navigate`).
3. Read the results with `snarevec__browser_query`. Starting selectors (verify - sites change):
   - Amazon result titles: `div[data-component-type="s-search-result"] h2`
   - Amazon result links: `div[data-component-type="s-search-result"] a.a-link-normal[href*="/dp/"]`
   - Flipkart product links: `a[href*="/p/"]`
   Skip "Sponsored" results unless the user asked for them. Pick the result that best
   matches what the user said (brand, size, price range); if two are equally good, list
   them briefly and ask.
4. Open the product: `browser_navigate` to its link (more reliable than clicking a tile
   that opens a new tab). Confirm with `browser_get_page_info`.
5. Read the price and title with `browser_query` (Amazon: `#productTitle`,
   `.a-price .a-offscreen`; Flipkart: `h1`, and look for the price near it).
6. Add to cart:
   - Amazon: `snarevec__browser_click` on `#add-to-cart-button`.
   - Flipkart: `browser_query` on `button` and click the one whose text is "Add to cart"
     (use `nth`).
   If a size/colour must be chosen first, choose what the user asked for, or ask.
   If a cover/warranty pop-up appears, decline it (look for "No thanks" / "Skip").
7. Verify it is really in the cart: open the cart (Amazon `https://www.amazon.in/gp/cart/view.html`,
   Flipkart `https://www.flipkart.com/viewcart`) and `browser_query` for the product title.
8. Report: product, price, quantity, and that it is in the cart and NOT ordered.

## When the DOM route fails
- A click returns `needs_confirmation`: relay it to the user in your own words; only pass
  `confirm` if they agree - and never for a checkout or payment step.
- Selector not found twice: take one `snarevec__browser_screenshot` to see why (pop-up,
  login wall, captcha). Login wall or captcha → hand to the user.
- Only if the page truly cannot be driven by DOM, use UACC on the visible browser:
  `uacc__get_screen_info` to read it, `uacc__smart_click` on the button by its label.
