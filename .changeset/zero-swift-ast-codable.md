---
'zero-swift': patch
---

`AST` is now `Codable`, and `AST.json` is public, so an app can check its Swift queries against the ASTs its server builds for them, as with custom queries whose rows the server's AST decides. Decode a server's AST JSON with `JSONDecoder` (or `JSONValueDecoder`) and compare it with `query.ast`; JSON that isn't an AST throws `DecodingError.dataCorrupted`, saying where. `json` writes the protocol's JSON, and for a normalized AST the same text as JS's `JSON.stringify(normalizeAST(ast))`: a relationship's `hidden` now comes before its `subquery`, as in JS. Query hashes don't change.
