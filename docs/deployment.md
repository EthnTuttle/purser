# Purser Deployment Guide

## Raspberry Pi / Linux (systemd)

### Prerequisites

- Rust toolchain (install via [rustup](https://rustup.rs/))
- Build essentials: `sudo apt install build-essential pkg-config`

### Build from source

```bash
cargo build --release
```

The binary will be at `target/release/purser`.

### Install

Create the purser system user and group:

```bash
sudo useradd --system --no-create-home purser
```

Install the binary:

```bash
sudo cp target/release/purser /usr/local/bin/purser
```

Create configuration and data directories:

```bash
sudo mkdir -p /etc/purser /var/lib/purser
```

Copy configuration files:

```bash
sudo cp .env /etc/purser/.env
sudo cp config.toml /etc/purser/config.toml
sudo cp products.toml /etc/purser/products.toml
```

Set permissions:

```bash
sudo chown -R root:purser /etc/purser
sudo chmod 0750 /etc/purser
sudo chmod 0640 /etc/purser/.env
sudo chown -R purser:purser /var/lib/purser
sudo chmod 0750 /var/lib/purser
```

### Enable the systemd service

```bash
sudo cp deploy/purser.service /etc/systemd/system/purser.service
sudo systemctl daemon-reload
sudo systemctl enable purser
sudo systemctl start purser
```

### Check status and logs

```bash
sudo systemctl status purser
sudo journalctl -u purser -f
```

## Docker

### Build the image

From the project root:

```bash
docker build -f deploy/Dockerfile -t purser .
```

### Configure

Ensure `.env` and `config.toml` exist in the project root with your settings. See `config.toml.example` and `products.toml.example` for reference.

### Run with Docker Compose

```bash
docker compose -f deploy/docker-compose.yml up -d
```

### Manage the container

View logs:

```bash
docker compose -f deploy/docker-compose.yml logs -f
```

Restart:

```bash
docker compose -f deploy/docker-compose.yml restart
```

Stop:

```bash
docker compose -f deploy/docker-compose.yml down
```

## Troubleshooting

### Missing environment variables

If purser exits immediately on startup, check that all required variables are set in `.env`. Common symptoms:

- `"missing STRIKE_API_KEY"` or `"missing SQUARE_ACCESS_TOKEN"` -- provider credentials not set.
- `"missing NOSTR_NSEC"` -- the daemon's Nostr secret key is not configured.

Verify with:

```bash
# systemd
sudo cat /etc/purser/.env

# Docker
docker compose -f deploy/docker-compose.yml exec purser env
```

### Relay connectivity issues

If the daemon starts but does not receive or publish events:

- Confirm relay URLs in `config.toml` are reachable from the host.
- Check that `ca-certificates` is installed (Docker image includes this by default).
- Look for TLS or WebSocket errors in the logs: `journalctl -u purser | grep -i relay`

### SQLite permission errors

If you see `"unable to open database file"` or similar:

- Ensure `/var/lib/purser` is owned by the `purser` user: `ls -la /var/lib/purser`
- For Docker, confirm the `purser-data` volume is mounted and writable.

### Checking daemon health

Purser has no HTTP endpoints. Monitor health through logs:

```bash
# systemd
sudo journalctl -u purser -f

# Docker
docker compose -f deploy/docker-compose.yml logs -f purser
```

Look for periodic polling messages and successful payment status updates as indicators that the daemon is running correctly.
