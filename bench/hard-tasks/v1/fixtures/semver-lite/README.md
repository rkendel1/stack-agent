# semver-lite

Parses semantic versions (`1.2.3`, `1.0.0-alpha.1`) and requirements (`^1.2`, `~1.2.3`,
`>=1.0, <2.0`, `1.2.*`), and matches versions against them.

## Requirements

* `^1.2.3` (caret, the default for a bare version): any version that does not change the major
  version: `>=1.2.3, <2.0.0`. For `0.x` versions the caret also only fixes the major version:
  `^0.2.3` is `>=0.2.3, <1.0.0`.
* `~1.2.3` (tilde): patch updates only: `>=1.2.3, <1.3.0`.
* `1.2.*`: any patch of 1.2.
* Comparators: `>`, `>=`, `<`, `<=`, `=`, combined with commas (all must hold).

Pre-release versions sort before their release (`1.0.0-alpha < 1.0.0`), and only match a
requirement that names a pre-release of the same `major.minor.patch`.
