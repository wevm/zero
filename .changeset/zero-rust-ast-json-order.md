---
'zero-rust': patch
---

`Ast::to_json` writes an AST's fields in the order JS's `normalizeAST` writes them, so an app can compare its queries' ASTs with a server's AST JSON as text: `json::stringify` of a normalized AST (`Ast::normalized`) is the text JS writes, byte for byte. The JSON is unchanged except for the key order of a relationship's fields: `hidden` now comes before `subquery`, as in JS. Query hashes are unchanged.
