# Install

!!! warning "From the first release"
    Prebuilt binaries start with **v0.1.0**, which isn't released yet. Until then, build
    from source (below). The commands on this page use `0.1.0` as the example version.

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

## Homebrew (macOS and Linux): coming soon

A Homebrew tap isn't available yet. Until it is, use `pip`, `cargo binstall` or a
direct download.

## cargo-binstall

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), Rust users get the
prebuilt binary instead of compiling:

```sh
cargo binstall --git https://github.com/buchochelliq-labs/open-data-suite ods-cli
```

## Direct download

Each [GitHub release](https://github.com/buchochelliq-labs/open-data-suite/releases)
has an archive per platform, named `ods-v<version>-<target>.tar.gz` (`.zip` on Windows).
It contains `ods`, `LICENSE`, `README.md`, `CHANGELOG.md` and `THIRD-PARTY-LICENSES.html`,
the licences of the open-source software built into `ods`.

=== "Linux"

    ```sh
    version=0.1.0 target=x86_64-unknown-linux-musl
    base=https://github.com/buchochelliq-labs/open-data-suite/releases/download/v$version
    curl -LO "$base/ods-v$version-$target.tar.gz" -LO "$base/SHA256SUMS"
    sha256sum --check --ignore-missing SHA256SUMS
    tar -xzf "ods-v$version-$target.tar.gz"
    install "ods-v$version-$target/ods" ~/.local/bin/
    ```

=== "macOS"

    ```sh
    version=0.1.0 target=aarch64-apple-darwin    # x86_64-apple-darwin on Intel
    base=https://github.com/buchochelliq-labs/open-data-suite/releases/download/v$version
    curl -LO "$base/ods-v$version-$target.tar.gz" -LO "$base/SHA256SUMS"
    shasum -a 256 --check --ignore-missing SHA256SUMS
    tar -xzf "ods-v$version-$target.tar.gz"
    install "ods-v$version-$target/ods" /usr/local/bin/
    ```

=== "Windows (PowerShell)"

    ```powershell
    $version = "0.1.0"; $name = "ods-v$version-x86_64-pc-windows-msvc"
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

Every archive, wheel and `SHA256SUMS` has a
[build provenance attestation](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations):
a signed statement that the release workflow built it from the tagged commit. With the
[GitHub CLI](https://cli.github.com):

```sh
gh attestation verify ods-v0.1.0-x86_64-unknown-linux-musl.tar.gz \
  --repo buchochelliq-labs/open-data-suite
```

## From source

With Rust 1.90 or newer ([rustup.rs](https://rustup.rs)):

```sh
cargo install --locked --git https://github.com/buchochelliq-labs/open-data-suite ods-cli
```

## Versions and upgrades

`ods version` prints the ODS version and the versions of the interfaces other tools rely
on (the plugin SDK and the JSON output). Before 1.0, a minor release may include breaking
changes; each one is listed under **Breaking** in the
[changelog](https://github.com/buchochelliq-labs/open-data-suite/blob/main/CHANGELOG.md),
with what to do. The release notes of each version are its changelog section.
[ADR-0019](adr/0019-release-and-versioning.md) has the full policy.

## For maintainers

A release is a `vX.Y.Z` tag on `main`, pushed after the release PR has bumped
`workspace.package.version` and moved `[Unreleased]` in `CHANGELOG.md` under the new
version (ADR-0019). The tag starts `.github/workflows/release.yml`, which:

1. checks that the tag matches the workspace version and is on `main`, and takes the
   release notes from the version's changelog section (`scripts/changelog-section.py`);
   the run fails if the section is missing;
2. builds each target once with `maturin`, producing both the wheel and the archive;
3. installs the wheels with pip and runs `ods version` on Linux, macOS and Windows;
4. writes `SHA256SUMS`, renders the Homebrew formula `ods.rb`, attests provenance, and
   creates the GitHub release;
5. publishes the wheels to PyPI.

Run the workflow by hand (**Actions → Release → Run workflow**) for a dry run: it builds
and tests everything, uploads the result as workflow artifacts, and publishes nothing.

One-time setup, by a repository and PyPI admin:

- **PyPI.** Create the `opendatasuite` project's
  [trusted publisher](https://docs.pypi.org/trusted-publishers/adding-a-publisher/)
  (a "pending publisher" before the first upload): owner `buchochelliq-labs`, repository
  `open-data-suite`, workflow `release.yml`, environment `pypi`. No API token is stored.
- **The `pypi` environment.** Create it under **Settings → Environments**, restrict it
  to `v*` tags, and add required reviewers so each upload waits for approval.
- **Tag protection.** Add a ruleset for `v*` tags so only maintainers can create them.
- **Homebrew tap.** Create the repository `buchochelliq-labs/homebrew-tap`. After each
  release, copy the release's `ods.rb` asset to `Formula/ods.rb` there and commit it.
  Automating this needs a token with write access to the tap, stored as a secret; it
  isn't set up yet.
- **Checks.** The `Packaging` workflow runs only on pull requests that change packaging
  files, so don't make its jobs required checks: a required check that never starts
  blocks every other pull request. Review its result on the pull requests where it runs,
  or drop its `paths` filter first if it should be required.
