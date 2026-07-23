# Security policy

Please report vulnerabilities privately to the future repository owner before opening a public issue.

Command actions are disabled by default. The daemon never invokes a shell for `process.run`; it launches the configured executable directly with an argument array.
