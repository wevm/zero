---
'zero-swift': patch
---

The client now speaks sync protocol 57, the version the JS client uses. Versions 54–57 only add query features the Swift client doesn't send yet (aggregates, `groupBy`, `distinct`, JSON path filters), so nothing else changes, and zero-cache keeps accepting older versions.
