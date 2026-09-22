# Rollback

Keep private state separate from immutable web release directories. Record the
active gateway release, Home binary and previous versions before each upgrade.

For a frontend rollback, atomically point the gateway's asset symlink at the
previous verified release and reload the browser/PWA. For gateway binary changes,
restore its prior release/service path and restart only the HMux gateway. Verify
HTTPS, CSP, assets, unauthenticated API rejection and authenticated Home connectivity.

For a Home connector rollback, stop the current connector and explicitly start
the verified previous binary with the same private config/token. Closing its
browser views must not kill original tmux/provider work. Never use `kill-server`
or detach/rename existing user sessions as part of deployment or tests.

Do not restore old credential/login files as an ordinary binary rollback: doing
so may revive revoked logins or an older TOTP policy. Preserve current account,
profile, push and usage state. Compare timestamped config backups before restoring
individual settings; never overwrite unrelated newer user changes.

Recovery checkpoint repair is a separate administrative operation; consult
[RECOVERY.md](RECOVERY.md). Restoring an arbitrary old checkpoint can restart work
that was deliberately closed and is not a routine deployment rollback.
