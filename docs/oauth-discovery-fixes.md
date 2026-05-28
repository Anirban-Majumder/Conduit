# OAuth Discovery Improvements — Implementation Plan

## Problem

Conduit's OAuth discovery only follows one path (Protected Resource Metadata → Authorization Server Metadata), while the working chrome-ai reference follows multiple paths in parallel. This causes Conduit to fail with MCP servers that work fine in chrome-ai.

## Changes Overview

| File | Change |
|------|--------|
| `src/daemon/upstream/oauth.rs` | Add direct auth server discovery, OIDC fallback, fallback endpoint generation |
| `src/daemon/upstream/probe.rs` | Rework `run_oauth_pre` to try multiple discovery strategies |

## Bug 1: Direct Authorization Server Discovery

**What:** chrome-ai tries `/.well-known/oauth-authorization-server` and `/.well-known/openid-configuration` directly on the MCP URL's origin, in parallel with Protected Resource Metadata discovery. Conduit only follows the PRM → AS chain.

**Why it matters:** Servers like Figma expose auth server metadata directly on their origin without a protected-resource metadata document. These servers work in chrome-ai but fail in Conduit with "no OAuth protected-resource metadata found".

**Where:** `src/daemon/upstream/oauth.rs` — `fetch_authorization_server_metadata` function (line 273).

**How:**
1. Add a new function `discover_authorization_server_direct(http, mcp_url)` that tries well-known endpoints on the MCP URL's origin directly (without going through PRM first).
2. Try these endpoints in order:
   - `{origin}/.well-known/oauth-authorization-server`
   - `{origin}/.well-known/openid-configuration`
3. For URLs with paths (e.g., `https://example.com/v1/mcp`), also try:
   - `{origin}/.well-known/oauth-authorization-server{path}`
   - `{origin}/.well-known/openid-configuration{path}`
   - `{path_url}/.well-known/openid-configuration` (path-appending variant)

## Bug 2: OpenID Connect Discovery Fallback

**What:** Conduit only tries `/.well-known/oauth-authorization-server`. chrome-ai also tries `/.well-known/openid-configuration` (OpenID Connect Discovery 1.0).

**Why it matters:** Some OAuth providers (particularly those built on OpenID Connect stacks) only publish metadata at the OIDC well-known path, not the RFC 8414 OAuth path.

**Where:** `src/daemon/upstream/oauth.rs` — `fetch_authorization_server_metadata` function (line 273).

**How:** Extend `fetch_authorization_server_metadata` to accept multiple candidate URLs and try each in sequence. The caller already iterates candidates — just add OIDC paths to the candidate list.

## Bug 3: Fallback Endpoint Generation

**What:** When all metadata discovery fails, chrome-ai generates fallback endpoints by stripping common MCP path suffixes (`/sse`, `/mcp`) and appending `/authorize`, `/token`, `/register`. These are validated with HEAD requests before use. Conduit returns a hard failure.

**Why it matters:** Some MCP servers have OAuth endpoints at the standard paths but don't publish metadata documents. The fallback gives these servers a chance to work.

**Where:** `src/daemon/upstream/oauth.rs` (new function) + `src/daemon/upstream/probe.rs` — `run_oauth_pre` (line 430).

**How:**
1. Add `generate_fallback_endpoints(mcp_url) -> AuthorizationServerMetadata` that:
   - Parses the MCP URL
   - Strips `/sse`, `/mcp`, `/v1/mcp`, `/api/mcp/sse` etc. from the path
   - Constructs `{base}/authorize`, `{base}/token`, `{base}/register`
2. Add `validate_endpoint(url) -> bool` that does a HEAD request and returns true if the status is not 404/500.
3. In `run_oauth_pre`, if `first_authorization_metadata` fails, try the fallback endpoints. Validate at least one endpoint responds before proceeding.

## Implementation Order

### Step 1: Refactor `fetch_authorization_server_metadata` to accept multiple candidates

Currently it takes a single `resource_metadata_url`. Change to accept `&[String]` candidates and try each in order, returning the first success. This is the foundation for bugs 1 and 2.

### Step 2: Add direct discovery candidates in `run_oauth_pre`

Before calling `first_authorization_metadata`, build a candidate list that includes:
- PRM-discovered auth server URLs (existing path)
- Direct well-known on MCP origin (bug 1)
- OIDC well-known paths (bug 2)

### Step 3: Add fallback endpoint generation

Add `generate_fallback_endpoints` and `validate_endpoint` functions. In `run_oauth_pre`, if all metadata candidates fail, try the fallback. Only proceed with DCR if the registration endpoint validates.

### Step 4: Run `cargo check` and `cargo clippy`

Fix any compilation or lint issues.

## Testing

- Existing tests in `oauth.rs` cover PKCE, DCR shape, authorize URL, etc. — no changes needed there.
- The new discovery functions are primarily I/O (HTTP requests) so they're best tested against live servers.
- `cargo nextest run` to verify no regressions.
