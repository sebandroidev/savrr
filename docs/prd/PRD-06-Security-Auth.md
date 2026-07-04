# PRD-06 · Security & Auth

Single-owner, self-hosted trust model. The server is on the user's own hardware, but it is reachable over the network, so treat it as internet-exposed.

---

## 1. Account & device model

- **One account** = the owner. (Household sub-accounts are a later extension; schema already carries `account_id` everywhere.)
- **Many devices** per account. Each device holds its own credential; devices are individually revocable.

## 2. Pairing a device

Avoid typing passwords on a Steam Deck. Use a short-lived pairing code:

1. On an already-paired device (or the server admin UI), the owner generates a **pairing code** (6–8 chars, TTL 5 min, single use).
2. New device: `POST /devices/pair { code, device_name, os }`.
3. Server validates the code → issues a **device credential**:
   - a long-lived **refresh secret** (stored hashed server-side, kept in OS keychain client-side),
   - short-lived **access JWTs** minted from it.
4. Code is burned.

First device / bootstrap: owner sets a password at server install; `POST /auth/login` returns a session used to mint the first pairing code.

## 3. Tokens

- **Access JWT:** short TTL (e.g. 15 min), claims `{ sub: device_id, acc: account_id, exp }`, signed HS256 with a server secret (or RS256 if you prefer rotating keys). Sent as `Bearer` on every REST + WS call.
- **Refresh:** device exchanges its refresh secret for a new access JWT via `POST /auth/refresh`. Refresh secrets are hashed at rest (argon2/bcrypt), never returned again after pairing.
- **Revocation:** `DELETE /devices/{id}` sets `revoked = 1`; server rejects that device's refresh + drops its WS.

Store secrets in the OS keychain: Windows Credential Manager, macOS Keychain, Linux Secret Service (libsecret) via the `keyring` crate. Never plaintext on disk.

## 4. Transport security

- **TLS everywhere.** For a home server, the realistic options (document all three):
  - Reverse proxy (Caddy/Traefik) terminating TLS with a real cert via a domain + Let's Encrypt (works even for LAN-only via DNS-01).
  - Tailscale / WireGuard overlay – devices reach the server over an encrypted mesh; simplest and most private, recommended default for non-technical setups.
  - Self-signed cert pinned by clients (fallback; clients pin the cert fingerprint).
- Reject plain HTTP except a localhost health check.

## 5. Encryption at rest (optional, config)

Saves are usually not sensitive, but offer opt-in **client-side encryption**:
- Owner sets a passphrase → derive a key (argon2id) → encrypt each `.savr` archive with an AEAD (XChaCha20-Poly1305) **before** upload.
- Server stores ciphertext blobs; it never sees plaintext or the key. `blob_hash` is computed over ciphertext (dedup still works per-account since the key is constant).
- Trade-off: cross-account dedup impossible (fine – single account) and lost passphrase = lost history. Make it explicit in the UI.

## 6. Authorization rules

- Every query is scoped by `account_id` from the JWT. A device can only touch its own account's games/versions/blobs.
- Blob access is gated: `GET /blobs/{hash}` allowed only if the requesting account has a version referencing that hash (prevents hash-guessing exfiltration across accounts).
- Pairing codes: rate-limit generation + validation; lock after N failed attempts.

## 7. Threat model (what we defend against)

| Threat | Mitigation |
|---|---|
| Attacker on LAN sniffs traffic | TLS / WireGuard |
| Stolen device | revoke device; access JWT short TTL; keychain-stored secret |
| Brute-force pairing code | short TTL, single-use, rate limit, lockout |
| Malicious client forges versions for another account | JWT `acc` scoping + blob access gate |
| Ransomware overwrites saves | server history is append-only + retention; restore any prior version |
| Server disk failure | it's a backup target, but recommend the NAS itself has redundancy/backup; document it |

**Explicitly out of scope v1:** defending against a compromised owner account, and multi-tenant isolation beyond the single-account boundary.

## 8. Privacy

- No telemetry by default. If any diagnostics are added later, opt-in and local-first.
- Manifest fetch hits GitHub (public) – the only outbound call to a third party; document it. Everything else stays between the user's devices and their server.
