# dev checks

## `boot-check.mjs`

Runs the dashboard's own JavaScript against a **live** bluee, with a stubbed
DOM, and reports any element it asks for that does not exist plus any runtime
error during boot.

`cargo test` covers *parsing* (`tests/dashboard_js.rs`). This covers *running* -
which is different, and is the gap that let a broken page ship once already:
every API endpoint answered perfectly under curl while the window sat on
"connecting…", because the script had died before it could do anything.

```bash
# 1. start bluee and note the port it prints
cargo run --release -- dash 7777

# 2. extract the inline script and run the check
node dev/boot-check.mjs 7777 <extracted.js> dash/index.html
```

Extract the script with:

```bash
python -c "import re;h=open('dash/index.html',encoding='utf-8').read();\
open('app.js','w',encoding='utf-8').write(re.findall(r'<script(?![^>]*\bsrc=)[^>]*>(.*?)</script>',h,re.S)[-1])"
```
