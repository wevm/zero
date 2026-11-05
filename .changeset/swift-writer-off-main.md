---
'zero-swift': patch
---

The client's WebSocket frames, such as mutations and query changes, no longer wait for a busy main thread. The writer that sends them one at a time ran on the main actor and went back to it after every frame, so while the app kept the main thread busy each frame waited its turn: on CI, 107 frames took 30 seconds to go out. The writer now runs off the main actor and still sends the frames, and the close after them, in order.
