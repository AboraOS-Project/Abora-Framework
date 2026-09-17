# Installer

Packaging components for a Debian/Ubuntu-style host.

## systemd unit

[`systemd/aborad.service`](systemd/aborad.service) runs `aborad` as an
unprivileged, heavily sandboxed service:

* dedicated `aborad` user/group,
* `NoNewPrivileges`, restricted (`strict`) `/`, private `/tmp` + `/dev`,
  kernel-tweak and realtime denial, no capabilities,
* logs to journald (structured JSON from stderr),
* restarts on failure.

### Manual install

```sh
# Build (see repo root; adjust the binary path as needed)
cargo build --release -p aborad
sudo install -Dm755 target/release/aborad /usr/sbin/aborad

# Dedicated user
sudo useradd --system --no-create-home --shell /usr/sbin/nologin aborad

# Config (edit first as needed)
sudo install -Dm644 ../config/abora.toml.default /etc/abora/abora.toml

# Unit
sudo install -Dm644 systemd/aborad.service /etc/systemd/system/aborad.service
sudo systemctl daemon-reload
sudo systemctl enable --now aborad
journalctl -u aborad -f
```

`StateDirectory=abora` and `RuntimeDirectory=abora` are created
automatically under `/var/lib/abora` and `/run/abora` with the right owner;
future DAEMON state (update history, tokens) belongs there.

### Verification

```sh
systemctl status aborad
curl http://127.0.0.1:7360/api/v1/health
sudo systemctl stop aborad   # graceful SIGTERM shutdown
```

## Why no full .deb/.rpm yet

Packaging (deb/rpm/arch) belongs to the broader Abora v5 packaging effort,
which spans more than the framework daemon. This directory captures the
service definition we want to ship, so the packaging work can reuse it.