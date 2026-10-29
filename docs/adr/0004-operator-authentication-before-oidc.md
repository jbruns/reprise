# 4. Operator authentication before OIDC

Date: 2026-10-28

## Status

Accepted

## Context

The Service is Operator-only and runs as a Protected Service. The Host's pattern prefers native OIDC, then forward auth
that hands the Service an identity header, then forward auth as a gate in front of the Service's own login. The
documented headless Audit must call the Service's API, and a headless client cannot complete a browser sign-in at the
IdP. Native OIDC in Title Select would delay a usable Service.

## Decision

- The web UI uses forward auth with identity handoff. Title Select trusts the identity header only on connections from
  the reverse proxy's network, and the deployment strips that header on every route that skips the IdP.
- The API also accepts one static Operator token, read from the file named by `TITLE_SELECT_OPERATOR_TOKEN_FILE`,
  through a Client Path. Title Select keeps only the token's hash and compares in constant time. Rotating it means
  replacing the secret and redeploying.
- There are no per-user accounts, tokens or scopes.

## Consequences

- This is an interim decision. Native OIDC, with a non-browser flow for the headless client, replaces or narrows the
  static token after the Service is usable, and a later ADR supersedes this one.
- Anyone holding the Operator token has full Operator access to the API, including approving Change Plans once they
  exist.
