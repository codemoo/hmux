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

If Home runs as a native user service, run `hmux-web service stop` before restoring
its binary and definition backups. Restart through `service start` only when the
restored binary supports the definition's flags. Releases predating user services
do not support `--log-file` or `service`; uninstall the service using the newer
binary first and run the older `connect` command in the foreground with the same
private token/config. A service definition must not repeatedly restart an
incompatible binary. See [service controls](OPERATIONS.md#automatic-home-startup-macos-and-linux).

Do not restore old credential/login files as an ordinary binary rollback: doing
so may revive revoked logins or an older TOTP policy. Preserve current account,
profile, push and usage state. Compare timestamped config backups before restoring
individual settings; never overwrite unrelated newer user changes.

Recovery checkpoint repair is a separate administrative operation; consult
[RECOVERY.md](RECOVERY.md). Restoring an arbitrary old checkpoint can restart work
that was deliberately closed and is not a routine deployment rollback.
