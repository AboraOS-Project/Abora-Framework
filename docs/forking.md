# Starting a fork

Some files are **system files**: the ones every fork has to change (name, identity,
boot sequence). Each carries one comment line:

```
# ABORA-SYSTEM-FILE 2: rename the boot entry below to your fork's name
```

The number is the order to edit them in, so **start with number 1**.

```sh
make fork-check     # lists the files still to customize, in order
```

While any marker is left, the live ISO says so on screen when it boots, just above
"Boot finished":

```
[WARN] fork: system files not customized yet (run 'make fork-check', edit them in order):
       1. Cargo.toml:16 - set your fork's authors, description, license and version here
       ...
```

`make iso` records the list at build time, so rebuild after deleting a marker.

When you have edited a file for your fork, **delete its `ABORA-SYSTEM-FILE` line**. It then
drops off the list. In your fork's CI, `scripts/fork-check.sh --strict` exits 1 while any remain.

## Current system files

1. `Cargo.toml`: authors, description, license, version
2. `iso/limine.conf`: the boot entry name
3. `iso/init`: the live system's boot sequence

## Marking a new system file

Add one comment line, at the start of a `#` or `//` comment:

```
# ABORA-SYSTEM-FILE 4: what the fork should change here
```

Pick the next number. Files that are not tracked by git are ignored.
