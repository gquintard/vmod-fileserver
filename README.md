# vmod_fileserver

Serve files directly from Varnish, no external backend needed!

As usual for vmods, the full API is in [vmod.vcc](vmod.vcc).

## Version matching

| vmod-fileserver | varnish |
|:----------------|:-------:|
| 0.0.8           |   7.7   |
| 0.0.7           |   7.6   |
| 0.0.6           |   7.5   |
| 0.0.5           |   7.4   |
| 0.0.3 -> 0.0.4  |   7.3   |
| 0.0.1 -> 0.0.2  |   7.2   |

## VCL Examples

``` vcl
import fileserver;

backend default none;

sub vcl_init {
	new www = fileserver.root("/var/www/html");
}

sub vcl_recv {
	set req.backend_hint = www.backend();
}
```

## Requirements

You'll need:
- `cargo` (and the accompanying `rust` package)
- `clang`
- `python3`
- the `varnish` 7.3 development libraries/headers ([depends on the `varnish` crate you are using](https://github.com/gquintard/varnish-rs#versions))

## Build and test

With `cargo` only:

``` bash
cargo build --release
cargo test --release
```

The vmod file will be found at `target/release/libvmod_fileserver.so`.

Alternatively, if you have `jq` and `rst2man`, you can use `build.sh`

``` bash
./build.sh [OUTDIR]
```

This will place the `so` file as well as the generated documentation in the `OUT` directory (or in the current directory if `OUT` wasn't specified).

## Packages

To avoid making a mess of your system, you probably should install your vmod as a proper package. This repository also offers different templates, and some quick recipes for different distributions.

### All platforms

First it's necessary to set the `VMOD_VERSION` (the version of this vmod) and `VARNISH_VERSION` (the Varnish version to build against) environment variables. It can be done manually, or using `cargo` and `jq`:
``` bash
VMOD_VERSION=$(cargo metadata --no-deps --format-version 1 | jq '.packages[0].version' -r)
VARNISH_MINOR=$(cargo metadata --format-version 1 | jq -r '.packages[] | select(.name == "varnish-sys") | .metadata.libvarnishapi.version ')
VARNISH_PATCH=0
VARNISH_VERSION="$VARNISH_MINOR.$VARNISH_PATCH"

# or
VMOD_VERSION=0.0.1
VARNISH_VERSION=7.0.0
```

Then create the dist tarball, for example using `git archive`:

``` bash
git archive --output=vmod_fileserver-$VMOD_VERSION.tar.gz --format=tar.gz HEAD
```

Then, follow distribution-specific instructions.

### Arch

``` bash
# create a work directory
mkdir build
# copy the tarball and PKGBUILD file, substituing the variables we care about
cp vmod_fileserver-$VMOD_VERSION.tar.gz build
sed -e "s/@VMOD_VERSION@/$VMOD_VERSION/" -e "s/@VARNISH_VERSION@/$VARNISH_VERSION/" pkg/arch/PKGBUILD > build/PKGBUILD

# build
cd build
makepkg -rsf
```

Your package will be the file with the `.pkg.tar.zst` extension in `build/`

### Alpine

Alpine needs a bit of setup on the first time, but the [documentation](https://wiki.alpinelinux.org/wiki/Creating_an_Alpine_package) is excellent.

``` bash
# install some packages, create a user, give it power and a key
apk add -q --no-progress --update tar alpine-sdk sudo
adduser -D builder
echo "builder ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers
addgroup builder abuild
su builder -c "abuild-keygen -nai"
```

Then, to actually build your package:

``` bash
# create a work directory
mkdir build
# copy the tarball and PKGBUIL file, substituing the variables we care about
cp vmod_fileserver-$VMOD_VERSION.tar.gz build
sed -e "s/@VMOD_VERSION@/$VMOD_VERSION/" -e "s/@VARNISH_VERSION@/$VARNISH_VERSION/" pkg/arch/APKBUILD > build/APKBUILD

su builder -c "abuild checksum"
su builder -c "abuild -r"
```
