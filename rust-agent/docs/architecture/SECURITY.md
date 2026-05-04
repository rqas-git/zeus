# Security Notes

## Cargo Audit Exceptions

`cargo audit` reads `.cargo/audit.toml` from the repository root.

The current exception is `RUSTSEC-2025-0141` for `bincode 1.3.3`, which is
reported as unmaintained through this dependency path:

```text
fff-search -> heed -> heed-types -> bincode
```

This is accepted temporarily because the project intentionally keeps FFF for
workspace search behavior. The documented ignore mirrors Codex's current
treatment of the same advisory while a transitive dependency still requires
`bincode 1.x`. Revisit the exception when FFF offers a release that removes the
`bincode 1.x` dependency, when a maintained FFF fork is available, or when
replacing FFF no longer risks search quality regressions.
