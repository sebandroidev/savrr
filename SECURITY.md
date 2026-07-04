# Security policy

Savrr holds people's save files and runs a server that, while it lives on your own hardware, is reachable over a network. I take reports about it seriously.

## Reporting a vulnerability

Email **nssoftdev@gmail.com** with the details. Please don't open a public issue for a security problem.

Include enough to reproduce it: what you did, what happened, and what you think the impact is. If you have a proof of concept, even better. I'll confirm I got your report, work on a fix, and credit you when it ships unless you'd rather stay anonymous.

There's no paid bounty. This is a personal open-source project.

## What Savrr defends against

The full threat model is in [docs/prd/PRD-06](docs/prd/PRD-06-Security-Auth.md). The short version:

- Traffic is meant to run over TLS or an encrypted overlay like Tailscale or WireGuard. Plain HTTP is rejected except for a local health check.
- Each device gets its own credential and can be revoked on its own. Access tokens are short-lived; the long-lived refresh secret is stored in your OS keychain, and only its hash is kept on the server.
- Every request is scoped to one account. A device can only touch its own account's games, versions, and blobs, and it can only download a blob its own history references.
- Server history is append-only. A restore always snapshots the current state first, so it's undoable, and old versions can't be silently erased.
- Pairing codes are short-lived, single-use, rate-limited, and locked out after repeated failures.

## What's out of scope for now

A compromised owner account, and isolation between multiple tenants (Savrr is single-owner by design). Optional client-side encryption of saves before upload is planned but not in the first release, so treat the server as able to read your saves until then.

## Supported versions

Savrr is pre-1.0. Fixes land on the latest release; there are no backports yet.
