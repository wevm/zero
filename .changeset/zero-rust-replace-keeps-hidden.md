---
'zero-rust': patch
---

A client that replaces itself after the server lost its state (`ClientNotFound`) now keeps a `set_visible(false)` the app made less than `hidden_disconnect_delay_ms` before. It disconnects when the hidden client would have; before, it stayed connected until the app called `set_visible` again.
