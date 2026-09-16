# Historical DMZ logind proposal — not implemented

An earlier proposal considered reducing SSH connection churn and adding a
root-owned logind cleanup guard for long-lived DMZ SSH scopes.

HMux contains no such guard, cleanup timer or installation path. This proposal
is not an operating procedure or permission to terminate sessions.

The current app reduces catalog churn through a persistent SSH producer.
Further DMZ lifecycle changes would require a separately reviewed design with
public systemd APIs, positive proof that a scope is inactive, protection of
active SSH/tmux work, isolated tests, a rollback path and approval before any
privileged service or host policy change.

See [architecture](../ARCHITECTURE.md) and [operations](../OPERATIONS.md).
