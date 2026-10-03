#!/bin/bash
# Builds the release binary and the RPM (EL10 / Fedora, x86_64) into target/rpm/. Run from anywhere.
# Needs rpm-build besides the build dependencies in the README.
set -euo pipefail
cd "$(dirname "$0")/.."
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK"/{SOURCES,SPECS,BUILD,RPMS,SRPMS,BUILDROOT} "$WORK/maildot-$VER"
cp target/release/maildot packaging/io.github.th3rumbl3m4t0r.jmaildot.{desktop,svg,metainfo.xml} "$WORK/maildot-$VER/"
tar -C "$WORK" -czf "$WORK/SOURCES/maildot-$VER.tar.gz" "maildot-$VER"
rpmbuild -bb --define "_topdir $WORK" --define "ver $VER" packaging/maildot.spec
mkdir -p target/rpm
install -m 644 "$WORK"/RPMS/x86_64/maildot-"$VER"-*.rpm target/rpm/
ls -l target/rpm/maildot-"$VER"-*.rpm
