# SnareVec crawl and search

> Read websites without opening a browser: crawl or add pages into a collection, embed, then search it.

- category: skills
- tools: snarevec__snarevec_status, snarevec__list_collections, snarevec__create_collection, snarevec__add_urls, snarevec__crawl_site, snarevec__embed_collection, snarevec__search_collection
- triggers: crawl, scrape, research this site, read the docs, index the site, collection
- created: 2026-10-07T05:30:00+00:00
- updated: 2026-10-07T05:30:00+00:00

---

1. `snarevec__snarevec_status`. NOT RUNNING means the daemon idled out (it stops after
   30 idle minutes) - it is not broken. Tell the user to open the SnareVec workbench (or
   run `pro/scripts/run-daemon.bat`), then retry.
2. `snarevec__list_collections`; reuse a fitting one or `snarevec__create_collection`
   with a short name.
3. Bring pages in:
   - a few known pages: `snarevec__add_urls`
   - a site or section: `snarevec__crawl_site` with `include` patterns to stay on topic,
     `max_pages` 10-30 to keep it quick. `render: "browser"` only for JS-heavy pages.
4. Crawling QUEUES pages - always run `snarevec__embed_collection` before searching.
5. `snarevec__search_collection` with a natural-language question; answer from the hits
   and name the pages you used.

Crawling is the unattended path: it does not use the user's login. For a logged-in page,
use the SnareVec browser tools instead (see the web-and-desktop-hybrid skill).
