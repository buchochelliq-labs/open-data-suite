# Install

!!! note "Versions"
    Prebuilt binaries start with **v0.0.1**. The commands on this page use `0.1.2` as the
    example version; the [releases](https://github.com/buchochelliq-labs/open-data-suite/releases)
    list the latest.

`ods` is a single binary with no runtime dependencies. Every channel below installs the
same build, for:

| Platform | Archive target |
|---|---|
| Linux x86_64 | `x86_64-unknown-linux-musl` (static, any distribution) |
| Linux arm64 | `aarch64-unknown-linux-musl` (static, any distribution) |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple silicon | `aarch64-apple-darwin` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |

## pip, next to dbt

If you install dbt with pip, install ODS the same way, in the same environment:

```sh
pip install opendatasuite
ods version
```

The package contains only the `ods` executable (like `ruff` and `uv`), so there is no
Python import and nothing else to configure. To keep it apart from your project's
environment instead:

```sh
pipx install opendatasuite       # or: uv tool install opendatasuite
```

Wheels exist for the platforms above, and for Linux with glibc 2.17 or newer on x86_64
and arm64.

## Homebrew (macOS and Linux)

```sh
brew install buchochelliq-labs/tap/ods
ods version
```

The formula, in the [buchochelliq-labs/homebrew-tap](https://github.com/buchochelliq-labs/homebrew-tap)
tap, installs the prebuilt binary from the GitHub release for macOS and Linux, arm64
and x86_64, and checks it against the release's checksums. `brew upgrade ods` moves to
a new release once its formula is in the tap.

## Chocolatey (Windows): under review

The [`opendatasuite`](https://community.chocolatey.org/packages/opendatasuite) package
has been submitted to Chocolatey and is waiting for its moderators to approve the first
version, which can take a couple of weeks. Until it is approved, use `pip`,
`cargo binstall` or a direct download. Once it is listed:

```powershell
choco install opendatasuite
ods version
```

The package downloads the release's Windows archive, checks its sha256 and puts `ods`
on your `PATH`. `choco upgrade opendatasuite` moves to a new release once Chocolatey has
approved it, usually within a day of the release.

## cargo-binstall

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), Rust users get the
prebuilt binary instead of compiling:

```sh
cargo binstall ods-cli
```

The crates are on [crates.io](https://crates.io/crates/ods-cli) from the first release
after 0.1.2. For 0.1.2 and earlier, name the repository:
`cargo binstall --git https://github.com/buchochelliq-labs/open-data-suite ods-cli`.

## Direct download

Each [GitHub release](https://github.com/buchochelliq-labs/open-data-suite/releases)
has an archive per platform, named `ods-v<version>-<target>.tar.gz` (`.zip` on Windows).
It contains `ods`, `LICENSE`, `README.md`, `CHANGELOG.md` and `THIRD-PARTY-LICENSES.html`,
the licences of the open-source software built into `ods`.

=== "Linux"

    ```sh
    version=0.1.2 target=x86_64-unknown-linux-musl
    base=https://github.com/buchochelliq-labs/open-data-suite/releases/download/v$version
    curl -LO "$base/ods-v$version-$target.tar.gz" -LO "$base/SHA256SUMS"
    sha256sum --check --ignore-missing SHA256SUMS
    tar -xzf "ods-v$version-$target.tar.gz"
    install "ods-v$version-$target/ods" ~/.local/bin/
    ```

=== "macOS"

    ```sh
    version=0.1.2 target=aarch64-apple-darwin    # x86_64-apple-darwin on Intel
    base=https://github.com/buchochelliq-labs/open-data-suite/releases/download/v$version
    curl -LO "$base/ods-v$version-$target.tar.gz" -LO "$base/SHA256SUMS"
    shasum -a 256 --check --ignore-missing SHA256SUMS
    tar -xzf "ods-v$version-$target.tar.gz"
    install "ods-v$version-$target/ods" /usr/local/bin/
    ```

=== "Windows (PowerShell)"

    ```powershell
    $version = "0.1.2"; $name = "ods-v$version-x86_64-pc-windows-msvc"
    $base = "https://github.com/buchochelliq-labs/open-data-suite/releases/download/v$version"
    Invoke-WebRequest "$base/$name.zip" -OutFile "$name.zip"
    Invoke-WebRequest "$base/SHA256SUMS" -OutFile SHA256SUMS
    $expected = (Select-String -Path SHA256SUMS -Pattern "  $name.zip$").Line.Split(" ")[0]
    if ((Get-FileHash "$name.zip" -Algorithm SHA256).Hash -ne $expected) { throw "checksum mismatch" }
    Expand-Archive "$name.zip" -DestinationPath .
    .\$name\ods.exe version
    ```

    Then move `ods.exe` to a folder on your `PATH`.

### Verify where a file came from

Every archive, `SHA256SUMS` and `ods.rb` on a GitHub release has a
[build provenance attestation](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations):
a signed statement that the release workflow built it from the tagged commit. (Releases
up to 0.1.2 also attached the wheels; from the next one, wheels are on PyPI only, with
PyPI's own attestations.) With the [GitHub CLI](https://cli.github.com):

```sh
gh attestation verify ods-v0.1.2-x86_64-unknown-linux-musl.tar.gz \
  --repo buchochelliq-labs/open-data-suite
```

## From source

With Rust 1.90 or newer ([rustup.rs](https://rustup.rs)):

```sh
cargo install --locked ods-cli
```

Or the latest `main`:
`cargo install --locked --git https://github.com/buchochelliq-labs/open-data-suite ods-cli`.

## Versions and upgrades

`ods version` prints the ODS version and the versions of the interfaces other tools rely
on (the plugin SDK and the JSON output). Before 1.0, a minor release may include breaking
changes and migrate the state store, so read the changelog before upgrading; a patch
release (0.1.1 → 0.1.2) never does. In the 0.0.x series every release could, even
though 0.0.1 → 0.0.2 looks like a patch. Each break is listed under **Breaking** in the
[changelog](https://github.com/buchochelliq-labs/open-data-suite/blob/main/CHANGELOG.md),
with what to do. The release notes of each version are its changelog section.
[ADR-0019](adr/0019-release-and-versioning.md) has the full policy.

## For maintainers

Releases use [release-kit](https://github.com/buchochelliq-labs/release-kit) (ADR-0019):
`release-kit.yml` lists every channel, `.github/release-kit/` is the installed kit, and
`.github/workflows/release.yml` runs it. Everything is released together at the
workspace version.

1. **The release PR** bumps `workspace.package.version` and the internal crates'
   `version = "X.Y.Z"` requirements in `Cargo.toml` (ADR-0035), moves `[Unreleased]` in
   `CHANGELOG.md` under the new version, and runs
   `python3 .github/release-kit/scripts/release.py sync` to copy the version into
   `release-kit.yml`. CI's `release.py check` fails until they agree.
2. **Once it is merged**, on an up-to-date `main`:
   `python3 .github/release-kit/scripts/release.py tag`. It pushes an annotated record
   tag per package that is due (`ods-core-vX.Y.Z`, …, `vX.Y.Z` for the GitHub release,
   `opendatasuite-vX.Y.Z` for PyPI), then `release-X.Y.Z`, which starts the release
   run, and waits for it. `release.py status` shows what is published.
3. **The release run**:
    1. checks the tag once and runs CI (`ci.yml`) on the tagged commit;
    2. builds the archives (static musl on Linux) and the wheels, each with the licence
       notices (`scripts/release-notices.sh`), and tests each where its runner can run
       it (`scripts/release-test-archive.sh`, `scripts/release-test-wheel.sh`);
    3. publishes the crates to crates.io, in dependency order (ADR-0035);
    4. creates the GitHub release on `vX.Y.Z`: the archives, `SHA256SUMS` and build
       provenance attestations, with the version's changelog section as notes
       (`scripts/release-notes.sh`; the run fails if the section is missing);
    5. publishes the wheels to PyPI, then installs the version from PyPI;
    6. renders the Homebrew formula from `SHA256SUMS`, attests it, attaches `ods.rb` to
       the release and opens a pull request on the tap (not for prereleases);
    7. packs the Chocolatey package, installs it on Windows, and pushes it
       (`.github/workflows/chocolatey.yml`).
4. **A failed release** is finished by running `release.py tag` again: it publishes
   only what is still due. See release-kit's
   [recovery guide](https://github.com/buchochelliq-labs/release-kit/blob/main/docs/recovery.md).

Pull requests that change packaging files run the `Packaging` workflow, which runs the
same notices, wheel test and notes scripts, and the formula and Chocolatey templates.

One-time setup, by a repository, crates.io and PyPI admin. Every environment below must
allow `release-*` tags, which is what starts the run:

- **crates.io.** Create the `crates-io` environment with a `CARGO_REGISTRY_TOKEN`
  secret: a crates.io API token with the `publish-new` and `publish-update` scopes. It
  publishes each crate's first version (crates.io offers Trusted Publishing only for a
  crate that exists). crates.io allows a burst of new crates and then one every few
  minutes, so the first release of all 18 crates takes about two hours; release-kit
  waits it out. After that release, add a
  [Trusted Publisher](https://crates.io/docs/trusted-publishing) to each crate
  (repository `buchochelliq-labs/open-data-suite`, workflow `release.yml`, environment
  `crates-io`), and the token is only used for new crates.
- **PyPI.** The `opendatasuite` project's
  [trusted publisher](https://docs.pypi.org/trusted-publishers/adding-a-publisher/):
  owner `buchochelliq-labs`, repository `open-data-suite`, workflow `release.yml`,
  environment `pypi`. No API token is stored.
- **The `pypi` environment**, with required reviewers so each upload waits for approval.
- **Tag protection.** A ruleset for `release-*` and `v*` tags (and the crates' record
  tags, `ods-*-v*` and `opendatasuite-v*`) so only maintainers can create them.
- **Homebrew tap.** The public repository `buchochelliq-labs/homebrew-tap` holds the
  formula. The release opens a pull request there with the new `Formula/ods.rb`;
  merging it publishes the release to `brew upgrade`. For that, create a fine-grained
  personal access token with access to `buchochelliq-labs/homebrew-tap` only and the
  **Contents** and **Pull requests** permissions set to read and write. Then create
  the `homebrew` environment with the token as its `HOMEBREW_TAP_TOKEN` secret. Without
  the secret the release's `publish-homebrew` job only prints a notice, and you copy the
  release's `ods.rb` asset to `Formula/ods.rb` in the tap by hand. When the token
  expires, the job fails: renew it and re-run the job.
- **Chocolatey.** Create the `chocolatey` environment with a `CHOCOLATEY_API_KEY`
  secret (the API key of the chocolatey.org account that owns `opendatasuite`) and
  required reviewers, like `pypi`. To push an existing release by hand (**Actions →
  Chocolatey → Run workflow**, `publish` on), the environment must also allow the
  branch the workflow runs from. Each push goes through Chocolatey's automated checks,
  and the package's first version through human moderation, before it is listed. While
  a version is still in moderation, Chocolatey refuses newer ones with
  **403 Forbidden**, so the release's `publish-chocolatey / push` job fails until the
  earlier version is approved. Then push the newer release by hand as above (v0.0.2
  waited this way for v0.0.1).
- **Checks.** The `Packaging` workflow runs only on pull requests that change packaging
  files, so don't make its jobs required checks: a required check that never starts
  blocks every other pull request. Review its result on the pull requests where it runs,
  or drop its `paths` filter first if it should be required.
