---
'zero-rust': minor
---

`ZeroOptions::hidden_disconnect_delay_ms` now defaults to 5 minutes, as JS's `hiddenTabDisconnectDelay` does, instead of 0: after `Zero::set_visible(false)`, the client stays connected for 5 minutes before it disconnects. An app that wants the old behavior, such as a mobile app the system suspends in the background, sets `hidden_disconnect_delay_ms: 0.0`.
