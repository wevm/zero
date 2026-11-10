---
'zero-swift': patch
---

The code generator moved to `packages/zerogen` in the repository, shared code that the CLI for each native client bundles. `zero-swift codegen` and `zero-swift drift record` take the same config file, flags, and options as before and write the same files, and the package's dependencies are unchanged.
