# CC1101 Upstream Patch

This repository carries a temporary local override of `cc1101` via:

- `firmware/Cargo.toml` (git dependency source)
- `Cargo.toml` (`[patch."https://github.com/dsvensson/cc1101.git"]` to `patches/cc1101`)

To keep the change upstreamable, the exact delta is tracked as:

- `patches/cc1101/upstream/0001-add-absolute-carrier-sense-threshold-setter.patch`

## Base Upstream Revision

The patch is based on upstream commit:

- `11a24c471188493d6815d3ea35f27979ef405f55`

## Apply Upstream

```bash
git clone https://github.com/dsvensson/cc1101.git
cd cc1101
git checkout 11a24c471188493d6815d3ea35f27979ef405f55
git am /path/to/motonet-of-things/patches/cc1101/upstream/0001-add-absolute-carrier-sense-threshold-setter.patch
```

If upstream moves on, rebase this patch on top of the new `main` tip before opening a PR.
