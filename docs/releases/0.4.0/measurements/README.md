# Local Windows GUI evidence

The final instrumented capture passed 788 tests with 7 ignored. Source manifests match before/after. The verification proof records raw-file SHA-256 values; raw profiles, LCOV, logs and the preserved executable remain in the workspace evidence directory.

`final-scoped-summary.json` contains exact source counts. `final-llvm-summary.json` preserves raw LLVM summary measurements with machine-specific source prefixes made relative; no counts changed. `final-uncovered-review.json` gives LCOV zero-DA locations, which can differ from LLVM summary counts because of generic instantiations.

`native-lifecycle.json` records the isolated owner lifecycle and the pixel-capture blocker. Only the private candidate directory was replaced. The source manifest uses repository-relative paths. Native credentials, profile databases and executables are not included here.

The first capture and its schema-reader failure were retained separately, then reprocessed; the final capture succeeded directly with LLVM schema 3 support. The Windows CI numeric baseline is not established.
