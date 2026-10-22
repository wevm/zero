---
'zero-swift': patch
---

JSON nested as deep as the parser accepts (5,000 levels) no longer overflows the stack when it is parsed, written, compared, hashed, or hashed as a custom query's arguments, even on a thread with a 512 KB stack, such as a test or cooperative thread on macOS. Before, a debug build overflowed at about 300 levels on such a thread. Freeing such a value still recurses: in a release build, a 5,000-deep value fits in a 1 MB stack, such as the iPhone main thread's, but not in 512 KB.
