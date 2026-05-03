# Relay Peer Trust Boundary Design

**Date:** 2026-04-29
**Status:** Draft for review
**Scope:** Define the trust boundary for relay mesh peer-control endpoints after the relay ownership hardening work.

## Problem

The relay mesh has peer-control endpoints under `/_peer/*` for join, heartbeat,
route announce, route withdraw, leave, digest, and full sync. Recent ownership
hardening made these messages stricter:

- peer-originated route changes must come from a known peer;
- message owner fields must match the claimed sender relay id;
- peer snapshots must not leak local-only IPC addresses or server ids;
- peer route withdraws must not delete local routes.

Those checks are necessary for correctness, but they are not identity
authentication. A request body that contains `sender_relay_id = relay-a` is only
a claim. If the relay HTTP port is reachable by an untrusted caller, that caller
can forge the field unless an external or cryptographic identity mechanism
binds the request to the relay id.

## Decision

C-Two should not treat request JSON fields as the root of peer identity. For the
current 0.x design, the relay mesh security contract is:

1. C-Two owns route-table correctness checks inside the relay process.
2. The deployment environment owns peer endpoint reachability, transport
   confidentiality, and node authentication.
3. The relay peer API remains an internal control-plane API. It is not a public
   internet-facing API.
4. IP allowlists are not a sufficient identity model. They can be used as a
   deployment-layer defense, but they must not be the basis for accepting a
   message as a specific relay.

In Kubernetes or similar environments, production deployments should place
`/_peer/*` behind cluster controls such as NetworkPolicy, private Services, mTLS
service mesh, SPIFFE/SPIRE workload identity, or equivalent infrastructure.
C-Two should document that requirement instead of implementing a partial cluster
security model in the relay protocol.

## Rationale

### Why Not IP Identity

IP addresses are routing metadata, not stable workload identity:

- Pods and containers are rescheduled and receive new IP addresses.
- NAT, sidecars, proxies, and load balancers can obscure the real source.
- A source IP can prove network location at best, not relay ownership.
- Multi-cluster and service-mesh deployments often decouple workload identity
  from IP addressing.

An IP allowlist can still reduce accidental exposure, but it does not prove
that a request was sent by the relay named in the body.

### Why Not Full Security Inside C-Two Now

A complete peer-auth system would need key distribution, bootstrap trust,
rotation, revocation, replay protection, and operational guidance. That overlaps
with what Kubernetes, mTLS service meshes, SPIFFE/SPIRE, Consul, etcd, WireGuard,
and similar infrastructure already provide.

Duplicating that too early would add protocol and configuration complexity
before C-Two has production deployment evidence that it needs to own this layer.

### What C-Two Still Must Enforce

C-Two should continue to enforce local invariants that are independent of the
deployment security layer:

- a peer message that claims `sender_relay_id` must be internally consistent;
- a known peer may only mutate routes owned by that peer;
- peer sync snapshots must not import impossible ownership state;
- peer route withdraw or leave must not delete local routes;
- peer snapshots must not carry local IPC-only fields such as server id or IPC
  address.

These checks prevent accidental or malformed peer messages from corrupting local
state. They should not be described as authentication.

## Near-Term Implementation Guidance

The current relay code should be shaped around this distinction:

- Keep strict message consistency checks.
- Keep known-peer membership checks.
- Keep local route protection.
- Avoid comments, function names, or docs that imply `sender_relay_id` is an
  authenticated identity by itself.
- Document `/_peer/*` as internal-only and deployment-protected.
- Do not add IP allowlist logic as the primary security mechanism.

If a standalone or Docker Compose deployment later needs a C-Two-owned minimum
protection layer, add a separate design for a mesh shared secret with HMAC:

- sign method, path, timestamp, sender relay id, and body hash;
- reject stale timestamps;
- compare signatures in constant time;
- require the sender to be a known peer after signature validation;
- keep this positioned as lightweight mesh protection, not as a replacement for
  mTLS or workload identity in production clusters.

That HMAC layer is intentionally out of scope for this design.

## Documentation Updates Needed

Relay mesh documentation should say:

- `/_peer/*` endpoints are internal control-plane endpoints.
- Operators must restrict peer endpoint reachability to trusted relay nodes.
- Production deployments should rely on cluster or service-mesh identity for
  peer authentication.
- C-Two validates peer message consistency and route ownership, but does not
  authenticate relay identity unless a future mesh-auth feature is explicitly
  enabled.

## Test And Review Expectations

Code changes that touch peer handlers or route-table merge behavior should be
reviewed with these checks:

- Can an untrusted caller forge only a JSON field and mutate local routes?
- Can a peer remove or overwrite local routes?
- Can a malformed snapshot erase valid routes before validation completes?
- Does any test or comment imply body fields are authenticated identities?
- Are local-only fields scrubbed before peer snapshots leave the process?

This spec does not require new runtime behavior by itself. It defines the
security boundary that future relay peer changes must preserve.
