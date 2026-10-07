---
name: project-kickoff
description: The questions to answer before a new project gets any code.
servers: []
---

I am starting something new. Before any code, walk me through this, one section
at a time - ask, wait for my answer, then move on. Do not dump the whole list.

1. **What is actually being built, in two sentences?** If it takes more, the
   scope is not decided yet.
2. **What does done look like?** Something checkable, not "it works well".
3. **What is the riskiest assumption?** The one that, if wrong, makes the rest
   pointless. Push me here - the first answer is usually not it.
4. **What am I deliberately not building?** Name the tempting adjacent thing.
5. **What already exists?** Search memory for anything related I have built or
   decided before. Check before I rebuild something I already have.
6. **What is the smallest version that proves the risky assumption?**

Then write it up as an artifact I can come back to, and tell me plainly if you
think the scope is too big for the time I said.

Note: this loop has no schedule. Run it with `harness loop project-kickoff`, or
just ask for it in chat.
