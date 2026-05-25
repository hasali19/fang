build:
    cargo build --bin fangd --release

install: build
    sudo systemctl stop fangd || true
    sudo cp $(pwd)/target/release/fangd /usr/local/bin/
    sudo mkdir -p /etc/dbus-1/system.d
    sudo mkdir -p /etc/dbus-1/system-services
    sudo cp $(pwd)/dist/dev.hasali.Fang.conf /etc/dbus-1/system.d/
    sudo cp $(pwd)/dist/fangd.service /etc/systemd/system/
    sudo systemctl daemon-reload
