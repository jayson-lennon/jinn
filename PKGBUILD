# Maintainer: Jayson Lennon <jayson@jaysonlennon.dev>

pkgname=nullslop
pkgver=0.16.0
pkgrel=1
pkgdesc='Agentic LLM agent harness'
url='https://github.com/jayson-lennon/nullslop'
license=(AGPL-3.0)
makedepends=('cargo' 'clang' 'upx')
depends=('sqlite' 'gcc-libs')
arch=('x86_64')

# Build from local checkout. Run makepkg from the project root directory.
# No source array — we reference $startdir directly.
options=(!debug)
source=()

prepare() {
    ln -sf "$startdir" "$srcdir/$pkgname-$pkgver"
    cd "$srcdir/$pkgname-$pkgver"
    export RUSTUP_TOOLCHAIN=stable
    cargo generate-lockfile
    cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
    cd "$srcdir/$pkgname-$pkgver"
    export RUSTUP_TOOLCHAIN=stable
    CFLAGS+=" -ffat-lto-objects" cargo build --frozen --release --all-features
    upx -9 target/release/nullslop
}

package() {
    cd "$srcdir/$pkgname-$pkgver"

    # Install binary.
    install -Dm0755 target/release/nullslop -t "$pkgdir/usr/bin/"

    # Install default themes, personas, and prompts to /usr/share/nullslop/.
    install -Dm0644 -t "$pkgdir/usr/share/nullslop/themes/" themes/*.toml
    install -Dm0644 -t "$pkgdir/usr/share/nullslop/personas/" personas/*.md
    install -Dm0644 -t "$pkgdir/usr/share/nullslop/prompts/" prompts/*.md

    # Install shell completions.
    local _bin="target/release/nullslop"
    install -Dm0644 /dev/stdin "$pkgdir/usr/share/bash-completion/completions/nullslop" \
        < <("$_bin" completions bash)
    install -Dm0644 /dev/stdin "$pkgdir/usr/share/zsh/site-functions/_nullslop" \
        < <("$_bin" completions zsh)
    install -Dm0644 /dev/stdin "$pkgdir/usr/share/fish/vendor_completions.d/nullslop.fish" \
        < <("$_bin" completions fish)
}
