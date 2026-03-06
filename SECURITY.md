# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 1.5.x   | ✅ Current release |
| < 1.5   | ❌ No longer supported |

## Reporting a Vulnerability

If you discover a security vulnerability in LeIndex, **please do not open a public issue**.

Instead, report it privately:

1. **Email:** Send details to the maintainers via [GitHub Security Advisories](https://github.com/scooter-lacroix/LeIndex/security/advisories/new)
2. **Include:**
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

We will acknowledge receipt within **48 hours** and aim to provide a fix or mitigation within **7 days** for critical issues.

## Security Design

LeIndex is designed with the following security principles:

### Local-First Architecture

- LeIndex runs entirely on your machine. No code or index data is sent to external servers.
- All indexing and search operations are performed locally.

### Database Discovery (Opt-In Only)

- The `leserve` HTTP server can auto-discover project databases, but this is **disabled by default**.
- You must explicitly set `LEINDEX_DISCOVERY_ROOTS` to enable it.
- Sensitive directories are automatically excluded from scanning:
  - Cryptographic materials: `/.ssh/`, `/.gnupg/`, `/.config/gnupg/`
  - Cloud credentials: `/.aws/`, `/.azure/`, `/.gcloud/`, `/.kube/`, `/.docker/`
  - Secret management: `/.op/`, `/.vault/`, `/.1password/`
  - Build artifacts: `/node_modules/`, `/target/`, `/.git/`

### Database Ingestion

- Paths must be absolute and validated for injection patterns.
- Files must be regular files (symlinks are rejected).
- Only `.db`, `.sqlite`, or `.sqlite3` extensions are accepted.
- All SQL operations use parameterized queries.

### MCP Server

- The MCP server binds to `127.0.0.1` by default (localhost only).
- Edit operations (`leindex_edit_apply`, `leindex_rename_symbol`) include dry-run/preview modes to prevent accidental changes.
- Token budgets are bounded and validated to prevent memory exhaustion.

## Disclosure Policy

We follow coordinated disclosure. Once a fix is released, we will:

1. Credit the reporter (unless anonymity is requested)
2. Publish a security advisory on GitHub
3. Include the fix in the next release with a changelog entry
