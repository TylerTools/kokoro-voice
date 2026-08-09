# Security policy

Report security vulnerabilities using GitHub's private vulnerability reporting
for this repository. Do not open a public issue containing tokens, diagnostics,
transcripts, clipboard contents, recordings, personal paths, or exploit details.

The supported release line is the latest signed release for macOS Apple silicon
or Windows x64. Unsigned development builds, Linux, and Intel macOS are not
security-supported release targets.

Kokoro Voice binds its engine to loopback, requires a per-user bearer token,
rejects oversized requests, and excludes content from structured diagnostics.
Any report showing remote service exposure, authentication bypass, secure-field
capture, out-of-range text modification, signature/update bypass, or diagnostic
content leakage is treated as high priority.
